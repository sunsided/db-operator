use k8s_openapi::api::core::v1::Secret;
use kube::Api;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::string::FromUtf8Error;

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct SecretRef {
    /// The name of the secret.
    pub name: String,
    /// The key of the connection string URL in the secret.
    pub key: String,
}

impl SecretRef {
    pub async fn get(&self, api: &Api<Secret>) -> Result<Secret, SecretError> {
        Ok(api.get(&self.name).await?)
    }

    pub async fn get_field(&self, api: &Api<Secret>) -> Result<String, SecretError> {
        let secret = self.get(api).await?;
        self.get_field_with(&secret).await
    }

    pub async fn get_field_with(&self, secret: &Secret) -> Result<String, SecretError> {
        let data = secret
            .data
            .as_ref()
            .ok_or(SecretError::SecretHasNoData)?
            .get(&self.key)
            .cloned()
            .ok_or_else(|| SecretError::SecretNotFound(self.key.to_owned()))?;

        let data = String::from_utf8(data.0)?;
        Ok(data)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("Kubernetes error: {0}")]
    Kubernetes(#[from] kube::Error),

    #[error("Secret not found: {0}")]
    SecretNotFound(String),

    #[error("Secret has no data")]
    SecretHasNoData,

    #[error("Secret has invalid base64 data")]
    InvalidBase64(#[from] FromUtf8Error),
}
