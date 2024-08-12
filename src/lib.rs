//! # db-operator - core code
//!
//! This crate contains the controller logic sans the API code. For API code, see
//! [`main.rs`](main.rs).

#![forbid(unsafe_code)]

use thiserror::Error;

/// Expose all controller components used by main
pub mod controllers;

/// Log and trace integrations
pub mod telemetry;

/// Metrics
mod metrics;
pub use metrics::Metrics;

mod connection;
#[cfg(test)]
pub mod fixtures;

use crate::controllers::database::Database;
use crate::controllers::database_event_recorder::DatabaseEventRecorder;
use crate::controllers::{database, database_server};
use chrono::{DateTime, Utc};
pub use controllers::database_server::DatabaseServer;
use futures::StreamExt;
use kube::api::ListParams;
use kube::runtime::events::{Recorder, Reporter};
use kube::runtime::watcher::Config;
use kube::runtime::Controller;
use kube::{Api, Client, Resource};
use serde::Serialize;
use sqlx::{Pool, Postgres};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

// Context for our reconciler
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Prometheus metrics
    pub metrics: Metrics,
    /// The connection pool for postgres servers.
    pub servers: Arc<RwLock<HashMap<String, Pool<Postgres>>>>,
}

/// State shared between the controller and the web server
#[derive(Clone, Default)]
pub struct State {
    /// Diagnostics populated by the reconciler
    diagnostics: Arc<RwLock<Diagnostics>>,
    /// Metrics registry
    registry: prometheus::Registry,
}

/// State wrapper around the controller outputs for the web server
impl State {
    /// Metrics getter
    pub fn metrics(&self) -> Vec<prometheus::proto::MetricFamily> {
        self.registry.gather()
    }

    /// State getter
    pub async fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.read().await.clone()
    }

    // Create a Controller Context that can update State
    pub fn to_context(&self, client: Client) -> Arc<Context> {
        Arc::new(Context {
            client,
            metrics: Metrics::default()
                .register(&self.registry)
                .expect("Metrics were already registered"),
            diagnostics: self.diagnostics.clone(),
            servers: Arc::new(RwLock::new(HashMap::new())),
        })
    }
}

/// Initialize the controller and shared state (given the crd is installed)
pub async fn run(state: State) {
    let client = Client::try_default().await.expect("failed to create kube Client");
    let servers = Api::<DatabaseServer>::all(client.clone());
    let databases = Api::<Database>::all(client.clone());

    if let Err(e) = servers.list(&ListParams::default().limit(1)).await {
        error!("DatabaseServer CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }

    if let Err(e) = databases.list(&ListParams::default().limit(1)).await {
        error!("Database CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }

    let context = state.to_context(client);

    let future1 = Controller::new(servers, Config::default().any_semantic())
        .shutdown_on_signal()
        .run(
            database_server::reconcile,
            database_server::error_policy,
            context.clone(),
        )
        .filter_map(|x| async move { Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    let future2 = Controller::new(databases, Config::default().any_semantic())
        .shutdown_on_signal()
        .run(database::reconcile, database::error_policy, context)
        .filter_map(|x| async move { Result::ok(x) })
        .for_each(|_| futures::future::ready(()));

    tokio::join!(future1, future2);
}

/// Diagnostics to be exposed by the web server
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
    pub reporter: Reporter,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "db-operator".into(),
        }
    }
}

impl Diagnostics {
    pub fn recorder(&self, client: Client, doc: &DatabaseServer) -> Recorder {
        Recorder::new(client, self.reporter.clone(), doc.object_ref(&()))
    }

    pub fn recorder_db(&self, client: Client, doc: &Database) -> DatabaseEventRecorder {
        Recorder::new(client, self.reporter.clone(), doc.object_ref(&())).into()
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("SerializationError: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("Kubernetes API Error: {0}")]
    KubeError(#[from] kube::Error),

    #[error("Finalizer Error: {0}")]
    // NB: awkward type because finalizer::Error embeds the reconciler error (which is this)
    // so boxing this error to break cycles
    FinalizerError(#[from] Box<kube::runtime::finalizer::Error<Error>>),

    #[error("IllegalDocument")]
    IllegalDatabaseServer,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn metric_label(&self) -> String {
        format!("{self:?}").to_lowercase()
    }
}
