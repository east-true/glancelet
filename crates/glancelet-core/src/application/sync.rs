use std::{collections::HashMap, sync::Arc};

use tokio::sync::Mutex;

use crate::{
    application::{Clock, WorkStore},
    extension::ExtensionRegistry,
    Result,
};

pub struct SyncCoordinator {
    store: Arc<dyn WorkStore>,
    registry: Arc<ExtensionRegistry>,
    clock: Arc<dyn Clock>,
    source_locks: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl SyncCoordinator {
    pub fn new(
        store: Arc<dyn WorkStore>,
        registry: Arc<ExtensionRegistry>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            registry,
            clock,
            source_locks: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub async fn sync(&self, source_config_id: &str) -> Result<usize> {
        let source_lock = {
            let mut locks = self.source_locks.lock().expect("source lock map poisoned");
            Arc::clone(
                locks
                    .entry(source_config_id.to_owned())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _guard = source_lock.lock().await;

        let config = self.store.source_config(source_config_id)?;
        let runtime = self.store.source_runtime(source_config_id)?;
        let adapter = self.registry.adapter(&config.source_type_id)?;
        let attempt_at = self.clock.now();
        self.store
            .record_sync_attempt(source_config_id, attempt_at)?;

        let batch = match adapter.fetch(&config, runtime.checkpoint).await {
            Ok(batch) => batch,
            Err(error) => {
                let retry_seconds = error
                    .retry_after_seconds()
                    .unwrap_or(config.expected_sync_interval_seconds)
                    .max(1);
                self.store.record_sync_failure(
                    source_config_id,
                    self.clock.now(),
                    self.clock.now() + chrono::Duration::seconds(retry_seconds),
                    &error.to_string(),
                )?;
                return Err(error);
            }
        };

        self.store
            .apply_source_batch(&config, &batch, self.clock.now())
    }
}

pub struct SourceChangeProcessor {
    store: Arc<dyn WorkStore>,
    registry: Arc<ExtensionRegistry>,
    clock: Arc<dyn Clock>,
}

impl SourceChangeProcessor {
    pub fn new(
        store: Arc<dyn WorkStore>,
        registry: Arc<ExtensionRegistry>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            registry,
            clock,
        }
    }

    pub fn process_pending(&self, limit: usize) -> Result<usize> {
        let changes = self.store.pending_source_changes(limit)?;
        let mut processed = 0;
        for change in changes {
            let config = self
                .store
                .source_config(&change.source_entity.source_config_id)?;
            let projector = self.registry.projector(&config.source_type_id)?;
            let draft = projector.project(&change.source_entity, &change)?;
            self.store
                .apply_projection(&change, &draft, projector.version(), self.clock.now())?;
            processed += 1;
        }
        Ok(processed)
    }
}
