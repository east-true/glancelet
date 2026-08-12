mod client;

pub use client::*;

use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    application::SecretStore,
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

pub const PROVIDER_ID: &str = "notion";
pub const SOURCE_TYPE: &str = "notion.data_source_tasks";
pub const DEFAULT_SYNC_INTERVAL_SECONDS: i64 = 300;

pub fn credential_key(connection_id: &str) -> String {
    format!("notion:{connection_id}")
}

pub struct NotionTokenProvider {
    secrets: Arc<dyn SecretStore>,
}

impl NotionTokenProvider {
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self { secrets }
    }

    pub fn save(&self, connection_id: &str, token: &str) -> Result<()> {
        self.secrets.set(&credential_key(connection_id), token)
    }

    pub fn delete(&self, connection_id: &str) -> Result<()> {
        self.secrets.delete(&credential_key(connection_id))
    }

    pub fn token(&self, connection_id: &str) -> Result<String> {
        self.secrets
            .get(&credential_key(connection_id))?
            .ok_or_else(|| {
                GlanceletError::AuthenticationRequired("Notion credential is missing".into())
            })
    }
}

pub fn registration(
    client: Arc<NotionApiClient>,
    tokens: Arc<NotionTokenProvider>,
) -> ProviderRegistration {
    ProviderRegistration {
        provider_id: ProviderId(PROVIDER_ID.into()),
        display_name: "Notion".into(),
        sources: vec![SourceRegistration {
            descriptor: SourceDescriptor {
                source_type_id: SourceTypeId(SOURCE_TYPE.into()),
                display_name: "Data Source Tasks".into(),
                description: "Project Notion data source pages as tasks".into(),
            },
            adapter: Arc::new(NotionTaskAdapter { client, tokens }),
            projector: Arc::new(NotionTaskProjector),
        }],
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionPropertyMapping {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionTaskProperties {
    pub title: NotionPropertyMapping,
    pub assignee: Option<NotionPropertyMapping>,
    pub status: Option<NotionPropertyMapping>,
    pub due: Option<NotionPropertyMapping>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionSourceSettings {
    pub data_source_id: String,
    pub data_source_name: String,
    pub properties: NotionTaskProperties,
    #[serde(default)]
    pub only_assigned_to_me: bool,
    #[serde(default)]
    pub active_status_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionPreviewRow {
    pub external_id: String,
    pub title: String,
    pub status: Option<String>,
    pub due: Option<TemporalValue>,
}

pub fn matches_source_config(
    config: &SourceConfig,
    connection_id: &str,
    data_source_id: &str,
) -> bool {
    config.connection_id == connection_id
        && config.source_type_id.0 == SOURCE_TYPE
        && config.settings["dataSourceId"].as_str() == Some(data_source_id)
}

pub fn validate_settings(schema: &NotionDataSource, settings: &NotionSourceSettings) -> Result<()> {
    if schema.id != settings.data_source_id {
        return Err(GlanceletError::InvalidOperation(
            "Notion data source identity changed during configuration".into(),
        ));
    }
    validate_mapping(schema, &settings.properties.title, "title", "Title")?;
    if settings.only_assigned_to_me && settings.properties.assignee.is_none() {
        return Err(GlanceletError::InvalidOperation(
            "assigned-to-me filtering requires a mapped Notion assignee property".into(),
        ));
    }
    if let Some(mapping) = settings.properties.assignee.as_ref() {
        validate_mapping(schema, mapping, "people", "Assignee")?;
    }
    if let Some(mapping) = settings.properties.status.as_ref() {
        validate_mapping(schema, mapping, "status", "Status")?;
        if settings.active_status_ids.is_empty() {
            return Err(GlanceletError::InvalidOperation(
                "select at least one active Notion status".into(),
            ));
        }
        let status = schema
            .property(&mapping.id)
            .and_then(|property| property.status.as_ref())
            .expect("validated status property has status schema");
        if settings
            .active_status_ids
            .iter()
            .any(|id| !status.options.iter().any(|option| &option.id == id))
        {
            return Err(client::needs_configuration(
                "Status",
                "contains a removed option",
            ));
        }
    } else if !settings.active_status_ids.is_empty() {
        return Err(GlanceletError::InvalidOperation(
            "active statuses require a mapped Notion status property".into(),
        ));
    }
    if let Some(mapping) = settings.properties.due.as_ref() {
        validate_mapping(schema, mapping, "date", "Due")?;
    }
    Ok(())
}

pub async fn preview(
    client: &NotionApiClient,
    token: &str,
    settings: &NotionSourceSettings,
    limit: usize,
) -> Result<Vec<NotionPreviewRow>> {
    let schema = client
        .retrieve_data_source(token, &settings.data_source_id)
        .await?;
    validate_settings(&schema, settings)?;
    let pages = client.query_pages(token, settings, &schema).await?;
    pages
        .iter()
        .take(limit)
        .map(|page| preview_row(page, settings))
        .collect()
}

struct NotionTaskAdapter {
    client: Arc<NotionApiClient>,
    tokens: Arc<NotionTokenProvider>,
}

#[async_trait]
impl SourceAdapter for NotionTaskAdapter {
    async fn fetch(
        &self,
        config: &SourceConfig,
        _checkpoint: Option<Value>,
    ) -> Result<SourceBatch> {
        let settings: NotionSourceSettings = serde_json::from_value(config.settings.clone())
            .map_err(|_| GlanceletError::Source("invalid Notion source settings".into()))?;
        let token = self.tokens.token(&config.connection_id)?;
        let schema = self
            .client
            .retrieve_data_source(&token, &settings.data_source_id)
            .await?;
        validate_settings(&schema, &settings)?;
        let pages = self.client.query_pages(&token, &settings, &schema).await?;
        let mutations = pages
            .iter()
            .map(|page| source_record(page, &settings, &schema).map(SourceMutation::Upsert))
            .collect::<Result<Vec<_>>>()?;
        Ok(SourceBatch {
            kind: SourceBatchKind::FullSnapshot,
            mutations,
            next_checkpoint: None,
        })
    }
}

struct NotionTaskProjector;

impl WorkProjector for NotionTaskProjector {
    fn project(&self, entity: &SourceEntity, _change: &SourceChange) -> Result<WorkDraft> {
        let progress = entity
            .metadata
            .get("progress")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| GlanceletError::Source("invalid Notion progress metadata".into()))?
            .unwrap_or(WorkProgress::Todo);
        let due = entity
            .metadata
            .get("due")
            .filter(|value| !value.is_null())
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| GlanceletError::Source("invalid Notion due metadata".into()))?;
        Ok(WorkDraft {
            kind: WorkKind::Action,
            title: entity.title.clone(),
            summary: None,
            priority: None,
            progress: Some(progress),
            start: None,
            end: None,
            due,
            dimensions: json!({
                "notion.data_source": entity.metadata.get("data_source_id")
                    .cloned().unwrap_or(Value::Null)
            }),
            facets: json!({}),
            binding_mode: WorkBindingMode::Mirror,
            progress_authority: ProgressAuthority::External,
        })
    }
}

fn source_record(
    page: &NotionPage,
    settings: &NotionSourceSettings,
    schema: &NotionDataSource,
) -> Result<SourceRecord> {
    let preview = preview_row(page, settings)?;
    let progress = progress_for_page(page, settings, schema);
    let metadata = json!({
        "data_source_id": settings.data_source_id,
        "status": preview.status,
        "progress": progress,
        "due": preview.due,
        "source_updated": page.last_edited_time,
    });
    let navigation = json!({ "web_url": page.url });
    let revision = revision(&preview.title, &metadata, &navigation);
    Ok(SourceRecord {
        identity: SourceIdentity {
            entity_type: "notion.data_source.page".into(),
            external_id: page.id.clone(),
        },
        title: preview.title,
        revision,
        display: json!({ "data_source": settings.data_source_name }),
        metadata,
        navigation,
    })
}

fn preview_row(page: &NotionPage, settings: &NotionSourceSettings) -> Result<NotionPreviewRow> {
    let title_property = page
        .property(&settings.properties.title.id)
        .ok_or_else(|| client::needs_configuration("Title", "is absent from a query result"))?;
    if title_property.kind != "title" {
        return Err(client::needs_configuration(
            "Title",
            "is no longer a title property",
        ));
    }
    let title = client::plain_text(&title_property.title, "Untitled");
    let status = settings
        .properties
        .status
        .as_ref()
        .and_then(|mapping| page.property(&mapping.id))
        .and_then(|property| property.status.as_ref())
        .map(|status| status.name.clone());
    let due = settings
        .properties
        .due
        .as_ref()
        .and_then(|mapping| page.property(&mapping.id))
        .and_then(|property| property.date.as_ref())
        .map(temporal_value)
        .transpose()?;
    Ok(NotionPreviewRow {
        external_id: page.id.clone(),
        title,
        status,
        due,
    })
}

fn progress_for_page(
    page: &NotionPage,
    settings: &NotionSourceSettings,
    schema: &NotionDataSource,
) -> WorkProgress {
    let Some(mapping) = settings.properties.status.as_ref() else {
        return WorkProgress::Todo;
    };
    let Some(status_id) = page
        .property(&mapping.id)
        .and_then(|property| property.status.as_ref())
        .map(|status| status.id.as_str())
    else {
        return WorkProgress::Todo;
    };
    let group_index = schema
        .property(&mapping.id)
        .and_then(|property| property.status.as_ref())
        .and_then(|status| {
            // Notion exposes the fixed status categories in To-do, In progress,
            // Complete order, but only returns their display names and memberships.
            status
                .groups
                .iter()
                .position(|group| group.option_ids.iter().any(|id| id == status_id))
        });
    match group_index {
        Some(1) => WorkProgress::Doing,
        Some(2..) => WorkProgress::Done,
        _ => WorkProgress::Todo,
    }
}

fn temporal_value(value: &NotionPageDate) -> Result<TemporalValue> {
    if let Ok(date) = NaiveDate::parse_from_str(&value.start, "%Y-%m-%d") {
        return Ok(TemporalValue::Date { date });
    }
    let instant = DateTime::parse_from_rfc3339(&value.start)
        .map_err(|_| GlanceletError::Source("Notion returned an invalid date value".into()))?
        .with_timezone(&Utc);
    Ok(TemporalValue::DateTime {
        instant,
        timezone: value.time_zone.clone(),
    })
}

fn revision(title: &str, metadata: &Value, navigation: &Value) -> String {
    let mut digest = Sha256::new();
    digest.update(title.as_bytes());
    digest.update(metadata.to_string().as_bytes());
    digest.update(navigation.to_string().as_bytes());
    URL_SAFE_NO_PAD.encode(digest.finalize())
}
