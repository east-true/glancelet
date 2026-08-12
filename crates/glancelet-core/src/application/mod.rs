mod clock;
mod connection;
mod navigation;
mod secrets;
mod sync;
mod work;

pub use clock::*;
pub use connection::*;
pub use navigation::*;
pub use secrets::*;
pub use sync::*;
pub use work::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    domain::{
        ProviderId, SourceBatch, SourceChange, WorkBinding, WorkDraft, WorkEntry, WorkPlanning,
        WorkProgress,
    },
    extension::{Connection, SourceConfig},
    Result,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFailureKind {
    AuthenticationRequired,
    RateLimited,
    Other,
}

#[derive(Clone, Debug)]
pub struct SourceRuntime {
    pub checkpoint: Option<Value>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub next_sync_at: Option<DateTime<Utc>>,
    pub failure_count: i64,
    pub last_error: Option<String>,
    pub config_revision: i64,
    pub failure_kind: Option<SourceFailureKind>,
}

impl SourceRuntime {
    pub fn authentication_required(&self) -> bool {
        self.failure_kind == Some(SourceFailureKind::AuthenticationRequired)
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
}

/// Coarse application/storage boundary. Transactional reconciliation remains an
/// implementation detail of the SQLite adapter rather than a repository graph.
pub trait WorkStore: Send + Sync {
    fn put_connection(&self, connection: &Connection) -> Result<()>;
    fn connect_connection(
        &self,
        connection: &Connection,
        source_configs: &[SourceConfig],
    ) -> Result<()>;
    fn connections(&self) -> Result<Vec<Connection>>;
    fn disconnect_connection(&self, connection_id: &str, provider_id: &ProviderId) -> Result<()>;
    fn resume_connection(&self, connection_id: &str) -> Result<()>;
    fn put_source_config(&self, config: &SourceConfig) -> Result<()>;
    fn put_source_configs(&self, configs: &[SourceConfig]) -> Result<()>;
    fn source_configs(&self) -> Result<Vec<SourceConfig>>;
    fn source_config(&self, id: &str) -> Result<SourceConfig>;
    fn source_runtime(&self, id: &str) -> Result<SourceRuntime>;
    fn source_sync_state(&self, id: &str) -> Result<(SourceConfig, SourceRuntime)>;
    fn record_sync_attempt(
        &self,
        id: &str,
        expected_config_revision: i64,
        now: DateTime<Utc>,
    ) -> Result<()>;
    fn record_sync_failure(
        &self,
        id: &str,
        expected_config_revision: i64,
        now: DateTime<Utc>,
        next_retry_at: Option<DateTime<Utc>>,
        kind: SourceFailureKind,
        error: &str,
    ) -> Result<()>;
    fn clear_sync_failure(&self, id: &str) -> Result<()>;
    fn apply_source_batch(
        &self,
        config: &SourceConfig,
        expected_config_revision: i64,
        batch: &SourceBatch,
        now: DateTime<Utc>,
    ) -> Result<usize>;
    fn pending_source_changes_at(
        &self,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<Vec<SourceChange>>;
    fn pending_source_changes(&self, limit: usize) -> Result<Vec<SourceChange>> {
        self.pending_source_changes_at(limit, Utc::now())
    }
    fn record_projection_failure(
        &self,
        change_id: i64,
        next_retry_at: DateTime<Utc>,
        error: &str,
    ) -> Result<()>;
    fn enqueue_reprojections(
        &self,
        source_config_id: &str,
        projector_version: i32,
        now: DateTime<Utc>,
    ) -> Result<usize>;
    fn apply_projection(
        &self,
        change: &SourceChange,
        draft: &WorkDraft,
        projector_version: i32,
        now: DateTime<Utc>,
    ) -> Result<()>;
    fn stored_work(&self) -> Result<Vec<StoredWork>>;
    fn dashboard_work(&self, now: DateTime<Utc>) -> Result<Vec<StoredWork>>;
    fn stored_work_by_id(&self, id: &str) -> Result<StoredWork>;
    fn mutate_work(&self, id: &str, mutation: WorkMutation, now: DateTime<Utc>) -> Result<()>;
    fn transition_local_progress(
        &self,
        id: &str,
        allowed_from: &[WorkProgress],
        to: WorkProgress,
        now: DateTime<Utc>,
    ) -> Result<()>;
}
