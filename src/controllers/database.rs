use crate::controllers::grants::Grants;
use crate::{Context, Error};
use chrono::Utc;
use kube::runtime::events::Recorder;
use kube::{
    api::{Api, Patch, PatchParams, ResourceExt},
    runtime::{
        controller::Action,
        events::{Event, EventType},
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
    /// The optional comment to add to the database.
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

#[instrument(skip(ctx, doc), fields(trace_id))]
pub async fn reconcile(doc: Arc<Database>, ctx: Arc<Context>) -> crate::Result<Action> {
    let trace_id = crate::telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.count_and_measure();
    ctx.diagnostics.write().await.last_event = Utc::now();
    let ns = doc.namespace().unwrap(); // doc is namespace scoped
    let dbs: Api<Database> = Api::namespaced(ctx.client.clone(), &ns);

    info!("Reconciling Database \"{}\" in {}", doc.name_any(), ns);
    finalizer(&dbs, DATABASE_FINALIZER, doc, |event| async {
        match event {
            Finalizer::Apply(doc) => doc.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(doc) => doc.cleanup(ctx.clone()).await,
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
                recorder
                    .publish(Event {
                        type_: EventType::Warning,
                        reason: "MissingServer".into(),
                        note: Some(format!(
                            "Unable to create database due to invalid server reference: {}",
                            self.spec.server_ref.name
                        )),
                        action: "CreateDatabase".into(),
                        secondary: None,
                    })
                    .await
                    .map_err(Error::KubeError)?;

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
                    recorder
                        .publish(Event {
                            type_: EventType::Warning,
                            reason: "InspectionFailed".into(),
                            note: Some(format!("Failed to test for databases: {err}")),
                            action: "CreateDatabase".into(),
                            secondary: None,
                        })
                        .await
                        .map_err(Error::KubeError)?;

                    return Ok(Action::requeue(Duration::from_secs(5 * 60)));
                }
            };

        let database_name = match sanitize_db_name(&self.spec.name) {
            Ok(name) => name,
            Err(err) => {
                recorder
                    .publish(Event {
                        type_: EventType::Warning,
                        reason: "InvalidName".into(),
                        note: Some(format!("Database name invalid: {err}")),
                        action: "ReconcileDatabase".into(),
                        secondary: None,
                    })
                    .await
                    .map_err(Error::KubeError)?;

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
                    // send an event once per hide
                    recorder
                        .publish(Event {
                            type_: EventType::Normal,
                            reason: "CreateRequested".into(),
                            note: Some(String::from("Creating database")),
                            action: "CreateDatabase".into(),
                            secondary: None,
                        })
                        .await
                        .map_err(Error::KubeError)?;

                    info!("Attempting to create database {}", self.spec.name);
                    created = self.create_database(&recorder, pool, &database_name).await?;
                }
            }
        }

        // Update the database comment.
        if let Some(comment) = &self.spec.comment {
            match sqlx::query::<Postgres>(&format!("COMMENT ON DATABASE \"{database_name}\" IS $1;"))
                .bind(comment)
                .execute(pool)
                .await
            {
                Ok(_result) => {
                    debug!("Set comment on database");
                }
                Err(err) => {
                    warn!("Failed to set comment on database {database_name}: {err}");
                    recorder
                        .publish(Event {
                            type_: EventType::Normal,
                            reason: "CommentFailed".into(),
                            note: Some(format!("Failed to apply comment to database: {err}")),
                            action: "CommentDatabase".into(),
                            secondary: None,
                        })
                        .await
                        .map_err(Error::KubeError)?;
                }
            }
        }

        // Go through all grants.
        for user_grant in &self.spec.grants {
            let user_name = match sanitize_user_name(&user_grant.user) {
                Ok(name) => name,
                Err(err) => {
                    recorder
                        .publish(Event {
                            type_: EventType::Warning,
                            reason: "InvalidName".into(),
                            note: Some(format!("User name invalid: {err}")),
                            action: "ReconcileDatabase".into(),
                            secondary: None,
                        })
                        .await
                        .map_err(Error::KubeError)?;

                    return Ok(Action::requeue(Duration::from_secs(5 * 60)));
                }
            };

            if let Some(db_grant) = &user_grant.database {
                for privilege in &db_grant.grants {
                    let privilege = privilege.to_sql();
                    match sqlx::query::<Postgres>(&format!(
                        "GRANT {privilege} ON \"{database_name}\" TO \"{user_name}\";"
                    ))
                    .execute(pool)
                    .await
                    {
                        Ok(_result) => {
                            debug!(
                                "Set privilege {privilege} on {database_name} to {}",
                                user_grant.user
                            );
                            recorder
                                .publish(Event {
                                    type_: EventType::Normal,
                                    reason: "Success".into(),
                                    note: Some(format!(
                                        "Applied privilege {privilege} on database {database_name} to {}",
                                        user_grant.user
                                    )),
                                    action: "ApplyGrant".into(),
                                    secondary: None,
                                })
                                .await
                                .map_err(Error::KubeError)?;
                        }
                        Err(err) => {
                            warn!("Failed to set comment on database {database_name}: {err}");
                            recorder
                                .publish(Event {
                                    type_: EventType::Warning,
                                    reason: "GrantFailed".into(),
                                    note: Some(format!("Failed to apply privilege {privilege} on database {database_name} to {}: {err}", user_grant.user)),
                                    action: "ApplyGrant".into(),
                                    secondary: None,
                                })
                                .await
                                .map_err(Error::KubeError)?;
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
        recorder: &Recorder,
        pool: &Pool<Postgres>,
        database_name: &String,
    ) -> Result<bool, Error> {
        Ok(
            match sqlx::query::<Postgres>(&format!("CREATE DATABASE \"{database_name}\";"))
                .execute(pool)
                .await
            {
                Ok(_result) => {
                    recorder
                        .publish(Event {
                            type_: EventType::Normal,
                            reason: "Created".into(),
                            note: Some(format!("Successfully created `{}`", self.name_any())),
                            action: "CreateDatabase".into(),
                            secondary: None,
                        })
                        .await
                        .map_err(Error::KubeError)?;
                    true
                }
                Err(err) => {
                    // TODO: Find a less hacky solution.
                    if err.to_string().contains("already exists") {
                        info!("The database already existed");
                        recorder
                            .publish(Event {
                                type_: EventType::Warning,
                                reason: "NothingToCreate".into(),
                                note: Some(format!("Database `{}` was already created", self.name_any())),
                                action: "CreateDatabase".into(),
                                secondary: None,
                            })
                            .await
                            .map_err(Error::KubeError)?;

                        true
                    } else {
                        recorder
                            .publish(Event {
                                type_: EventType::Warning,
                                reason: "CreationFailed".into(),
                                note: Some(format!(
                                    "Unable to create database `{}`: {}",
                                    self.name_any(),
                                    err
                                )),
                                action: "CreateDatabase".into(),
                                secondary: None,
                            })
                            .await
                            .map_err(Error::KubeError)?;
                        false
                    }
                }
            },
        )
    }

    // Finalizer cleanup (the object was deleted, ensure nothing is orphaned)
    async fn cleanup(&self, ctx: Arc<Context>) -> crate::Result<Action> {
        let recorder = ctx.diagnostics.read().await.recorder_db(ctx.client.clone(), self);

        // Delete the database only when requested
        if self.spec.delete {
            recorder
                .publish(Event {
                    type_: EventType::Normal,
                    reason: "DeleteRequested".into(),
                    note: Some(format!("Deleting `{}` due to configuration", self.name_any())),
                    action: "DeleteDatabase".into(),
                    secondary: None,
                })
                .await
                .map_err(Error::KubeError)?;

            // Run deletion
            let servers = ctx.servers.read().await;
            if let Some(pool) = servers.get(&self.spec.server_ref.name) {
                info!("Attempting to delete database {}", self.spec.name);
                let database_name = match sanitize_db_name(&self.spec.name) {
                    Ok(name) => name,
                    Err(err) => {
                        recorder
                            .publish(Event {
                                type_: EventType::Warning,
                                reason: "InvalidName".into(),
                                note: Some(format!("Database name invalid: {err}")),
                                action: "DeleteDatabase".into(),
                                secondary: None,
                            })
                            .await
                            .map_err(Error::KubeError)?;

                        return Ok(Action::requeue(Duration::from_secs(5 * 60)));
                    }
                };

                match sqlx::query::<Postgres>(&format!("DROP DATABASE \"{database_name}\";"))
                    .bind(&self.spec.name)
                    .execute(pool)
                    .await
                {
                    Ok(result) => {
                        if result.rows_affected() > 0 {
                            recorder
                                .publish(Event {
                                    type_: EventType::Normal,
                                    reason: "Deleted".into(),
                                    note: Some(format!("Successfully deleted `{}`", self.name_any())),
                                    action: "DeleteDatabase".into(),
                                    secondary: None,
                                })
                                .await
                                .map_err(Error::KubeError)?;
                        } else {
                            recorder
                                .publish(Event {
                                    type_: EventType::Warning,
                                    reason: "NothingToDelete".into(),
                                    note: Some(format!("Database `{}` was already removed", self.name_any())),
                                    action: "DeleteDatabase".into(),
                                    secondary: None,
                                })
                                .await
                                .map_err(Error::KubeError)?;
                        }
                    }
                    Err(err) => {
                        recorder
                            .publish(Event {
                                type_: EventType::Warning,
                                reason: "DeletionFailed".into(),
                                note: Some(format!(
                                    "Unable to delete database `{}`: {}",
                                    self.name_any(),
                                    err
                                )),
                                action: "DeleteDatabase".into(),
                                secondary: None,
                            })
                            .await
                            .map_err(Error::KubeError)?;
                        return Err(Error::IllegalDatabaseServer);
                    }
                };

                recorder
                    .publish(Event {
                        type_: EventType::Normal,
                        reason: "Deleted".into(),
                        note: Some(format!("Successfully deleted `{}`", self.name_any())),
                        action: "DeleteDatabase".into(),
                        secondary: None,
                    })
                    .await
                    .map_err(Error::KubeError)?;
            } else {
                recorder
                    .publish(Event {
                        type_: EventType::Warning,
                        reason: "DeletionFailed".into(),
                        note: Some(format!(
                            "Unable to delete database `{}` because the connection to the server was lost",
                            self.name_any()
                        )),
                        action: "DeleteDatabase".into(),
                        secondary: None,
                    })
                    .await
                    .map_err(Error::KubeError)?;
            }
        } else {
            recorder
                .publish(Event {
                    type_: EventType::Normal,
                    reason: "Ignored".into(),
                    note: Some(format!(
                        "Skipping deletion of `{}` due to configuration",
                        self.name_any()
                    )),
                    action: "DeleteDatabase".into(),
                    secondary: None,
                })
                .await
                .map_err(Error::KubeError)?;
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
