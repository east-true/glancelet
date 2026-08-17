use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    application::WorkStore,
    domain::{
        ProgressAuthority, ProviderId, SourceBatch, SourceBatchKind, SourceChange, SourceEntity,
        SourceIdentity, SourceMutation, SourceRecord, SourceTypeId, WorkBindingMode, WorkDraft,
        WorkKind, WorkProgress,
    },
    extension::{
        Connection, ProviderRegistration, SourceAdapter, SourceConfig, SourceDescriptor,
        SourceRegistration, WorkProjector,
    },
    GlanceletError, Result,
};

pub const PROVIDER_ID: &str = "local";
pub const SOURCE_TYPE: &str = "local.manual";
pub const CONNECTION_ID: &str = "local-built-in";
pub const SOURCE_CONFIG_ID: &str = "local-manual";
pub const MAX_TITLE_LENGTH: usize = 240;
const MANUAL_SYNC_INTERVAL_SECONDS: i64 = 315_360_000;

pub fn registration() -> ProviderRegistration {
    ProviderRegistration {
        provider_id: ProviderId(PROVIDER_ID.into()),
        display_name: "Local".into(),
        sources: vec![SourceRegistration {
            descriptor: SourceDescriptor {
                source_type_id: SourceTypeId(SOURCE_TYPE.into()),
                display_name: "Manual Capture".into(),
                description: "Locally captured Actions owned by Glancelet".into(),
            },
            // Local acquisition enters through `ingest`; this inert adapter performs no I/O.
            // Keeping the registered source pull-safe preserves the existing extension contract.
            adapter: Arc::new(LocalManualAdapter),
            projector: Arc::new(LocalManualProjector),
        }],
    }
}

pub fn ingest(
    store: &dyn WorkStore,
    capture_id: &str,
    title: &str,
    now: DateTime<Utc>,
) -> Result<SourceIdentity> {
    Uuid::parse_str(capture_id)
        .map_err(|_| GlanceletError::InvalidOperation("invalid capture request id".into()))?;
    let title = title.trim();
    if title.is_empty() {
        return Err(GlanceletError::InvalidOperation(
            "capture title is required".into(),
        ));
    }
    if title.chars().count() > MAX_TITLE_LENGTH {
        return Err(GlanceletError::InvalidOperation(format!(
            "capture title must be at most {MAX_TITLE_LENGTH} characters"
        )));
    }

    ensure_source(store)?;
    let (config, runtime) = store.source_sync_state(SOURCE_CONFIG_ID)?;
    let identity = SourceIdentity {
        entity_type: "manual_capture".into(),
        external_id: capture_id.into(),
    };
    store.apply_source_batch(
        &config,
        runtime.config_revision,
        &SourceBatch {
            kind: SourceBatchKind::Delta,
            mutations: vec![SourceMutation::Upsert(SourceRecord {
                identity: identity.clone(),
                title: title.into(),
                revision: capture_id.into(),
                display: json!({}),
                metadata: json!({"kind": "action"}),
                navigation: json!({}),
            })],
            next_checkpoint: None,
        },
        now,
    )?;
    Ok(identity)
}

fn ensure_source(store: &dyn WorkStore) -> Result<()> {
    if let Some(existing) = store
        .source_configs()?
        .into_iter()
        .find(|config| config.id == SOURCE_CONFIG_ID)
    {
        if existing.source_type_id.0 != SOURCE_TYPE || existing.connection_id != CONNECTION_ID {
            return Err(GlanceletError::InvalidOperation(
                "the built-in manual source identity is already in use".into(),
            ));
        }
        if existing.enabled && existing.removed_at.is_none() {
            return Ok(());
        }
        store.put_source_config(&manual_source_config())?;
        return Ok(());
    }

    store.connect_connection(&manual_connection(), &[manual_source_config()])
}

fn manual_connection() -> Connection {
    Connection {
        id: CONNECTION_ID.into(),
        provider_id: ProviderId(PROVIDER_ID.into()),
        display_name: "Local".into(),
        config: json!({"built_in": true}),
    }
}

fn manual_source_config() -> SourceConfig {
    SourceConfig {
        id: SOURCE_CONFIG_ID.into(),
        connection_id: CONNECTION_ID.into(),
        source_type_id: SourceTypeId(SOURCE_TYPE.into()),
        display_name: "Manual Capture".into(),
        enabled: true,
        removed_at: None,
        expected_sync_interval_seconds: MANUAL_SYNC_INTERVAL_SECONDS,
        settings: json!({"built_in": true}),
    }
}

struct LocalManualAdapter;

#[async_trait]
impl SourceAdapter for LocalManualAdapter {
    async fn fetch(
        &self,
        _config: &SourceConfig,
        checkpoint: Option<Value>,
    ) -> Result<SourceBatch> {
        Ok(SourceBatch {
            kind: SourceBatchKind::Delta,
            mutations: Vec::new(),
            next_checkpoint: checkpoint,
        })
    }
}

struct LocalManualProjector;

impl WorkProjector for LocalManualProjector {
    fn project(&self, entity: &SourceEntity, _change: &SourceChange) -> Result<WorkDraft> {
        Ok(WorkDraft {
            kind: WorkKind::Action,
            title: entity.title.clone(),
            summary: None,
            priority: None,
            progress: Some(WorkProgress::Todo),
            start: None,
            end: None,
            due: None,
            dimensions: json!({}),
            facets: json!({}),
            binding_mode: WorkBindingMode::Capture,
            progress_authority: ProgressAuthority::Local,
        })
    }
}
