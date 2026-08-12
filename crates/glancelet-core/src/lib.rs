pub mod application;
pub mod domain;
pub mod extension;
pub mod sources;
pub mod storage;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GlanceletError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("source error: {0}")]
    Source(String),
    #[error("source configuration is required: {0}")]
    ConfigurationRequired(String),
    #[error("transient network error: {0}")]
    TransientNetwork(String),
    #[error("provider failure: {0}")]
    ProviderFailure(String),
    #[error("unknown source type: {0}")]
    UnknownSource(String),
    #[error("invalid operation: {0}")]
    InvalidOperation(String),
    #[error("invalid navigation target: {0}")]
    InvalidNavigation(String),
    #[error("OS secret store is unavailable: {0}")]
    SecretStoreUnavailable(String),
    #[error("authentication is required: {0}")]
    AuthenticationRequired(String),
    #[error("OAuth failed: {0}")]
    OAuth(String),
    #[error("{provider} rate limited the request; retry after {retry_after_seconds} seconds")]
    RateLimited {
        provider: String,
        retry_after_seconds: i64,
    },
    #[error("not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, GlanceletError>;

impl GlanceletError {
    pub fn retry_after_seconds(&self) -> Option<i64> {
        match self {
            Self::RateLimited {
                retry_after_seconds,
                ..
            } => Some(*retry_after_seconds),
            _ => None,
        }
    }
}
