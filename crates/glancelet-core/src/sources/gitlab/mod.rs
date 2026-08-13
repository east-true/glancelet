mod client;
mod oauth;

pub use client::*;
pub use oauth::*;

use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{
    application::{Clock, SecretStore},
    domain::{
        ProgressAuthority, ProviderId, SourceBatch, SourceBatchKind, SourceChange, SourceEntity,
        SourceIdentity, SourceMutation, SourceRecord, SourceTypeId, WorkBindingMode, WorkDraft,
        WorkKind,
    },
    extension::{
        Connection, ProviderRegistration, SourceAdapter, SourceConfig, SourceDescriptor,
        SourceRegistration, WorkProjector,
    },
    GlanceletError, Result,
};

pub const PROVIDER_ID: &str = "gitlab";
pub const SOURCE_TYPE: &str = "gitlab.todos";
pub const DEFAULT_SYNC_INTERVAL_SECONDS: i64 = 300;

pub fn credential_key(connection_id: &str) -> String {
    format!("gitlab:{connection_id}")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitlabTodoSettings {
    pub instance_origin: String,
}

pub fn registration(
    client: Arc<GitlabApiClient>,
    tokens: Arc<GitlabTokenProvider>,
) -> ProviderRegistration {
    ProviderRegistration {
        provider_id: ProviderId(PROVIDER_ID.into()),
        display_name: "GitLab".into(),
        sources: vec![SourceRegistration {
            descriptor: SourceDescriptor {
                source_type_id: SourceTypeId(SOURCE_TYPE.into()),
                display_name: "GitLab To-Dos".into(),
                description: "Pending GitLab items that need your attention".into(),
            },
            adapter: Arc::new(GitlabTodosAdapter { client, tokens }),
            projector: Arc::new(GitlabTodoProjector),
        }],
    }
}

pub struct GitlabTokenProvider {
    client_id: String,
    client: Arc<GitlabApiClient>,
    secrets: Arc<dyn SecretStore>,
    clock: Arc<dyn Clock>,
    locks: std::sync::Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

impl GitlabTokenProvider {
    pub fn new(
        client_id: impl Into<String>,
        client: Arc<GitlabApiClient>,
        secrets: Arc<dyn SecretStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client,
            secrets,
            clock,
            locks: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn save(&self, connection_id: &str, credential: &GitlabCredential) -> Result<()> {
        let encoded = serde_json::to_string(credential).map_err(|_| {
            GlanceletError::SecretStoreUnavailable("cannot encode GitLab credential".into())
        })?;
        self.secrets.set(&credential_key(connection_id), &encoded)
    }

    pub fn delete(&self, connection_id: &str) -> Result<()> {
        self.secrets.delete(&credential_key(connection_id))
    }

    pub async fn access(
        &self,
        connection_id: &str,
        instance: &GitlabInstance,
    ) -> Result<GitlabAuth> {
        let key = credential_key(connection_id);
        let lock = {
            let mut locks = self.locks.lock().expect("GitLab token lock map poisoned");
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(Mutex::new(()));
                locks.insert(key.clone(), Arc::downgrade(&lock));
                lock
            }
        };
        let _guard = lock.lock().await;
        let raw = self.secrets.get(&key)?.ok_or_else(|| {
            GlanceletError::AuthenticationRequired("GitLab credential is missing".into())
        })?;
        let credential: GitlabCredential = serde_json::from_str(&raw).map_err(|_| {
            GlanceletError::AuthenticationRequired("GitLab credential is invalid".into())
        })?;
        let should_refresh = credential
            .expires_at()
            .is_some_and(|expiry| expiry <= self.clock.now() + chrono::Duration::minutes(5));
        if !should_refresh {
            return Ok(credential.auth());
        }
        let refresh_token = credential.refresh_token().ok_or_else(|| {
            GlanceletError::AuthenticationRequired(
                "GitLab OAuth token expired; reconnect this account".into(),
            )
        })?;
        if self.client_id.trim().is_empty() {
            return Err(GlanceletError::AuthenticationRequired(
                "GitLab client ID is required to refresh this connection".into(),
            ));
        }
        let replacement = self
            .client
            .refresh_token(instance, &self.client_id, refresh_token)
            .await?;
        let auth = replacement.auth();
        self.save(connection_id, &replacement)?;
        Ok(auth)
    }
}

pub fn matches_source_config(config: &SourceConfig, connection_id: &str) -> bool {
    config.connection_id == connection_id && config.source_type_id.0 == SOURCE_TYPE
}

pub fn matches_connection(
    connection: &Connection,
    instance: &GitlabInstance,
    user_id: &str,
) -> bool {
    connection.provider_id.0 == PROVIDER_ID
        && connection.config["instance_origin"] == instance.origin()
        && connection.config["user_id"] == user_id
}

struct GitlabTodosAdapter {
    client: Arc<GitlabApiClient>,
    tokens: Arc<GitlabTokenProvider>,
}

#[async_trait]
impl SourceAdapter for GitlabTodosAdapter {
    async fn fetch(
        &self,
        config: &SourceConfig,
        _checkpoint: Option<Value>,
    ) -> Result<SourceBatch> {
        let settings: GitlabTodoSettings = serde_json::from_value(config.settings.clone())
            .map_err(|_| {
                GlanceletError::ConfigurationRequired("invalid GitLab To-Dos settings".into())
            })?;
        let instance = GitlabInstance::parse(&settings.instance_origin)?;
        let auth = self.tokens.access(&config.connection_id, &instance).await?;
        let mut records = self
            .client
            .todos(&instance, &auth)
            .await?
            .into_iter()
            .map(|todo| todo_record(&self.client, &instance, todo))
            .collect::<Result<Vec<_>>>()?;
        records.sort_by(|left, right| left.identity.external_id.cmp(&right.identity.external_id));
        Ok(SourceBatch {
            kind: SourceBatchKind::FullSnapshot,
            mutations: records.into_iter().map(SourceMutation::Upsert).collect(),
            next_checkpoint: None,
        })
    }
}

struct GitlabTodoProjector;

impl WorkProjector for GitlabTodoProjector {
    fn project(&self, entity: &SourceEntity, _change: &SourceChange) -> Result<WorkDraft> {
        let mut dimensions = serde_json::Map::new();
        if let Some(project) = entity.metadata.get("project") {
            dimensions.insert("gitlab.project".into(), project.clone());
        }
        if let Some(action) = entity.metadata.get("action") {
            dimensions.insert("gitlab.action".into(), action.clone());
        }
        if let Some(target_type) = entity.metadata.get("target_type") {
            dimensions.insert("gitlab.target_type".into(), target_type.clone());
        }
        Ok(WorkDraft {
            kind: WorkKind::Action,
            title: entity.title.clone(),
            summary: None,
            priority: None,
            progress: None,
            start: None,
            end: None,
            due: None,
            dimensions: Value::Object(dimensions),
            facets: json!({}),
            binding_mode: WorkBindingMode::Mirror,
            progress_authority: ProgressAuthority::None,
        })
    }
}

fn todo_record(
    client: &GitlabApiClient,
    instance: &GitlabInstance,
    todo: GitlabTodo,
) -> Result<SourceRecord> {
    let title = todo
        .target_title
        .as_deref()
        .map(|title| normalized_title(title, ""))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| format!("GitLab {} to-do", display_target_type(&todo.target_type)));
    let navigation_url = client.navigation_url(instance, &todo.target_url)?;
    let metadata = json!({
        "project": todo.project_path,
        "action": todo.action_name,
        "target_type": todo.target_type,
        "created_at": todo.created_at,
        "source_updated": todo.updated_at,
    });
    let navigation = json!({ "web_url": navigation_url });
    let revision = revision(&title, &metadata, &navigation)?;
    Ok(SourceRecord {
        identity: SourceIdentity {
            entity_type: "gitlab.todo".into(),
            external_id: todo.id.to_string(),
        },
        title,
        revision,
        display: json!({
            "project": metadata["project"],
            "action": action_label(metadata["action"].as_str().unwrap_or("other")),
        }),
        metadata,
        navigation,
    })
}

fn normalized_title(value: &str, fallback: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        fallback.into()
    } else {
        collapsed.chars().take(240).collect()
    }
}

fn display_target_type(value: &str) -> String {
    value
        .chars()
        .enumerate()
        .flat_map(|(index, character)| {
            if index > 0 && character.is_ascii_uppercase() {
                vec![' ', character.to_ascii_lowercase()]
            } else {
                vec![character.to_ascii_lowercase()]
            }
        })
        .collect()
}

fn action_label(value: &str) -> String {
    value
        .split('_')
        .enumerate()
        .map(|(index, word)| {
            if index == 0 {
                let mut characters = word.chars();
                characters
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                    .unwrap_or_default()
            } else {
                word.into()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn revision(title: &str, metadata: &Value, navigation: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(&json!({
        "title": title,
        "metadata": metadata,
        "navigation": navigation,
    }))
    .map_err(|_| GlanceletError::Source("cannot normalize GitLab source record".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
}
