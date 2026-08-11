use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceTypeId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceBatchKind {
    Delta,
    FullSnapshot,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub entity_type: String,
    pub external_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceRecord {
    pub identity: SourceIdentity,
    pub title: String,
    pub revision: String,
    #[serde(default)]
    pub display: Value,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default)]
    pub navigation: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SourceMutation {
    Upsert(SourceRecord),
    Deactivate(SourceIdentity),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceBatch {
    pub kind: SourceBatchKind,
    pub mutations: Vec<SourceMutation>,
    pub next_checkpoint: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceEntity {
    pub id: String,
    pub source_config_id: String,
    pub identity: SourceIdentity,
    pub title: String,
    pub revision: String,
    pub active: bool,
    pub activation_seq: i64,
    pub display: Value,
    pub metadata: Value,
    pub navigation: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceChangeKind {
    Created,
    Updated,
    Deactivated,
    Reactivated,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceChange {
    pub id: i64,
    pub source_entity: SourceEntity,
    pub kind: SourceChangeKind,
    pub occurred_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
}
