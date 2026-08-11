use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    domain::{ProviderId, SourceBatch, SourceChange, SourceEntity, SourceTypeId, WorkDraft},
    GlanceletError, Result,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub provider_id: ProviderId,
    pub display_name: String,
    #[serde(default)]
    pub config: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceConfig {
    pub id: String,
    pub connection_id: String,
    pub source_type_id: SourceTypeId,
    pub display_name: String,
    /// A reversible pause. Removed configs are never active even if this is true.
    pub enabled: bool,
    /// History-preserving removal. Re-adding restores this SourceConfig identity.
    #[serde(default)]
    pub removed_at: Option<DateTime<Utc>>,
    pub expected_sync_interval_seconds: i64,
    #[serde(default)]
    pub settings: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceDescriptor {
    pub source_type_id: SourceTypeId,
    pub display_name: String,
    pub description: String,
}

#[async_trait]
pub trait SourceAdapter: Send + Sync {
    /// A paginated implementation must return an error unless every page was collected.
    async fn fetch(&self, config: &SourceConfig, checkpoint: Option<Value>) -> Result<SourceBatch>;
}

pub trait WorkProjector: Send + Sync {
    fn version(&self) -> i32 {
        1
    }

    fn project(&self, entity: &SourceEntity, change: &SourceChange) -> Result<WorkDraft>;
}

pub struct SourceRegistration {
    pub descriptor: SourceDescriptor,
    pub adapter: Arc<dyn SourceAdapter>,
    pub projector: Arc<dyn WorkProjector>,
}

pub struct ProviderRegistration {
    pub provider_id: ProviderId,
    pub display_name: String,
    pub sources: Vec<SourceRegistration>,
}

struct RegisteredSource {
    provider_id: ProviderId,
    provider_display_name: String,
    descriptor: SourceDescriptor,
    adapter: Arc<dyn SourceAdapter>,
    projector: Arc<dyn WorkProjector>,
}

#[derive(Default)]
pub struct ExtensionRegistry {
    sources: HashMap<SourceTypeId, RegisteredSource>,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: ProviderRegistration) -> Result<()> {
        for source in provider.sources {
            let id = source.descriptor.source_type_id.clone();
            if self.sources.contains_key(&id) {
                return Err(GlanceletError::InvalidOperation(format!(
                    "source type '{}' is already registered",
                    id.0
                )));
            }
            self.sources.insert(
                id,
                RegisteredSource {
                    provider_id: provider.provider_id.clone(),
                    provider_display_name: provider.display_name.clone(),
                    descriptor: source.descriptor,
                    adapter: source.adapter,
                    projector: source.projector,
                },
            );
        }
        Ok(())
    }

    pub fn adapter(&self, id: &SourceTypeId) -> Result<Arc<dyn SourceAdapter>> {
        self.sources
            .get(id)
            .map(|source| Arc::clone(&source.adapter))
            .ok_or_else(|| GlanceletError::UnknownSource(id.0.clone()))
    }

    pub fn projector(&self, id: &SourceTypeId) -> Result<Arc<dyn WorkProjector>> {
        self.sources
            .get(id)
            .map(|source| Arc::clone(&source.projector))
            .ok_or_else(|| GlanceletError::UnknownSource(id.0.clone()))
    }

    pub fn display_metadata(&self, id: &SourceTypeId) -> Result<SourceDisplayMetadata> {
        let source = self
            .sources
            .get(id)
            .ok_or_else(|| GlanceletError::UnknownSource(id.0.clone()))?;
        Ok(SourceDisplayMetadata {
            provider_id: source.provider_id.clone(),
            provider_name: source.provider_display_name.clone(),
            source_name: source.descriptor.display_name.clone(),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceDisplayMetadata {
    pub provider_id: ProviderId,
    pub provider_name: String,
    pub source_name: String,
}
