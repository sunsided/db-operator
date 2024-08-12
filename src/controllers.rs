pub mod database;
pub mod database_event_recorder;
pub mod database_server;
mod grants;
pub(crate) mod secrets;

pub static LAST_APPLIED_CONFIGURATION: &str = "db-operator.widemeadows.de/last-applied-configuration";
pub static DATABASE_READY: &str = "db-operator.widemeadows.de/database-ready";
