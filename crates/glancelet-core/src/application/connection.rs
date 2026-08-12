use std::sync::Arc;

use super::{SecretStore, WorkStore};
use crate::{domain::ProviderId, Result};

/// Coordinates the durable connection state with provider credential cleanup.
///
/// SQLite is moved to the safe disconnected state first. If credential cleanup
/// fails, sources remain disabled and the operation can be retried without
/// making provider calls with a partially disconnected connection.
pub struct ConnectionCommandService {
    store: Arc<dyn WorkStore>,
    secrets: Arc<dyn SecretStore>,
}

impl ConnectionCommandService {
    pub fn new(store: Arc<dyn WorkStore>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { store, secrets }
    }

    pub fn disconnect(
        &self,
        connection_id: &str,
        provider_id: &ProviderId,
        credential_key: &str,
    ) -> Result<()> {
        self.store
            .disconnect_connection(connection_id, provider_id)?;
        self.secrets.delete(credential_key)
    }
}
