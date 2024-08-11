use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgConnectOptions;
use std::str::FromStr;

/// Database connection details.
#[derive(Default, Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct Connection {
    pub url: Option<String>,
    pub details: Option<ConnectionDetails>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ConnectionDetails {
    /// The host to connect to.
    pub host: String,
    /// The username of the administrative user.
    pub user: String,
    /// The password of the administrative user.
    pub password: String,
    /// The database to connect to for the administrative user; defaults to `postgres`.
    pub dbname: Option<String>,
}

impl Connection {
    pub fn to_pg_connect_options(&self) -> crate::Result<PgConnectOptions, sqlx::Error> {
        if let Some(url) = &self.url {
            return Ok(PgConnectOptions::from_str(url)?.application_name(env!("CARGO_CRATE_NAME")));
        }

        if let Some(details) = &self.details {
            return Ok(PgConnectOptions::new()
                .host(&details.host)
                .username(&details.user)
                .password(&details.password)
                .database(&details.dbname.to_owned().unwrap_or(String::from("postgres")))
                .application_name(env!("CARGO_CRATE_NAME")));
        }

        Err(sqlx::Error::Configuration(
            String::from("Missing configuration").into(),
        ))
    }
}
