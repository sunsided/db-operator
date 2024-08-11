use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgConnectOptions;
use std::str::FromStr;

/// Database connection details.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Connection {
    Url(StringValue),
    Details(ConnectionDetails),
}

impl Default for Connection {
    fn default() -> Self {
        Self::Url(StringValue::Value(String::from(
            "postgres://localhost:5432/postgres",
        )))
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct ConnectionDetails {
    /// The host to connect to.
    pub host: StringValue,
    /// The username of the administrative user.
    pub user: StringValue,
    /// The password of the administrative user.
    pub password: StringValue,
    /// The database to connect to for the administrative user; defaults to `postgres`.
    pub dbname: Option<StringValue>,
}

/// A literal value.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StringValue {
    Value(String),
    SecretRef(SecretRef),
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct SecretRef {
    /// The name of the secret.
    pub name: String,
    /// The key of the connection string URL in the secret.
    pub key: String,
}

impl StringValue {
    pub fn resolve(&self) -> Result<String, ResolveValueError> {
        match self {
            Self::Value(value) => Ok(value.to_owned()),
            Self::SecretRef(_) => todo!(),
        }
    }
}

impl ConnectionDetails {
    pub fn resolve(&self) -> Result<PgConnectOptions, ResolveValueError> {
        let host = self.host.resolve()?;
        let user = self.user.resolve()?;
        let password = self.password.resolve()?;
        let dbname = match &self.dbname {
            None => String::from("postgres"),
            Some(value) => value.resolve()?,
        };

        Ok(PgConnectOptions::new()
            .host(&host)
            .username(&user)
            .password(&password)
            .database(&dbname)
            .application_name(env!("CARGO_CRATE_NAME")))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveValueError {}

#[derive(Debug, thiserror::Error)]
pub enum ConnectionDetailsError {
    #[error(transparent)]
    DatabaseError(#[from] sqlx::Error),
    #[error(transparent)]
    ValueResolution(#[from] ResolveValueError),
}

impl Connection {
    pub fn to_pg_connect_options(&self) -> crate::Result<PgConnectOptions, ConnectionDetailsError> {
        Ok(match self {
            Connection::Url(value) => {
                PgConnectOptions::from_str(&value.resolve()?)?.application_name(env!("CARGO_CRATE_NAME"))
            }
            Connection::Details(details) => details.resolve()?,
        })
    }
}
