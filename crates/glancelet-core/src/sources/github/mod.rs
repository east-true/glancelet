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
use url::Url;

use crate::{
    application::{Clock, SecretStore},
    domain::{
        ProgressAuthority, ProviderId, SourceBatch, SourceBatchKind, SourceChange, SourceEntity,
        SourceIdentity, SourceMutation, SourceRecord, SourceTypeId, WorkBindingMode, WorkDraft,
        WorkKind,
    },
    extension::{
        ProviderRegistration, SourceAdapter, SourceConfig, SourceDescriptor, SourceRegistration,
        WorkProjector,
    },
    GlanceletError, Result,
};

pub const PROVIDER_ID: &str = "github";
pub const REVIEW_REQUESTS_SOURCE_TYPE: &str = "github.review_requests";
pub const ASSIGNED_ISSUES_SOURCE_TYPE: &str = "github.assigned_issues";
pub const WORKFLOW_FAILURES_SOURCE_TYPE: &str = "github.workflow_failures";
pub const DEFAULT_SYNC_INTERVAL_SECONDS: i64 = 300;

pub fn credential_key(connection_id: &str) -> String {
    format!("github:{connection_id}")
}

pub fn registration(
    client: Arc<GithubApiClient>,
    tokens: Arc<GithubTokenProvider>,
) -> ProviderRegistration {
    ProviderRegistration {
        provider_id: ProviderId(PROVIDER_ID.into()),
        display_name: "GitHub".into(),
        sources: vec![
            SourceRegistration {
                descriptor: SourceDescriptor {
                    source_type_id: SourceTypeId(REVIEW_REQUESTS_SOURCE_TYPE.into()),
                    display_name: "Review Requests".into(),
                    description: "Open pull requests requesting your review".into(),
                },
                adapter: Arc::new(GithubReviewRequestsAdapter {
                    client: Arc::clone(&client),
                    tokens: Arc::clone(&tokens),
                }),
                projector: Arc::new(GithubProjector {
                    kind: WorkKind::Action,
                }),
            },
            SourceRegistration {
                descriptor: SourceDescriptor {
                    source_type_id: SourceTypeId(ASSIGNED_ISSUES_SOURCE_TYPE.into()),
                    display_name: "Assigned Issues".into(),
                    description: "Open issues assigned to you".into(),
                },
                adapter: Arc::new(GithubAssignedIssuesAdapter {
                    client: Arc::clone(&client),
                    tokens: Arc::clone(&tokens),
                }),
                projector: Arc::new(GithubProjector {
                    kind: WorkKind::Action,
                }),
            },
            SourceRegistration {
                descriptor: SourceDescriptor {
                    source_type_id: SourceTypeId(WORKFLOW_FAILURES_SOURCE_TYPE.into()),
                    display_name: "Workflow Failures".into(),
                    description: "Currently failing default-branch workflows".into(),
                },
                adapter: Arc::new(GithubWorkflowFailuresAdapter { client, tokens }),
                projector: Arc::new(GithubProjector {
                    kind: WorkKind::Attention,
                }),
            },
        ],
    }
}

pub struct GithubTokenProvider {
    client_id: String,
    client: Arc<GithubApiClient>,
    secrets: Arc<dyn SecretStore>,
    clock: Arc<dyn Clock>,
    locks: std::sync::Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

impl GithubTokenProvider {
    pub fn new(
        client_id: impl Into<String>,
        client: Arc<GithubApiClient>,
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

    pub fn save(&self, connection_id: &str, credential: &GithubCredential) -> Result<()> {
        let encoded = serde_json::to_string(credential).map_err(|_| {
            GlanceletError::SecretStoreUnavailable("cannot encode GitHub credential".into())
        })?;
        self.secrets.set(&credential_key(connection_id), &encoded)
    }

    pub fn delete(&self, connection_id: &str) -> Result<()> {
        self.secrets.delete(&credential_key(connection_id))
    }

    pub async fn access_token(&self, connection_id: &str) -> Result<String> {
        let key = credential_key(connection_id);
        let lock = {
            let mut locks = self.locks.lock().expect("GitHub token lock map poisoned");
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
            GlanceletError::AuthenticationRequired("GitHub credential is missing".into())
        })?;
        let credential: GithubCredential = serde_json::from_str(&raw).map_err(|_| {
            GlanceletError::AuthenticationRequired("GitHub credential is invalid".into())
        })?;
        let should_refresh = credential
            .expires_at()
            .is_some_and(|expiry| expiry <= self.clock.now() + chrono::Duration::minutes(5));
        if !should_refresh {
            return Ok(credential.access_token().to_owned());
        }
        let refresh = credential.refresh_token().ok_or_else(|| {
            GlanceletError::AuthenticationRequired("GitHub user access token expired".into())
        })?;
        if credential
            .refresh_token_expires_at()
            .is_some_and(|expiry| expiry <= self.clock.now())
        {
            return Err(GlanceletError::AuthenticationRequired(
                "GitHub refresh token expired".into(),
            ));
        }
        if self.client_id.trim().is_empty() {
            return Err(GlanceletError::AuthenticationRequired(
                "GitHub App client ID is required to refresh this connection".into(),
            ));
        }
        let replacement = self.client.refresh_token(&self.client_id, refresh).await?;
        let access_token = replacement.access_token().to_owned();
        self.save(connection_id, &replacement)?;
        Ok(access_token)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubWorkflowSettings {
    pub repository_id: u64,
    pub repository_node_id: String,
    pub repository: String,
    pub default_branch: String,
}

pub fn matches_global_source_config(
    config: &SourceConfig,
    connection_id: &str,
    source_type: &str,
) -> bool {
    config.connection_id == connection_id && config.source_type_id.0 == source_type
}

pub fn matches_workflow_source_config(
    config: &SourceConfig,
    connection_id: &str,
    repository_id: u64,
) -> bool {
    config.connection_id == connection_id
        && config.source_type_id.0 == WORKFLOW_FAILURES_SOURCE_TYPE
        && serde_json::from_value::<GithubWorkflowSettings>(config.settings.clone())
            .is_ok_and(|settings| settings.repository_id == repository_id)
}

struct GithubReviewRequestsAdapter {
    client: Arc<GithubApiClient>,
    tokens: Arc<GithubTokenProvider>,
}

#[async_trait]
impl SourceAdapter for GithubReviewRequestsAdapter {
    async fn fetch(
        &self,
        config: &SourceConfig,
        _checkpoint: Option<Value>,
    ) -> Result<SourceBatch> {
        let token = self.tokens.access_token(&config.connection_id).await?;
        let issues = self.client.review_requests(&token).await?;
        snapshot(
            issues
                .into_iter()
                .map(|issue| issue_record(issue, "pull_request")),
        )
    }
}

struct GithubAssignedIssuesAdapter {
    client: Arc<GithubApiClient>,
    tokens: Arc<GithubTokenProvider>,
}

#[async_trait]
impl SourceAdapter for GithubAssignedIssuesAdapter {
    async fn fetch(
        &self,
        config: &SourceConfig,
        _checkpoint: Option<Value>,
    ) -> Result<SourceBatch> {
        let token = self.tokens.access_token(&config.connection_id).await?;
        let issues = self.client.assigned_issues(&token).await?;
        snapshot(issues.into_iter().map(|issue| issue_record(issue, "issue")))
    }
}

struct GithubWorkflowFailuresAdapter {
    client: Arc<GithubApiClient>,
    tokens: Arc<GithubTokenProvider>,
}

#[async_trait]
impl SourceAdapter for GithubWorkflowFailuresAdapter {
    async fn fetch(
        &self,
        config: &SourceConfig,
        _checkpoint: Option<Value>,
    ) -> Result<SourceBatch> {
        let settings: GithubWorkflowSettings = serde_json::from_value(config.settings.clone())
            .map_err(|_| GlanceletError::Source("invalid GitHub workflow settings".into()))?;
        let repository = GithubRepository {
            id: settings.repository_id,
            node_id: settings.repository_node_id,
            full_name: settings.repository,
            default_branch: settings.default_branch,
        };
        let token = self.tokens.access_token(&config.connection_id).await?;
        let workflows = self.client.workflows(&token, &repository).await?;
        let mut records = Vec::new();
        for workflow in workflows {
            let Some(run) = self
                .client
                .latest_completed_run(&token, &repository, workflow.id)
                .await?
            else {
                continue;
            };
            let Some(conclusion) = run.conclusion.as_deref() else {
                continue;
            };
            if is_failure_conclusion(conclusion) {
                records.push(workflow_record(&repository, workflow, run)?);
            }
        }
        snapshot(records.into_iter().map(Ok))
    }
}

struct GithubProjector {
    kind: WorkKind,
}

impl WorkProjector for GithubProjector {
    fn project(&self, entity: &SourceEntity, _change: &SourceChange) -> Result<WorkDraft> {
        let mut dimensions = serde_json::Map::new();
        if let Some(repository) = entity.metadata.get("repository") {
            dimensions.insert("github.repository".into(), repository.clone());
        }
        if let Some(number) = entity.metadata.get("number") {
            dimensions.insert("github.number".into(), number.clone());
        }
        Ok(WorkDraft {
            kind: self.kind,
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

fn snapshot<I>(records: I) -> Result<SourceBatch>
where
    I: IntoIterator<Item = Result<SourceRecord>>,
{
    let mut records = records.into_iter().collect::<Result<Vec<_>>>()?;
    records.sort_by(|left, right| left.identity.external_id.cmp(&right.identity.external_id));
    Ok(SourceBatch {
        kind: SourceBatchKind::FullSnapshot,
        mutations: records.into_iter().map(SourceMutation::Upsert).collect(),
        next_checkpoint: None,
    })
}

fn issue_record(issue: GithubIssue, entity_type: &str) -> Result<SourceRecord> {
    if issue.node_id.trim().is_empty() {
        return Err(GlanceletError::Source(
            "GitHub issue response omitted stable node identity".into(),
        ));
    }
    let repository = repository_from_api_url(&issue.repository_url)?;
    let metadata = json!({
        "repository": repository,
        "number": issue.number,
        "source_updated": issue.updated_at,
    });
    let navigation = https_navigation(&issue.html_url);
    let revision = revision(&issue.title, &metadata, &navigation)?;
    Ok(SourceRecord {
        identity: SourceIdentity {
            entity_type: format!("github.{entity_type}"),
            external_id: issue.node_id,
        },
        title: normalized_title(&issue.title, "Untitled GitHub item"),
        revision,
        display: json!({ "repository": repository }),
        metadata,
        navigation,
    })
}

fn workflow_record(
    repository: &GithubRepository,
    workflow: GithubWorkflow,
    run: GithubWorkflowRun,
) -> Result<SourceRecord> {
    let conclusion = run.conclusion.as_deref().ok_or_else(|| {
        GlanceletError::Source("GitHub completed workflow run omitted conclusion".into())
    })?;
    let title = format!(
        "{} {}",
        normalized_title(&workflow.name, "GitHub workflow"),
        failure_label(conclusion)
    );
    let metadata = json!({
        "repository": repository.full_name,
        "workflow_id": workflow.id,
        "run_id": run.id,
        "conclusion": conclusion,
        "default_branch": repository.default_branch,
        "source_updated": run.updated_at,
    });
    let navigation = https_navigation(&run.html_url);
    let revision = revision(&title, &metadata, &navigation)?;
    Ok(SourceRecord {
        identity: SourceIdentity {
            entity_type: "github.workflow".into(),
            external_id: workflow.id.to_string(),
        },
        title,
        revision,
        display: json!({ "repository": repository.full_name }),
        metadata,
        navigation,
    })
}

pub fn is_failure_conclusion(conclusion: &str) -> bool {
    matches!(
        conclusion,
        "failure" | "timed_out" | "startup_failure" | "action_required"
    )
}

fn failure_label(conclusion: &str) -> &'static str {
    match conclusion {
        "timed_out" => "timed out",
        "startup_failure" => "failed to start",
        "action_required" => "needs action",
        _ => "failed",
    }
}

fn repository_from_api_url(value: &str) -> Result<String> {
    let url = Url::parse(value)
        .map_err(|_| GlanceletError::Source("invalid GitHub repository URL".into()))?;
    if url.scheme() != "https" {
        return Err(GlanceletError::Source(
            "GitHub repository URL was not HTTPS".into(),
        ));
    }
    let segments = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    let index = segments
        .iter()
        .position(|segment| *segment == "repos")
        .ok_or_else(|| GlanceletError::Source("invalid GitHub repository URL".into()))?;
    if segments.len() != index + 3 {
        return Err(GlanceletError::Source(
            "invalid GitHub repository URL".into(),
        ));
    }
    Ok(format!("{}/{}", segments[index + 1], segments[index + 2]))
}

fn https_navigation(value: &str) -> Value {
    if Url::parse(value).is_ok_and(|url| url.scheme() == "https") {
        json!({ "web_url": value })
    } else {
        json!({})
    }
}

fn normalized_title(value: &str, fallback: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        fallback.into()
    } else {
        collapsed.chars().take(240).collect()
    }
}

fn revision(title: &str, metadata: &Value, navigation: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(&json!({
        "title": title,
        "metadata": metadata,
        "navigation": navigation,
    }))
    .map_err(|_| GlanceletError::Source("cannot normalize GitHub source record".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
}
