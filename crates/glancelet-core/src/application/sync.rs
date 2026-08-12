use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use tokio::{
    sync::{Mutex, Semaphore},
    task::JoinSet,
};

const MAX_CONCURRENT_SYNCS: usize = 4;
const PROJECTION_RETRY_DELAY_SECONDS: i64 = 300;

use crate::{
    application::{Clock, SourceFailureKind, WorkStore},
    extension::ExtensionRegistry,
    GlanceletError, Result,
};

pub struct SyncCoordinator {
    store: Arc<dyn WorkStore>,
    registry: Arc<ExtensionRegistry>,
    clock: Arc<dyn Clock>,
    source_locks: std::sync::Mutex<HashMap<String, Weak<Mutex<()>>>>,
    sync_permits: Semaphore,
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
            sync_permits: Semaphore::new(MAX_CONCURRENT_SYNCS),
        }
    }

    fn source_lock(&self, source_config_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.source_locks.lock().expect("source lock map poisoned");
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(source_config_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(source_config_id.to_owned(), Arc::downgrade(&lock));
        lock
    }

    pub async fn sync(&self, source_config_id: &str) -> Result<usize> {
        let source_lock = self.source_lock(source_config_id);
        let _source_guard = source_lock.lock().await;
        let _permit =
            self.sync_permits.acquire().await.map_err(|_| {
                GlanceletError::Source("source synchronization was shut down".into())
            })?;

        let (config, runtime) = self.store.source_sync_state(source_config_id)?;
        if !config.enabled || config.removed_at.is_some() {
            return Err(GlanceletError::InvalidOperation(
                "source is disabled or removed".into(),
            ));
        }
        if runtime.authentication_required() {
            return Err(GlanceletError::AuthenticationRequired(
                "Reconnect the source connection before syncing".into(),
            ));
        }
        let adapter = self.registry.adapter(&config.source_type_id)?;
        let attempt_at = self.clock.now();
        self.store
            .record_sync_attempt(source_config_id, runtime.config_revision, attempt_at)?;

        let batch = match adapter.fetch(&config, runtime.checkpoint).await {
            Ok(batch) => batch,
            Err(error) => {
                let now = self.clock.now();
                let next_retry_at = if matches!(error, GlanceletError::AuthenticationRequired(_)) {
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
                    runtime.config_revision,
                    now,
                    next_retry_at,
                    failure_kind(&error),
                    &error.to_string(),
                )?;
                return Err(error);
            }
        };

        self.store
            .apply_source_batch(&config, runtime.config_revision, &batch, self.clock.now())
    }

    pub fn resume_connection(&self, connection_id: &str) -> Result<()> {
        self.store.resume_connection(connection_id)
    }

    pub async fn sync_many(
        self: &Arc<Self>,
        source_config_ids: Vec<String>,
    ) -> Vec<(String, Result<usize>)> {
        let mut pending = source_config_ids.into_iter();
        let mut jobs = JoinSet::new();
        for _ in 0..MAX_CONCURRENT_SYNCS {
            let Some(source_config_id) = pending.next() else {
                break;
            };
            spawn_sync(&mut jobs, Arc::clone(self), source_config_id);
        }

        let mut results = Vec::new();
        while let Some(completed) = jobs.join_next().await {
            match completed {
                Ok(result) => results.push(result),
                Err(_) => results.push((
                    "unknown".into(),
                    Err(GlanceletError::Source(
                        "source sync task stopped unexpectedly".into(),
                    )),
                )),
            }
            if let Some(source_config_id) = pending.next() {
                spawn_sync(&mut jobs, Arc::clone(self), source_config_id);
            }
        }
        results
    }
}

fn spawn_sync(
    jobs: &mut JoinSet<(String, Result<usize>)>,
    coordinator: Arc<SyncCoordinator>,
    source_config_id: String,
) {
    jobs.spawn(async move {
        let result = coordinator.sync(&source_config_id).await;
        (source_config_id, result)
    });
}

fn failure_kind(error: &GlanceletError) -> SourceFailureKind {
    match error {
        GlanceletError::AuthenticationRequired(_) => SourceFailureKind::AuthenticationRequired,
        GlanceletError::RateLimited { .. } => SourceFailureKind::RateLimited,
        _ => SourceFailureKind::Other,
    }
}

#[derive(Debug, Default)]
pub struct ProjectionDrainReport {
    pub attempted: usize,
    pub processed: usize,
    pub failures: Vec<String>,
    pub reached_limit: bool,
}

impl ProjectionDrainReport {
    pub fn failure_message(&self) -> Option<String> {
        let mut failures = self.failures.clone();
        if self.reached_limit {
            failures.push("projection drain reached its safety limit".into());
        }
        (!failures.is_empty()).then(|| failures.join("; "))
    }

    fn merge(&mut self, other: Self) {
        self.attempted += other.attempted;
        self.processed += other.processed;
        self.failures.extend(other.failures);
        self.reached_limit |= other.reached_limit;
    }
}

pub struct SourceChangeProcessor {
    store: Arc<dyn WorkStore>,
    registry: Arc<ExtensionRegistry>,
    clock: Arc<dyn Clock>,
    processing_lock: std::sync::Mutex<()>,
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
            processing_lock: std::sync::Mutex::new(()),
        }
    }

    fn enqueue_reprojections(&self) -> Result<usize> {
        let now = self.clock.now();
        let mut queued = 0;
        for config in self
            .store
            .source_configs()?
            .into_iter()
            .filter(|config| config.removed_at.is_none())
        {
            let Ok(projector) = self.registry.projector(&config.source_type_id) else {
                continue;
            };
            queued += self
                .store
                .enqueue_reprojections(&config.id, projector.version(), now)?;
        }
        Ok(queued)
    }

    fn process_changes(
        &self,
        changes: Vec<crate::domain::SourceChange>,
    ) -> Result<ProjectionDrainReport> {
        let mut report = ProjectionDrainReport {
            attempted: changes.len(),
            ..ProjectionDrainReport::default()
        };
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
                Ok(()) => report.processed += 1,
                Err(error) => {
                    let message = error.to_string();
                    let next_retry_at = self.clock.now()
                        + chrono::Duration::seconds(PROJECTION_RETRY_DELAY_SECONDS);
                    self.store.record_projection_failure(
                        change.id,
                        next_retry_at,
                        &message.chars().take(1_000).collect::<String>(),
                    )?;
                    report
                        .failures
                        .push(format!("change {}: {message}", change.id));
                }
            }
        }
        Ok(report)
    }

    pub fn process_pending(&self, limit: usize) -> Result<usize> {
        let _guard = self
            .processing_lock
            .lock()
            .expect("source change processor lock poisoned");
        if limit == 0 {
            return Err(GlanceletError::InvalidOperation(
                "projection batch size must be positive".into(),
            ));
        }
        self.enqueue_reprojections()?;
        let changes = self
            .store
            .pending_source_changes_at(limit, self.clock.now())?;
        let report = self.process_changes(changes)?;
        if let Some(message) = report.failure_message() {
            Err(GlanceletError::Source(format!(
                "{} projection(s) failed: {message}",
                report.failures.len()
            )))
        } else {
            Ok(report.processed)
        }
    }

    pub fn drain_pending(
        &self,
        batch_size: usize,
        max_changes: usize,
    ) -> Result<ProjectionDrainReport> {
        let _guard = self
            .processing_lock
            .lock()
            .expect("source change processor lock poisoned");
        if batch_size == 0 || max_changes == 0 {
            return Err(GlanceletError::InvalidOperation(
                "projection drain limits must be positive".into(),
            ));
        }
        self.enqueue_reprojections()?;
        let mut report = ProjectionDrainReport::default();
        while report.attempted < max_changes {
            let limit = batch_size.min(max_changes - report.attempted);
            let changes = self
                .store
                .pending_source_changes_at(limit, self.clock.now())?;
            if changes.is_empty() {
                break;
            }
            report.merge(self.process_changes(changes)?);
        }
        report.reached_limit = report.attempted >= max_changes
            && !self
                .store
                .pending_source_changes_at(1, self.clock.now())?
                .is_empty();
        Ok(report)
    }
}
