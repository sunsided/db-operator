use crate::controllers::database_event_recorder::{DatabaseEventRecorder, EventAction, EventReason};
use crate::controllers::grants::{Grants, ToSql};
use crate::{Context, Error};
use chrono::Utc;
use kube::{
    api::{Api, Patch, PatchParams, ResourceExt},
    runtime::{
        controller::Action,
        finalizer::{finalizer, Event as Finalizer},
    },
    CustomResource,
};
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Pool, Postgres};
use std::sync::Arc;
use tokio::time::Duration;
use tracing::*;

pub static DATABASE_FINALIZER: &str = "database.db-operator.widemeadows.de";

/// Generate the Kubernetes wrapper struct `Database` from our Spec and Status struct
///
/// This provides a hook for generating the CRD yaml (in crdgen.rs)
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "Database",
    group = "db-operator.widemeadows.de",
    version = "v1",
    singular = "database",
    plural = "databases",
    namespaced
)]
#[kube(status = "DatabaseStatus", shortname = "db")]
#[serde(rename_all = "camelCase")]
pub struct DatabaseServerSpec {
    /// A reference to the `DatabaseServer` instance to use.
    pub server_ref: DatabaseServerRef,
    /// The name of the database to create.
    pub name: String,
    /// The optional database comment.
    pub comment: Option<String>,
    /// Whether to create the database when reconciling the resource.
    pub create: Option<bool>,
    /// Whether to delete the database when removing the resource.
    pub delete: bool,
    /// User grants to apply.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub grants: Vec<Grants>,
}

/// A reference to a `DatabaseServer`
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub struct DatabaseServerRef {
    /// A reference to the `DatabaseServer` instance.
    pub name: String,
}

/// The status object of `Database`
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub struct DatabaseStatus {
    pub processed: bool,
    pub created: bool,
}

impl Database {
    fn was_created(&self) -> bool {
        self.status.as_ref().map(|s| s.created).unwrap_or(false)
    }
}

#[instrument(skip(ctx, database), fields(trace_id))]
pub async fn reconcile(database: Arc<Database>, ctx: Arc<Context>) -> crate::Result<Action> {
    let trace_id = crate::telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.count_and_measure();
    ctx.diagnostics.write().await.last_event = Utc::now();
    let ns = database.namespace().unwrap(); // doc is namespace scoped
    let dbs: Api<Database> = Api::namespaced(ctx.client.clone(), &ns);

    info!("Reconciling Database \"{}\" in {}", database.name_any(), ns);
    finalizer(&dbs, DATABASE_FINALIZER, database, |event| async {
        match event {
            Finalizer::Apply(db) => db.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(db) => db.cleanup(ctx.clone()).await,
        }
    })
        .await
        .map_err(|e| Error::FinalizerError(Box::new(e)))
}

pub fn error_policy(doc: Arc<Database>, error: &Error, ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    ctx.metrics.reconcile_failure_db(&doc, error);
    Action::requeue(Duration::from_secs(5 * 60))
}

impl Database {
    // Reconcile (for non-finalizer related changes)
    async fn reconcile(&self, ctx: Arc<Context>) -> crate::Result<Action> {
        let client = ctx.client.clone();
        let recorder = ctx.diagnostics.read().await.recorder_db(client.clone(), self);
        let ns = self.namespace().unwrap();
        let name = self.name_any();
        let dbs: Api<Database> = Api::namespaced(client.clone(), &ns);

        let servers = ctx.servers.read().await;
        let server_id = &self.spec.server_ref.name;
        let pool = match servers.get(server_id) {
            None => {
                warn!("Unable to look up database server {}", server_id);
                recorder.warn(EventAction::CreateDatabase, EventReason::MissingServer, format!(
                    "Unable to create database for {name} due to unresolved server reference: {}",
                    self.spec.server_ref.name
                ))
                    .await?;

                return Ok(Action::requeue(Duration::from_secs(5 * 60)));
            }
            Some(pool) => pool,
        };

        let exists =
            match sqlx::query_scalar::<Postgres, i32>("SELECT 1 FROM pg_database WHERE datname = $1;")
                .bind(&self.spec.name)
                .fetch_optional(pool)
                .await
            {
                Ok(Some(_value)) => true,
                Ok(None) => false,
                Err(err) => {
                    recorder.warn(EventAction::CreateDatabase, EventReason::InspectionFailed, format!(
                        "Unable to inspect databases for {name}: {err}"
                    ))
                        .await?;
                    return Ok(Action::requeue(Duration::from_secs(5 * 60)));
                }
            };

        let database_name = match sanitize_db_name(&self.spec.name) {
            Ok(name) => name,
            Err(err) => {
                recorder.warn(EventAction::CreateDatabase, EventReason::InvalidName, format!(
                    "Invalid database name for {name}: {err}"))
                    .await?;

                return Ok(Action::requeue(Duration::from_secs(5 * 60)));
            }
        };

        let mut created = false;
        if let Some(status) = &self.status {
            if status.processed && exists {
                created = true;
            } else {
                let should_create = self.spec.create.unwrap_or(true);
                if !self.was_created() && should_create {
                    info!("Attempting to create database {}", self.spec.name);
                    created = self.create_database(&recorder, pool, &database_name).await?;
                }
            }
        }

        // Update the database comment.
        if let Some(comment) = &self.spec.comment {
            if comment.contains("--") || comment.contains("/*") || comment.contains("*/") || comment.chars().any(|c| !c.is_ascii() || c.is_ascii_control()) {
                warn!("Comment contains illegal characters for {name}");
                recorder.warn(EventAction::ApplyComment, EventReason::Ignored, format!(
                    "Comment for database {name} contains illegal characters - skipping"))
                    .await?;
            } else {
                let sanitized = comment.replace("'", "''").replace(r#"\"#, r#"\\"#);
                match sqlx::query::<Postgres>(&format!("COMMENT ON DATABASE \"{database_name}\" IS '{sanitized}';"))
                    .bind(comment)
                    .execute(pool)
                    .await
                {
                    Ok(_result) => {
                        debug!("Set comment on database {name}");
                    }
                    Err(err) => {
                        warn!("Failed to set comment on database {database_name}: {err}");
                        recorder.warn(EventAction::ApplyComment, EventReason::Failed, format!(
                            "Failed to apply comment to database {name}: {err}"))
                            .await?;
                    }
                }
            }
        }

        // Go through all grants.
        for user_grant in &self.spec.grants {
            let user_name = match sanitize_user_name(&user_grant.user) {
                Ok(name) => name,
                Err(err) => {
                    recorder.warn(EventAction::ApplyGrant, EventReason::InvalidName, format!(
                        "Invalid user name for {name}: {err}"))
                        .await?;

                    return Ok(Action::requeue(Duration::from_secs(5 * 60)));
                }
            };

            if let Some(db_grant) = &user_grant.database {
                for privilege in &db_grant.grants {
                    let privilege = privilege.to_sql();
                    match sqlx::query::<Postgres>(&format!(
                        "GRANT {privilege} ON DATABASE \"{database_name}\" TO \"{user_name}\";"
                    ))
                        .execute(pool)
                        .await
                    {
                        Ok(_result) => {
                            debug!(
                                "Set privilege {privilege} on {database_name} to {}",
                                user_grant.user
                            );
                            recorder.info(EventAction::ApplyGrant, EventReason::Success, format!(
                                "Applied privilege {privilege} on database {database_name} to {}",
                                user_grant.user
                            ))
                                .await?;
                        }
                        Err(err) => {
                            warn!("Failed to apply grant on database {database_name}: {err}");
                            recorder.warn(EventAction::ApplyGrant, EventReason::Failed, format!("Failed to apply privilege {privilege} on database {database_name} to {}: {err}", user_grant.user))
                                .await?;
                        }
                    }
                }
            }

            // TODO: To correctly apply the schema permissions, we'd have to switch to this database.
            //       This assumes that our user has permissions to connect to the newly-created database.
            //       We could try to temporarily give us the necessary permission, but it might still be forbidden.
            if let Some(schema_grant) = &user_grant.schema {
                for privilege in &schema_grant.grants {
                    let privilege = privilege.to_sql();
                    info!(
                        "The following operation is currently not supported: GRANT {privilege} ON SCHEMA \"{database_name}\" TO {};", user_grant.user
                    );
                }
            }

            // TODO: To correctly apply the table permissions, we'd have to switch to this database.
            //       This assumes that our user has permissions to connect to the newly-created database.
            //       We could try to temporarily give us the necessary permission, but it might still be forbidden.
            if let Some(table_grant) = &user_grant.table {
                for privilege in &table_grant.grants {
                    let privilege = privilege.to_sql();
                    let schema = table_grant.schema.to_owned().unwrap_or(String::from("public"));
                    info!(
                        "The following operation is currently not supported: GRANT {privilege} ON TABLE {schema}.{database_name} TO {};", user_grant.user
                    );
                }
            }
        }

        // always overwrite status object with what we saw
        let new_status = Patch::Apply(json!({
            "apiVersion": "db-operator.widemeadows.de/v1",
            "kind": "Database",
            "status": DatabaseStatus {
                processed: true,
                created
            }
        }));
        let ps = PatchParams::apply("cntrlr").force();
        let _o = dbs
            .patch_status(&name, &ps, &new_status)
            .await
            .map_err(Error::KubeError)?;

        // If no events were received, check back every 5 minutes
        Ok(Action::requeue(Duration::from_secs(5 * 60)))
    }

    async fn create_database(
        &self,
        recorder: &DatabaseEventRecorder,
        pool: &Pool<Postgres>,
        database_name: &String,
    ) -> Result<bool, Error> {
        Ok(
            match sqlx::query::<Postgres>(&format!("CREATE DATABASE \"{database_name}\";"))
                .execute(pool)
                .await
            {
                Ok(_result) => {
                    recorder.info(EventAction::CreateDatabase, EventReason::Success, format!("Successfully created database for {}", self.name_any()))
                        .await?;
                    true
                }
                Err(err) => {
                    // TODO: Find a less hacky solution.
                    if err.to_string().contains("already exists") {
                        info!("Database already existed for {}", self.name_any());
                        recorder.info(EventAction::CreateDatabase, EventReason::Success, format!("Database for {} already existed", self.name_any()))
                            .await?;
                        true
                    } else {
                        recorder.warn(EventAction::CreateDatabase, EventReason::Failed, format!("Unable to create database for {}: {err}", self.name_any()))
                            .await?;
                        false
                    }
                }
            },
        )
    }

    // Finalizer cleanup (the object was deleted, ensure nothing is orphaned)
    async fn cleanup(&self, ctx: Arc<Context>) -> crate::Result<Action> {
        let recorder = ctx.diagnostics.read().await.recorder_db(ctx.client.clone(), self);
        let name = self.name_any();

        // Delete the database only when requested
        if !self.spec.delete {
            recorder.info(EventAction::DeleteDatabase, EventReason::Ignored, format!("Skipping deletion of database for {name} due to configuration"))
                .await?;
            return Ok(Action::await_change());
        }

        // Run deletion
        let servers = ctx.servers.read().await;
        if let Some(pool) = servers.get(&self.spec.server_ref.name) {
            info!("Attempting to delete database {}", self.spec.name);
            let database_name = match sanitize_db_name(&self.spec.name) {
                Ok(name) => name,
                Err(err) => {
                    recorder.info(EventAction::DeleteDatabase, EventReason::InvalidName,
                                  format!("Invalid database name for {name}: {err}"))
                        .await?;
                    return Ok(Action::requeue(Duration::from_secs(5 * 60)));
                }
            };

            match sqlx::query::<Postgres>(&format!("DROP DATABASE \"{database_name}\";"))
                .bind(&self.spec.name)
                .execute(pool)
                .await
            {
                Ok(_result) => {
                    recorder.info(EventAction::DeleteDatabase, EventReason::Success,
                                  format!("Successfully deleted database for {name}"))
                        .await?;
                }
                Err(err) => {
                    recorder.info(EventAction::DeleteDatabase, EventReason::Failed,
                                  format!("Failed to deleted database for {name}: {err}"))
                        .await?;
                    return Ok(Action::requeue(Duration::from_secs(5 * 60)));
                }
            };
        } else {
            recorder.info(EventAction::DeleteDatabase, EventReason::MissingServer,
                          format!(
                              "Unable to create database for {name} due to unresolved server reference: {}",
                              self.spec.server_ref.name
                          ))
                .await?;
            return Ok(Action::requeue(Duration::from_secs(5 * 60)));
        }

        Ok(Action::await_change())
    }
}

fn sanitize_db_name(name: &str) -> Result<String, String> {
    // Regex to match valid database names (alphanumeric and underscores)
    let re = Regex::new(r"^[a-zA-Z0-9_]+$").expect("Invalid regex");

    // Check if the name is valid
    if !re.is_match(name) {
        return Err(
            "Invalid database name: Only alphanumeric characters and underscores are allowed.".into(),
        );
    }

    // Check length constraint (assuming PostgreSQL's limit of 63 characters)
    if name.len() > 63 {
        return Err("Invalid database name: Name must be 63 characters or fewer.".into());
    }

    // Add additional checks for reserved words if necessary
    Ok(name.to_string())
}

fn sanitize_user_name(name: &str) -> Result<String, String> {
    // Regex to match valid database names (alphanumeric and underscores)
    let re = Regex::new(r"^[a-zA-Z0-9_]+$").expect("Invalid regex");

    // Check if the name is valid
    if !re.is_match(name) {
        return Err("Invalid user name: Only alphanumeric characters and underscores are allowed.".into());
    }

    // Check length constraint (assuming PostgreSQL's limit of 63 characters)
    if name.len() > 63 {
        return Err("Invalid user name: Name must be 63 characters or fewer.".into());
    }

    // Add additional checks for reserved words if necessary
    Ok(name.to_string())
}
