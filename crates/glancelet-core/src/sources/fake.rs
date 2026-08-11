use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{
    domain::{
        ProgressAuthority, ProviderId, SourceBatch, SourceBatchKind, SourceChange, SourceEntity,
        SourceIdentity, SourceMutation, SourceRecord, SourceTypeId, TemporalValue, WorkBindingMode,
        WorkDraft, WorkKind, WorkProgress,
    },
    extension::{
        ProviderRegistration, SourceAdapter, SourceConfig, SourceDescriptor, SourceRegistration,
        WorkProjector,
    },
    GlanceletError, Result,
};

pub const MIRROR_SOURCE_TYPE: &str = "dev.glancelet.fake-mirror";
pub const CAPTURE_SOURCE_TYPE: &str = "dev.glancelet.fake-capture";

pub fn registration() -> ProviderRegistration {
    ProviderRegistration {
        provider_id: ProviderId("dev.glancelet.fake".into()),
        display_name: "Fake Sources".into(),
        sources: vec![
            SourceRegistration {
                descriptor: SourceDescriptor {
                    source_type_id: SourceTypeId(MIRROR_SOURCE_TYPE.into()),
                    display_name: "Fake Mirror".into(),
                    description: "Deterministic externally-owned work for Phase 0".into(),
                },
                adapter: Arc::new(SettingsAdapter),
                projector: Arc::new(FakeProjector {
                    mode: WorkBindingMode::Mirror,
                    progress_authority: ProgressAuthority::External,
                }),
            },
            SourceRegistration {
                descriptor: SourceDescriptor {
                    source_type_id: SourceTypeId(CAPTURE_SOURCE_TYPE.into()),
                    display_name: "Fake Capture".into(),
                    description: "Deterministic locally-completable captures for Phase 0".into(),
                },
                adapter: Arc::new(SettingsAdapter),
                projector: Arc::new(FakeProjector {
                    mode: WorkBindingMode::Capture,
                    progress_authority: ProgressAuthority::Local,
                }),
            },
        ],
    }
}

struct SettingsAdapter;

#[async_trait]
impl SourceAdapter for SettingsAdapter {
    async fn fetch(
        &self,
        config: &SourceConfig,
        _checkpoint: Option<Value>,
    ) -> Result<SourceBatch> {
        if config
            .settings
            .get("fail")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(GlanceletError::Source(
                "fake source was configured to fail".into(),
            ));
        }

        let kind = match config
            .settings
            .get("batch_kind")
            .and_then(Value::as_str)
            .unwrap_or("full_snapshot")
        {
            "delta" => SourceBatchKind::Delta,
            "full_snapshot" => SourceBatchKind::FullSnapshot,
            value => {
                return Err(GlanceletError::Source(format!(
                    "unsupported fake batch kind: {value}"
                )))
            }
        };
        let records: Vec<SourceRecord> = serde_json::from_value(
            config
                .settings
                .get("records")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map_err(|error| GlanceletError::Source(error.to_string()))?;
        let deactivated: Vec<SourceIdentity> = serde_json::from_value(
            config
                .settings
                .get("deactivated")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map_err(|error| GlanceletError::Source(error.to_string()))?;

        let mutations = records
            .into_iter()
            .map(SourceMutation::Upsert)
            .chain(deactivated.into_iter().map(SourceMutation::Deactivate))
            .collect();
        Ok(SourceBatch {
            kind,
            mutations,
            next_checkpoint: config.settings.get("checkpoint").cloned(),
        })
    }
}

struct FakeProjector {
    mode: WorkBindingMode,
    progress_authority: ProgressAuthority,
}

impl WorkProjector for FakeProjector {
    fn project(&self, entity: &SourceEntity, _change: &SourceChange) -> Result<WorkDraft> {
        let kind = entity
            .metadata
            .get("kind")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| GlanceletError::Source(error.to_string()))?
            .unwrap_or(WorkKind::Action);
        let external_progress = entity
            .metadata
            .get("progress")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| GlanceletError::Source(error.to_string()))?;
        let temporal = |field: &str| -> Result<Option<TemporalValue>> {
            entity
                .metadata
                .get(field)
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| GlanceletError::Source(error.to_string()))
        };

        Ok(WorkDraft {
            kind,
            title: entity.title.clone(),
            summary: entity
                .metadata
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_owned),
            priority: entity
                .metadata
                .get("priority")
                .and_then(Value::as_i64)
                .map(|value| value as i32),
            progress: match self.progress_authority {
                ProgressAuthority::Local => Some(WorkProgress::Todo),
                ProgressAuthority::External => external_progress,
                ProgressAuthority::None => None,
            },
            start: temporal("start")?,
            end: temporal("end")?,
            due: temporal("due")?,
            dimensions: entity
                .metadata
                .get("dimensions")
                .cloned()
                .unwrap_or_else(|| json!({})),
            facets: entity
                .metadata
                .get("facets")
                .cloned()
                .unwrap_or_else(|| json!({})),
            binding_mode: self.mode,
            progress_authority: self.progress_authority,
        })
    }
}
