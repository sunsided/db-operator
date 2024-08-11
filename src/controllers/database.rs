use crate::{Context, Error};
use chrono::Utc;
use kube::{
    api::{Api, Patch, PatchParams, ResourceExt},
    runtime::{
        controller::Action,
        events::{Event, EventType},
        finalizer::{finalizer, Event as Finalizer},
    },
    CustomResource,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Postgres;
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
        }

        // always overwrite status object with what we saw
        let new_status = Patch::Apply(json!({
            "apiVersion": "db-operator.widemeadows.de/v1",
            "kind": "Database",
            "status": DatabaseStatus {
                created: true  // TODO: only if it exists ...
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
            if let Some(pool) = servers.get(&self.name_any()) {
                info!("Attempting to delete database {}", self.spec.name);
                match sqlx::query::<Postgres>("DROP DATABASE $1;")
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
