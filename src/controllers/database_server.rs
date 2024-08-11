use crate::{Context, Error};
use chrono::Utc;
use k8s_openapi::api::core::v1::Secret;
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
use sqlx::postgres::PgPoolOptions;
use sqlx::Postgres;
use std::sync::Arc;
use tokio::time::Duration;
use tracing::*;

pub static DATABASE_SERVER_FINALIZER: &str = "databaseservers.db-operator.widemeadows.de";

/// Generate the Kubernetes wrapper struct `DatabaseServer` from our Spec and Status struct
///
/// This provides a hook for generating the CRD yaml (in crdgen.rs)
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(Default))]
#[kube(
    kind = "DatabaseServer",
    group = "db-operator.widemeadows.de",
    version = "v1",
    singular = "databaseserver",
    plural = "databaseservers",
    namespaced
)]
#[kube(status = "DatabaseServerStatus", shortname = "doc")]
pub struct DatabaseServerSpec {
    pub connection: crate::connection::Connection,
    pub enable: bool,
}

/// The status object of `DatabaseServer`
#[derive(Deserialize, Serialize, Clone, Default, Debug, JsonSchema)]
pub struct DatabaseServerStatus {
    pub enabled: bool,
    /// Whether a connection attempt was successful.
    pub connected: bool,
    /// The server version
    pub server_version: Option<String>,
}

impl DatabaseServer {
    fn was_enabled(&self) -> bool {
        self.status.as_ref().map(|s| s.enabled).unwrap_or(false)
    }

    fn was_connected(&self) -> bool {
        self.status.as_ref().map(|s| s.connected).unwrap_or(false)
    }
}

#[instrument(skip(ctx, doc), fields(trace_id))]
pub async fn reconcile(doc: Arc<DatabaseServer>, ctx: Arc<Context>) -> crate::Result<Action> {
    let trace_id = crate::telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.count_and_measure();
    ctx.diagnostics.write().await.last_event = Utc::now();
    let ns = doc.namespace().unwrap(); // doc is namespace scoped
    let docs: Api<DatabaseServer> = Api::namespaced(ctx.client.clone(), &ns);

    info!("Reconciling DatabaseServer \"{}\" in {}", doc.name_any(), ns);
    finalizer(&docs, DATABASE_SERVER_FINALIZER, doc, |event| async {
        match event {
            Finalizer::Apply(doc) => doc.reconcile(ctx.clone()).await,
            Finalizer::Cleanup(doc) => doc.cleanup(ctx.clone()).await,
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

pub fn error_policy(doc: Arc<DatabaseServer>, error: &Error, ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    ctx.metrics.reconcile_failure(&doc, error);
    Action::requeue(Duration::from_secs(5 * 60))
}

#[derive(Debug, sqlx::FromRow)]
struct PostgresVersionRow {
    version: Option<String>,
}

impl DatabaseServer {
    // Reconcile (for non-finalizer related changes)
    async fn reconcile(&self, ctx: Arc<Context>) -> crate::Result<Action> {
        let client = ctx.client.clone();
        let recorder = ctx.diagnostics.read().await.recorder(client.clone(), self);
        let ns = self.namespace().unwrap();
        let name = self.name_any();
        let docs: Api<DatabaseServer> = Api::namespaced(client.clone(), &ns);
        let secrets: Api<Secret> = Api::namespaced(client, &ns);

        let should_enable = self.spec.enable;
        if !self.was_enabled() && should_enable {
            // send an event once per hide
            recorder
                .publish(Event {
                    type_: EventType::Normal,
                    reason: "EnableRequested".into(),
                    note: Some(String::from("Enabling database server use")),
                    action: "Enabling".into(),
                    secondary: None,
                })
                .await
                .map_err(Error::KubeError)?;
        }

        // Connect to the database ensure validity of configuration.
        let mut server_version = None;
        let is_connected = match self.spec.connection.to_pg_connect_options(&secrets).await {
            Ok(connect_options) => {
                let host = connect_options.get_host().to_owned();

                match PgPoolOptions::new()
                    .max_connections(5)
                    .connect_with(connect_options)
                    .await
                {
                    Ok(pool) => {
                        server_version = match sqlx::query_as::<Postgres, PostgresVersionRow>(
                            "SELECT version() AS version;",
                        )
                        .fetch_one(&pool)
                        .await
                        {
                            Ok(row) => Some(row.version.unwrap_or(String::from("unknown"))),
                            Err(_) => None,
                        };

                        {
                            let mut servers = ctx.servers.write().await;
                            servers.insert(name.clone(), pool);
                        }

                        if !self.was_connected() {
                            recorder
                                .publish(Event {
                                    type_: EventType::Normal,
                                    reason: "Connected".into(),
                                    note: Some(format!(
                                        "Successfully connected to host `{}`: {}",
                                        host,
                                        server_version.to_owned().unwrap_or(String::from("unknown"))
                                    )),
                                    action: "Connect".into(),
                                    secondary: None,
                                })
                                .await
                                .map_err(Error::KubeError)?;
                        }
                        true
                    }
                    Err(err) => {
                        if !self.was_connected() {
                            recorder
                                .publish(Event {
                                    type_: EventType::Warning,
                                    reason: "ConnectionFailed".into(),
                                    note: Some(format!("Failed to connect to `{name}`: {err}")),
                                    action: "Connect".into(),
                                    secondary: None,
                                })
                                .await
                                .map_err(Error::KubeError)?;
                        }

                        error!(
                            "Failed to connect to database instance {}: {}",
                            self.metadata.name.to_owned().unwrap_or_default(),
                            err
                        );
                        false
                    }
                }
            }
            Err(err) => {
                recorder
                    .publish(Event {
                        type_: EventType::Normal,
                        reason: "ConnectionFailed".into(),
                        note: Some(format!("Invalid connection details: {err}")),
                        action: "Connect".into(),
                        secondary: None,
                    })
                    .await
                    .map_err(Error::KubeError)?;
                false
            }
        };

        // always overwrite status object with what we saw
        let new_status = Patch::Apply(json!({
            "apiVersion": "db-operator.widemeadows.de/v1",
            "kind": "DatabaseServer",
            "status": DatabaseServerStatus {
                enabled: should_enable,
                connected: is_connected,
                server_version
            }
        }));
        let ps = PatchParams::apply("cntrlr").force();
        let _o = docs
            .patch_status(&name, &ps, &new_status)
            .await
            .map_err(Error::KubeError)?;

        // If no events were received, check back every 5 minutes
        Ok(Action::requeue(Duration::from_secs(5 * 60)))
    }

    // Finalizer cleanup (the object was deleted, ensure nothing is orphaned)
    async fn cleanup(&self, ctx: Arc<Context>) -> crate::Result<Action> {
        let recorder = ctx.diagnostics.read().await.recorder(ctx.client.clone(), self);

        // Remove the connection pool.
        {
            let mut servers = ctx.servers.write().await;
            servers.remove(&self.name_any());
        }

        // DatabaseServer doesn't have any real cleanup, so we just publish an event
        recorder
            .publish(Event {
                type_: EventType::Normal,
                reason: "DeleteRequested".into(),
                note: Some(format!("Delete `{}`", self.name_any())),
                action: "Deleting".into(),
                secondary: None,
            })
            .await
            .map_err(Error::KubeError)?;
        Ok(Action::await_change())
    }
}

// Mock tests relying on fixtures.rs and its primitive apiserver mocks
#[cfg(test)]
mod test {
    use super::{error_policy, reconcile, Context, DatabaseServer};
    use crate::fixtures::{timeout_after_1s, Scenario};
    use std::sync::Arc;

    #[tokio::test]
    async fn documents_without_finalizer_gets_a_finalizer() {
        let (testctx, fakeserver, _) = Context::test();
        let doc = DatabaseServer::test();
        let mocksrv = fakeserver.run(Scenario::FinalizerCreation(doc.clone()));
        reconcile(Arc::new(doc), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn finalized_doc_causes_status_patch() {
        let (testctx, fakeserver, _) = Context::test();
        let doc = DatabaseServer::test().finalized();
        let mocksrv = fakeserver.run(Scenario::StatusPatch(doc.clone()));
        reconcile(Arc::new(doc), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn finalized_doc_with_hide_causes_event_and_hide_patch() {
        let (testctx, fakeserver, _) = Context::test();
        let doc = DatabaseServer::test().finalized().needs_hide();
        let scenario = Scenario::EventPublishThenStatusPatch("HideRequested".into(), doc.clone());
        let mocksrv = fakeserver.run(scenario);
        reconcile(Arc::new(doc), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn finalized_doc_with_delete_timestamp_causes_delete() {
        let (testctx, fakeserver, _) = Context::test();
        let doc = DatabaseServer::test().finalized().needs_delete();
        let mocksrv = fakeserver.run(Scenario::Cleanup("DeleteRequested".into(), doc.clone()));
        reconcile(Arc::new(doc), testctx).await.expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn illegal_doc_reconcile_errors_which_bumps_failure_metric() {
        let (testctx, fakeserver, _registry) = Context::test();
        let doc = Arc::new(DatabaseServer::illegal().finalized());
        let mocksrv = fakeserver.run(Scenario::RadioSilence);
        let res = reconcile(doc.clone(), testctx.clone()).await;
        timeout_after_1s(mocksrv).await;
        assert!(res.is_err(), "apply reconciler fails on illegal doc");
        let err = res.unwrap_err();
        assert!(err.to_string().contains("IllegalDatabaseServer"));
        // calling error policy with the reconciler error should cause the correct metric to be set
        error_policy(doc.clone(), &err, testctx.clone());
        //dbg!("actual metrics: {}", registry.gather());
        let failures = testctx
            .metrics
            .failures
            .with_label_values(&["illegal", "finalizererror(applyfailed(illegaldocument))"])
            .get();
        assert_eq!(failures, 1);
    }

    // Integration test without mocks
    use kube::api::{Api, ListParams, Patch, PatchParams};
    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn integration_reconcile_should_set_status_and_send_event() {
        let client = kube::Client::try_default().await.unwrap();
        let ctx = crate::State::default().to_context(client.clone());

        // create a test doc
        let doc = DatabaseServer::test().finalized().needs_hide();
        let docs: Api<DatabaseServer> = Api::namespaced(client.clone(), "default");
        let ssapply = PatchParams::apply("ctrltest");
        let patch = Patch::Apply(doc.clone());
        docs.patch("test", &ssapply, &patch).await.unwrap();

        // reconcile it (as if it was just applied to the cluster like this)
        reconcile(Arc::new(doc), ctx).await.unwrap();

        // verify side-effects happened
        let output = docs.get_status("test").await.unwrap();
        assert!(output.status.is_some());
        // verify hide event was found
        let events: Api<k8s_openapi::api::core::v1::Event> = Api::all(client.clone());
        let opts =
            ListParams::default().fields("involvedObject.kind=DatabaseServer,involvedObject.name=test");
        let event = events
            .list(&opts)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.reason.as_deref() == Some("HideRequested"))
            .last()
            .unwrap();
        dbg!("got ev: {:?}", &event);
        assert_eq!(event.action.as_deref(), Some("Hiding"));
    }
}
