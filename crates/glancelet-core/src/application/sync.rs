use std::{collections::HashMap, sync::Arc};

use tokio::{sync::Mutex, task::JoinSet};

const PROJECTION_RETRY_DELAY_SECONDS: i64 = 300;

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
        if !config.enabled || config.removed_at.is_some() {
            return Err(crate::GlanceletError::InvalidOperation(
                "source is disabled or removed".into(),
            ));
        }
        let runtime = self.store.source_runtime(source_config_id)?;
        if runtime.authentication_required() {
            return Err(crate::GlanceletError::AuthenticationRequired(
                "Reconnect the source connection before syncing".into(),
            ));
        }
        let adapter = self.registry.adapter(&config.source_type_id)?;
        let attempt_at = self.clock.now();
        self.store
            .record_sync_attempt(source_config_id, attempt_at)?;

        let batch = match adapter.fetch(&config, runtime.checkpoint).await {
            Ok(batch) => batch,
            Err(error) => {
                let now = self.clock.now();
                let next_retry_at =
                    if matches!(error, crate::GlanceletError::AuthenticationRequired(_)) {
                        // Authentication cannot recover with time. A successful reconnect
                        // explicitly resumes every SourceConfig for this Connection.
                        None
                    } else {
                        let retry_seconds = error
                            .retry_after_seconds()
                            .unwrap_or(config.expected_sync_interval_seconds)
                            .max(1);
                        Some(now + chrono::Duration::seconds(retry_seconds))
                    };
                self.store.record_sync_failure(
                    source_config_id,
                    now,
                    next_retry_at,
                    &error.to_string(),
                )?;
                return Err(error);
            }
        };

        self.store
            .apply_source_batch(&config, &batch, self.clock.now())
    }

    pub fn resume_connection(&self, connection_id: &str) -> Result<()> {
        for config in
            self.store.source_configs()?.into_iter().filter(|config| {
                config.connection_id == connection_id && config.removed_at.is_none()
            })
        {
            if self
                .store
                .source_runtime(&config.id)?
                .authentication_required()
            {
                self.store.clear_sync_failure(&config.id)?;
            }
        }
        Ok(())
    }

    /// Runs independent SourceConfigs concurrently while `sync` keeps the
    /// existing per-SourceConfig single-flight invariant.
    pub async fn sync_many(
        self: &Arc<Self>,
        source_config_ids: Vec<String>,
    ) -> Vec<(String, Result<usize>)> {
        let mut jobs = JoinSet::new();
        for source_config_id in source_config_ids {
            let coordinator = Arc::clone(self);
            jobs.spawn(async move {
                let result = coordinator.sync(&source_config_id).await;
                (source_config_id, result)
            });
        }
        let mut results = Vec::new();
        while let Some(completed) = jobs.join_next().await {
            match completed {
                Ok(result) => results.push(result),
                Err(_) => results.push((
                    "unknown".into(),
                    Err(crate::GlanceletError::Source(
                        "source sync task stopped unexpectedly".into(),
                    )),
                )),
            }
        }
        results
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
        let changes = self
            .store
            .pending_source_changes_at(limit, self.clock.now())?;
        let mut processed = 0;
        let mut failures = Vec::new();
        for change in changes {
            let result = (|| {
                let config = self
                    .store
                    .source_config(&change.source_entity.source_config_id)?;
                let projector = self.registry.projector(&config.source_type_id)?;
                let draft = projector.project(&change.source_entity, &change)?;
                self.store
                    .apply_projection(&change, &draft, projector.version(), self.clock.now())
            })();
            match result {
                Ok(()) => processed += 1,
                Err(error) => {
                    let message = error.to_string();
                    let next_retry_at = self.clock.now()
                        + chrono::Duration::seconds(PROJECTION_RETRY_DELAY_SECONDS);
                    self.store.record_projection_failure(
                        change.id,
                        next_retry_at,
                        &message.chars().take(1_000).collect::<String>(),
                    )?;
                    failures.push(format!("change {}: {message}", change.id));
                }
            }
        }
        if failures.is_empty() {
            Ok(processed)
        } else {
            Err(crate::GlanceletError::Source(format!(
                "{} projection(s) failed: {}",
                failures.len(),
                failures.join("; ")
            )))
        }
    }
}
