mod clock;
mod navigation;
mod secrets;
mod sync;
mod work;

pub use clock::*;
pub use navigation::*;
pub use secrets::*;
pub use sync::*;
pub use work::*;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{
    domain::{
        SourceBatch, SourceChange, WorkBinding, WorkDraft, WorkEntry, WorkPlanning, WorkProgress,
    },
    extension::{Connection, SourceConfig},
    Result,
};

#[derive(Clone, Debug)]
pub struct SourceRuntime {
    pub checkpoint: Option<Value>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub next_sync_at: Option<DateTime<Utc>>,
    pub failure_count: i64,
    pub last_error: Option<String>,
}

impl SourceRuntime {
    pub fn authentication_required(&self) -> bool {
        self.last_error
            .as_deref()
            .is_some_and(|error| error.starts_with("authentication is required"))
    }
}

#[derive(Clone, Debug)]
pub struct StoredWork {
    pub entry: WorkEntry,
    pub binding: WorkBinding,
    pub source_config: SourceConfig,
    pub connection: Connection,
    pub runtime: SourceRuntime,
    pub source_display: Value,
    pub navigation: Value,
}

#[derive(Clone, Debug)]
pub enum WorkMutation {
    SetPlanning(WorkPlanning),
    Snooze(DateTime<Utc>),
    Dismiss,
    SetPinned(bool),
    SetProgress(WorkProgress),
}

/// Coarse application/storage boundary. Transactional reconciliation remains an
/// implementation detail of the SQLite adapter rather than a repository graph.
pub trait WorkStore: Send + Sync {
    fn put_connection(&self, connection: &Connection) -> Result<()>;
    fn connections(&self) -> Result<Vec<Connection>>;
    fn put_source_config(&self, config: &SourceConfig) -> Result<()>;
    fn source_configs(&self) -> Result<Vec<SourceConfig>>;
    fn source_config(&self, id: &str) -> Result<SourceConfig>;
    fn source_runtime(&self, id: &str) -> Result<SourceRuntime>;
    fn record_sync_attempt(&self, id: &str, now: DateTime<Utc>) -> Result<()>;
    fn record_sync_failure(
        &self,
        id: &str,
        now: DateTime<Utc>,
        next_retry_at: Option<DateTime<Utc>>,
        error: &str,
    ) -> Result<()>;
    fn clear_sync_failure(&self, id: &str) -> Result<()>;
    fn apply_source_batch(
        &self,
        config: &SourceConfig,
        batch: &SourceBatch,
        now: DateTime<Utc>,
    ) -> Result<usize>;
    fn pending_source_changes(&self, limit: usize) -> Result<Vec<SourceChange>>;
    fn apply_projection(
        &self,
        change: &SourceChange,
        draft: &WorkDraft,
        projector_version: i32,
        now: DateTime<Utc>,
    ) -> Result<()>;
    fn stored_work(&self) -> Result<Vec<StoredWork>>;
    fn stored_work_by_id(&self, id: &str) -> Result<StoredWork>;
    fn mutate_work(&self, id: &str, mutation: WorkMutation, now: DateTime<Utc>) -> Result<()>;
}
