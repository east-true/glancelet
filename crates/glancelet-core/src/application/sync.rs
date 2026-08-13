use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use tokio::{
    sync::{Mutex, Semaphore},
    task::JoinSet,
};

const MAX_CONCURRENT_SYNCS: usize = 4;
const MIN_SYNC_RETRY_SECONDS: i64 = 30;
const MAX_SYNC_RETRY_SECONDS: i64 = 6 * 60 * 60;
const PROJECTION_RETRY_BASE_SECONDS: i64 = 5 * 60;
const PROJECTION_RETRY_MAX_SECONDS: i64 = 6 * 60 * 60;
const MAX_PROJECTION_ATTEMPTS: i64 = 5;

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
                let kind = SourceFailureKind::from(&error);
                let next_retry_at = retry_at(
                    source_config_id,
                    config.expected_sync_interval_seconds,
                    runtime.failure_count + 1,
                    kind,
                    &error,
                    now,
                );
                self.store.record_sync_failure(
                    source_config_id,
                    runtime.config_revision,
                    now,
                    next_retry_at,
                    kind,
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

    /// Runs independent SourceConfigs concurrently while `sync` keeps the
    /// existing per-SourceConfig single-flight invariant. The shared semaphore
    /// keeps large source sets from opening an unbounded number of provider calls.
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
                    Err(GlanceletError::Source(
                        "source sync task stopped unexpectedly".into(),
                    )),
                )),
            }
        }
        results
    }
}

fn retry_at(
    source_config_id: &str,
    expected_interval_seconds: i64,
    failure_count: i64,
    kind: SourceFailureKind,
    error: &GlanceletError,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    match kind {
        SourceFailureKind::AuthenticationRequired | SourceFailureKind::ConfigurationRequired => {
            None
        }
        SourceFailureKind::RateLimited => {
            Some(now + chrono::Duration::seconds(error.retry_after_seconds().unwrap_or(60).max(1)))
        }
        SourceFailureKind::TransientNetwork
        | SourceFailureKind::ProviderFailure
        | SourceFailureKind::Other => Some(
            now + chrono::Duration::seconds(exponential_retry_seconds(
                source_config_id,
                expected_interval_seconds,
                failure_count,
            )),
        ),
    }
}

fn exponential_retry_seconds(
    source_config_id: &str,
    expected_interval_seconds: i64,
    failure_count: i64,
) -> i64 {
    let base = expected_interval_seconds.clamp(MIN_SYNC_RETRY_SECONDS, MAX_SYNC_RETRY_SECONDS);
    let exponent = (failure_count.saturating_sub(1)).clamp(0, 12) as u32;
    let multiplier = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
    let without_jitter = base.saturating_mul(multiplier).min(MAX_SYNC_RETRY_SECONDS);
    let jitter_window = (without_jitter / 5).max(1);
    let jitter = deterministic_jitter(source_config_id, failure_count, jitter_window);
    without_jitter
        .saturating_add(jitter)
        .min(MAX_SYNC_RETRY_SECONDS)
}

fn deterministic_jitter(source_config_id: &str, failure_count: i64, window: i64) -> i64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in source_config_id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    for byte in failure_count.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % (window as u64 + 1)) as i64
}

#[derive(Debug, Default)]
pub struct ProjectionDrainReport {
    pub attempted: usize,
    pub processed: usize,
    pub failures: Vec<String>,
}

impl ProjectionDrainReport {
    pub fn changed_work(&self) -> bool {
        self.processed > 0
    }

    pub fn failure_message(&self) -> Option<String> {
        (!self.failures.is_empty()).then(|| {
            format!(
                "{} projection(s) failed: {}",
                self.failures.len(),
                self.failures.join("; ")
            )
        })
    }

    fn merge(&mut self, other: Self) {
        self.attempted += other.attempted;
        self.processed += other.processed;
        self.failures.extend(other.failures);
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
            let mut projector_version = 0;
            let result = (|| {
                let config = self
                    .store
                    .source_config(&change.source_entity.source_config_id)?;
                let projector = self.registry.projector(&config.source_type_id)?;
                projector_version = projector.version();
                let draft = projector.project(&change.source_entity, &change)?;
                self.store
                    .apply_projection(&change, &draft, projector_version, self.clock.now())
            })();
            match result {
                Ok(()) => report.processed += 1,
                Err(error) => {
                    let message = error.to_string();
                    let state = self.store.record_projection_failure(
                        change.id,
                        projector_version,
                        self.clock.now(),
                        PROJECTION_RETRY_BASE_SECONDS,
                        PROJECTION_RETRY_MAX_SECONDS,
                        MAX_PROJECTION_ATTEMPTS,
                        &message.chars().take(1_000).collect::<String>(),
                    )?;
                    if state.quarantined() {
                        report.failures.push(format!(
                            "change {} quarantined after {} projection attempts: {message}",
                            change.id, state.failure_count
                        ));
                    } else {
                        report.failures.push(format!(
                            "change {} projection attempt {} failed; retry at {}: {message}",
                            change.id,
                            state.failure_count,
                            state
                                .next_retry_at
                                .expect("non-quarantined projection failure has retry time")
                        ));
                    }
                }
            }
        }
        Ok(report)
    }

    pub fn process_pending(&self, limit: usize) -> Result<usize> {
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
            Err(GlanceletError::Source(message))
        } else {
            Ok(report.processed)
        }
    }

    pub fn drain_pending(
        &self,
        batch_size: usize,
        max_changes: usize,
    ) -> Result<ProjectionDrainReport> {
        if batch_size == 0 || max_changes == 0 {
            return Err(GlanceletError::InvalidOperation(
                "projection drain limits must be positive".into(),
            ));
        }
        self.enqueue_reprojections()?;
        let mut report = ProjectionDrainReport::default();
        while report.attempted < max_changes {
            let remaining = max_changes - report.attempted;
            let limit = batch_size.min(remaining);
            let changes = self
                .store
                .pending_source_changes_at(limit, self.clock.now())?;
            if changes.is_empty() {
                break;
            }
            report.merge(self.process_changes(changes)?);
        }
        if !self
            .store
            .pending_source_changes_at(1, self.clock.now())?
            .is_empty()
        {
            report.failures.push(format!(
                "projection drain reached its {max_changes}-change safety limit"
            ));
        }
        Ok(report)
    }
}
