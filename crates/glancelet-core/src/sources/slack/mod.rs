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
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{
    application::{Clock, SecretStore},
    domain::{
        ProgressAuthority, ProviderId, SourceBatch, SourceBatchKind, SourceChange, SourceEntity,
        SourceIdentity, SourceMutation, SourceRecord, SourceTypeId, WorkBindingMode, WorkDraft,
        WorkKind, WorkProgress,
    },
    extension::{
        ProviderRegistration, SourceAdapter, SourceConfig, SourceDescriptor, SourceRegistration,
        WorkProjector,
    },
    GlanceletError, Result,
};

pub const PROVIDER_ID: &str = "slack";
pub const SOURCE_TYPE: &str = "slack.reaction_capture";
pub const DEFAULT_REACTION: &str = "todo";

pub fn credential_key(connection_id: &str) -> String {
    format!("slack:{connection_id}")
}

pub fn registration(
    client: Arc<SlackApiClient>,
    tokens: Arc<SlackTokenProvider>,
) -> ProviderRegistration {
    ProviderRegistration {
        provider_id: ProviderId(PROVIDER_ID.into()),
        display_name: "Slack".into(),
        sources: vec![SourceRegistration {
            descriptor: SourceDescriptor {
                source_type_id: SourceTypeId(SOURCE_TYPE.into()),
                display_name: "Reaction Capture".into(),
                description: "Capture Slack messages reacted to by you".into(),
            },
            adapter: Arc::new(SlackReactionAdapter { client, tokens }),
            projector: Arc::new(SlackReactionProjector),
        }],
    }
}

pub struct SlackTokenProvider {
    client_id: String,
    client: Arc<SlackApiClient>,
    secrets: Arc<dyn SecretStore>,
    clock: Arc<dyn Clock>,
    locks: std::sync::Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

impl SlackTokenProvider {
    pub fn new(
        client_id: impl Into<String>,
        client: Arc<SlackApiClient>,
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

    pub fn save(&self, connection_id: &str, credential: &SlackCredential) -> Result<()> {
        let value = serde_json::to_string(credential).map_err(|_| {
            GlanceletError::SecretStoreUnavailable("cannot encode credential".into())
        })?;
        self.secrets.set(&credential_key(connection_id), &value)
    }

    pub fn delete(&self, connection_id: &str) -> Result<()> {
        self.secrets.delete(&credential_key(connection_id))
    }

    pub async fn access_token(&self, connection_id: &str) -> Result<String> {
        let key = credential_key(connection_id);
        let lock = {
            let mut locks = self.locks.lock().expect("Slack token lock map poisoned");
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
            GlanceletError::AuthenticationRequired("Slack credential is missing".into())
        })?;
        let credential: SlackCredential = serde_json::from_str(&raw).map_err(|_| {
            GlanceletError::AuthenticationRequired("Slack credential is invalid".into())
        })?;
        let should_refresh = credential
            .expires_at()
            .is_some_and(|expiry| expiry <= self.clock.now() + chrono::Duration::minutes(5));
        if !should_refresh {
            return Ok(credential.access_token().to_owned());
        }
        let refresh = credential
            .refresh_token()
            .ok_or_else(|| GlanceletError::AuthenticationRequired("Slack token expired".into()))?;
        if self.client_id.trim().is_empty() {
            return Err(GlanceletError::AuthenticationRequired(
                "Slack client ID is required to refresh this connection".into(),
            ));
        }
        let replacement = self
            .client
            .refresh_token(&self.client_id, refresh, self.clock.now())
            .await?;
        let token = replacement.access_token().to_owned();
        // Replace the complete bundle only after Slack has issued a valid new pair.
        self.save(connection_id, &replacement)?;
        Ok(token)
    }
}

struct SlackReactionAdapter {
    client: Arc<SlackApiClient>,
    tokens: Arc<SlackTokenProvider>,
}

#[async_trait]
impl SourceAdapter for SlackReactionAdapter {
    async fn fetch(
        &self,
        config: &SourceConfig,
        _checkpoint: Option<Value>,
    ) -> Result<SourceBatch> {
        let settings: SlackSourceSettings = serde_json::from_value(config.settings.clone())
            .map_err(|_| GlanceletError::Source("invalid Slack source settings".into()))?;
        let reaction_name = normalize_reaction_name(settings.reaction_name.as_deref())?;
        let token = self.tokens.access_token(&config.connection_id).await?;
        let mut cursor = None;
        let mut messages = HashMap::<String, (String, ReactionMessage)>::new();

        loop {
            let page = self
                .client
                .reactions_page(&token, cursor.as_deref())
                .await?;
            for item in page.items {
                if item.kind != "message" {
                    continue;
                }
                let (Some(channel), Some(message)) = (item.channel, item.message) else {
                    continue;
                };
                let Some(ts) = message.ts.as_deref() else {
                    continue;
                };
                let captured = message.reactions.iter().any(|reaction| {
                    reaction.name == reaction_name
                        && reaction.users.iter().any(|user| user == &settings.user_id)
                });
                if captured {
                    let identity = external_id(&settings.team_id, &channel, ts, &reaction_name);
                    messages.entry(identity).or_insert((channel, message));
                }
            }
            let next = page
                .response_metadata
                .map(|metadata| metadata.next_cursor)
                .unwrap_or_default();
            if next.is_empty() {
                break;
            }
            cursor = Some(next);
        }

        let mut messages = messages.into_iter().collect::<Vec<_>>();
        messages.sort_by(|a, b| a.0.cmp(&b.0));
        let mut mutations = Vec::with_capacity(messages.len());
        for (identity, (channel, message)) in messages {
            let message_ts = message
                .ts
                .as_deref()
                .expect("filtered message has timestamp");
            let permalink = self.client.permalink(&token, &channel, message_ts).await?;
            let title = normalize_title(&message.text);
            let metadata = json!({
                "team_id": settings.team_id.clone(),
                "channel_id": channel.clone(),
                "message_ts": message_ts,
                "thread_ts": message.thread_ts.clone(),
                "reaction_name": reaction_name.clone(),
            });
            let revision = revision(&title, &permalink, &metadata);
            mutations.push(SourceMutation::Upsert(SourceRecord {
                identity: SourceIdentity {
                    entity_type: "slack.message.reaction".into(),
                    external_id: identity,
                },
                title,
                revision,
                display: json!({ "workspace": settings.team_name.clone() }),
                metadata,
                navigation: json!({ "web_url": permalink }),
            }));
        }
        Ok(SourceBatch {
            kind: SourceBatchKind::FullSnapshot,
            mutations,
            next_checkpoint: None,
        })
    }
}

struct SlackReactionProjector;

impl WorkProjector for SlackReactionProjector {
    fn project(&self, entity: &SourceEntity, _change: &SourceChange) -> Result<WorkDraft> {
        let metadata = &entity.metadata;
        Ok(WorkDraft {
            kind: WorkKind::Action,
            title: entity.title.clone(),
            summary: None,
            priority: None,
            progress: Some(WorkProgress::Todo),
            start: None,
            end: None,
            due: None,
            dimensions: json!({
                "slack.workspace": metadata.get("team_id").cloned().unwrap_or(Value::Null)
            }),
            facets: json!({
                "slack.reaction_capture": {
                    "channel_id": metadata.get("channel_id").cloned().unwrap_or(Value::Null),
                    "message_ts": metadata.get("message_ts").cloned().unwrap_or(Value::Null),
                    "reaction_name": metadata.get("reaction_name").cloned().unwrap_or(Value::Null)
                }
            }),
            binding_mode: WorkBindingMode::Capture,
            progress_authority: ProgressAuthority::Local,
        })
    }
}

#[derive(Deserialize)]
struct SlackSourceSettings {
    team_id: String,
    #[serde(default = "default_team_name")]
    team_name: String,
    user_id: String,
    reaction_name: Option<String>,
}

fn default_team_name() -> String {
    "Slack workspace".into()
}

pub fn normalize_reaction_name(value: Option<&str>) -> Result<String> {
    let value = value.unwrap_or(DEFAULT_REACTION).trim().trim_matches(':');
    let valid = !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+' | b'-'));
    if !valid {
        return Err(GlanceletError::InvalidOperation(
            "reaction name must contain only letters, numbers, _, +, or -".into(),
        ));
    }
    Ok(value.to_owned())
}

pub fn external_id(team_id: &str, channel: &str, message_ts: &str, reaction: &str) -> String {
    format!("{team_id}/{channel}/{message_ts}/{reaction}")
}

fn normalize_title(text: &str) -> String {
    let decoded = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title = collapsed.chars().take(240).collect::<String>();
    if collapsed.chars().count() > 240 {
        title.push('…');
    }
    if title.is_empty() {
        "Slack message".into()
    } else {
        title
    }
}

fn revision(title: &str, permalink: &str, metadata: &Value) -> String {
    let mut digest = Sha256::new();
    digest.update(title.as_bytes());
    digest.update(permalink.as_bytes());
    digest.update(metadata.to_string().as_bytes());
    URL_SAFE_NO_PAD.encode(digest.finalize())
}
