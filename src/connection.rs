use crate::controllers::secrets::{SecretError, SecretRef};
use k8s_openapi::api::core::v1::Secret;
use kube::Api;
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

impl StringValue {
    pub async fn resolve(&self, api: &Api<Secret>) -> Result<String, ResolveValueError> {
        match self {
            Self::Value(value) => Ok(value.to_owned()),
            Self::SecretRef(sr) => Ok(sr.get_field(api).await?),
        }
    }
}

impl ConnectionDetails {
    pub async fn resolve(&self, api: &Api<Secret>) -> Result<PgConnectOptions, ResolveValueError> {
        let host = self.host.resolve(api).await?;
        let user = self.user.resolve(api).await?;
        let password = self.password.resolve(api).await?;
        let dbname = match &self.dbname {
            None => String::from("postgres"),
            Some(value) => value.resolve(api).await?,
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
pub enum ResolveValueError {
    #[error(transparent)]
    SecretError(#[from] SecretError),
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectionDetailsError {
    #[error(transparent)]
    DatabaseError(#[from] sqlx::Error),
    #[error(transparent)]
    ValueResolution(#[from] ResolveValueError),
}

impl Connection {
    pub async fn to_pg_connect_options(
        &self,
        api: &Api<Secret>,
    ) -> crate::Result<PgConnectOptions, ConnectionDetailsError> {
        Ok(match self {
            Connection::Url(value) => PgConnectOptions::from_str(&value.resolve(api).await?)?
                .application_name(env!("CARGO_CRATE_NAME")),
            Connection::Details(details) => details.resolve(api).await?,
        })
    }
}
