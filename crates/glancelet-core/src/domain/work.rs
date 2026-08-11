use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Action,
    Event,
    Attention,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkLifecycle {
    Active,
    Resolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkProgress {
    Todo,
    Doing,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "date", rename_all = "snake_case")]
pub enum WorkPlanning {
    Inbox,
    Backlog,
    Planned(NaiveDate),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalDisposition {
    Normal,
    Snoozed,
    Dismissed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TemporalValue {
    Date {
        date: NaiveDate,
    },
    DateTime {
        instant: DateTime<Utc>,
        timezone: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkBindingMode {
    Mirror,
    Capture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressAuthority {
    None,
    Local,
    External,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkDraft {
    pub kind: WorkKind,
    pub title: String,
    pub summary: Option<String>,
    pub priority: Option<i32>,
    pub progress: Option<WorkProgress>,
    pub start: Option<TemporalValue>,
    pub end: Option<TemporalValue>,
    pub due: Option<TemporalValue>,
    #[serde(default)]
    pub dimensions: Value,
    #[serde(default)]
    pub facets: Value,
    pub binding_mode: WorkBindingMode,
    pub progress_authority: ProgressAuthority,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkEntry {
    pub id: String,
    pub kind: WorkKind,
    pub title: String,
    pub summary: Option<String>,
    pub priority: Option<i32>,
    pub lifecycle: WorkLifecycle,
    pub progress: Option<WorkProgress>,
    pub planning: Option<WorkPlanning>,
    pub disposition: LocalDisposition,
    pub pinned: bool,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub start: Option<TemporalValue>,
    pub end: Option<TemporalValue>,
    pub due: Option<TemporalValue>,
    pub dimensions: Value,
    pub facets: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkBinding {
    pub source_entity_id: String,
    pub work_entry_id: String,
    pub mode: WorkBindingMode,
    pub progress_authority: ProgressAuthority,
    pub source_activation_seq: i64,
    pub projector_version: i32,
}
