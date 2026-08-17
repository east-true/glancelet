mod clock;
mod connection;
mod navigation;
mod secrets;
mod sync;
mod widgets;
mod work;

pub use clock::*;
pub use connection::*;
pub use navigation::*;
pub use secrets::*;
pub use sync::*;
pub use widgets::*;
pub use work::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    domain::{
        ProviderId, SourceBatch, SourceChange, SourceIdentity, WorkBinding, WorkDraft, WorkEntry,
        WorkPlanning, WorkProgress,
    },
    extension::{Connection, SourceConfig},
    GlanceletError, Result,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFailureKind {
    AuthenticationRequired,
    ConfigurationRequired,
    RateLimited,
    TransientNetwork,
    ProviderFailure,
    Other,
}

impl From<&GlanceletError> for SourceFailureKind {
    fn from(error: &GlanceletError) -> Self {
        match error {
            GlanceletError::AuthenticationRequired(_) => Self::AuthenticationRequired,
            GlanceletError::ConfigurationRequired(_) => Self::ConfigurationRequired,
            GlanceletError::RateLimited { .. } => Self::RateLimited,
            GlanceletError::TransientNetwork(_) => Self::TransientNetwork,
            GlanceletError::ProviderFailure(_) => Self::ProviderFailure,
            _ => Self::Other,
        }
    }
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

    pub fn automatic_retry_blocked(&self) -> bool {
        matches!(
            self.failure_kind,
            Some(
                SourceFailureKind::AuthenticationRequired
                    | SourceFailureKind::ConfigurationRequired
            )
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionFailureState {
    pub failure_count: i64,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub quarantined_at: Option<DateTime<Utc>>,
}

impl ProjectionFailureState {
    pub fn quarantined(&self) -> bool {
        self.quarantined_at.is_some()
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
    #[allow(clippy::too_many_arguments)]
    fn record_projection_failure(
        &self,
        change_id: i64,
        projector_version: i32,
        failed_at: DateTime<Utc>,
        retry_base_seconds: i64,
        retry_max_seconds: i64,
        max_attempts: i64,
        error: &str,
    ) -> Result<ProjectionFailureState>;
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
    fn widget_layout(&self) -> Result<Vec<WidgetInstance>>;
    fn save_widget_layout(&self, widgets: &[WidgetInstance]) -> Result<()>;
    fn desktop_preferences(&self) -> Result<DesktopPreferences>;
    fn save_desktop_preferences(&self, preferences: &DesktopPreferences) -> Result<()>;
    fn stored_work_by_id(&self, id: &str) -> Result<StoredWork>;
    fn work_id_for_source_identity(
        &self,
        source_config_id: &str,
        identity: &SourceIdentity,
    ) -> Result<Option<String>>;
    fn mutate_work(&self, id: &str, mutation: WorkMutation, now: DateTime<Utc>) -> Result<()>;
    fn transition_local_progress(
        &self,
        id: &str,
        allowed_from: &[WorkProgress],
        to: WorkProgress,
        now: DateTime<Utc>,
    ) -> Result<()>;
}
