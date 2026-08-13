use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use reqwest::{header::RETRY_AFTER, Client, RequestBuilder, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{application::Clock, GlanceletError, Result};

pub const API_VERSION: &str = "2026-03-10";
const ACCEPT: &str = "application/vnd.github+json";
const USER_AGENT: &str = "Glancelet/0.1";

#[derive(Clone)]
pub struct GithubApiClient {
    http: Client,
    api_base: String,
    oauth_base: String,
    clock: Arc<dyn Clock>,
}

impl GithubApiClient {
    pub fn production(clock: Arc<dyn Clock>) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(network_error)?;
        Ok(Self::new(
            http,
            "https://api.github.com",
            "https://github.com/login",
            clock,
        ))
    }

    pub fn new(
        http: Client,
        api_base: impl Into<String>,
        oauth_base: impl Into<String>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            http,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            oauth_base: oauth_base.into().trim_end_matches('/').to_owned(),
            clock,
        }
    }

    pub(crate) async fn request_device_code(&self, client_id: &str) -> Result<GithubDeviceCode> {
        if client_id.trim().is_empty() {
            return Err(GlanceletError::OAuth(
                "GLANCELET_GITHUB_CLIENT_ID is not configured".into(),
            ));
        }
        let response = self
            .oauth_request(
                self.http
                    .post(format!("{}/device/code", self.oauth_base))
                    .form(&[("client_id", client_id)]),
            )
            .send()
            .await
            .map_err(network_error)?;
        if !response.status().is_success() {
            return Err(oauth_response_error(response).await);
        }
        response
            .json::<GithubDeviceCode>()
            .await
            .map_err(|_| malformed("unexpected GitHub device authorization response"))
    }

    pub(crate) async fn poll_device_token(
        &self,
        client_id: &str,
        device_code: &str,
    ) -> Result<DeviceTokenPoll> {
        let response = self
            .oauth_request(
                self.http
                    .post(format!("{}/oauth/access_token", self.oauth_base))
                    .form(&[
                        ("client_id", client_id),
                        ("device_code", device_code),
                        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ]),
            )
            .send()
            .await
            .map_err(network_error)?;
        if !response.status().is_success() {
            return Err(oauth_response_error(response).await);
        }
        let raw: RawTokenResponse = response
            .json()
            .await
            .map_err(|_| malformed("unexpected GitHub device token response"))?;
        match raw.error.as_deref() {
            Some("authorization_pending") => Ok(DeviceTokenPoll::Pending),
            Some("slow_down") => Ok(DeviceTokenPoll::SlowDown),
            Some("access_denied") => Ok(DeviceTokenPoll::AccessDenied),
            Some("expired_token") => Ok(DeviceTokenPoll::Expired),
            Some("device_flow_disabled") => Err(GlanceletError::OAuth(
                "GitHub App Device Flow is disabled".into(),
            )),
            Some("unverified_user_email") => Err(GlanceletError::OAuth(
                "GitHub requires a verified primary email before authorization".into(),
            )),
            Some(_) => Err(GlanceletError::OAuth(
                "GitHub rejected the device authorization request".into(),
            )),
            None => Ok(DeviceTokenPoll::Authorized(GithubCredential::from_raw(
                raw,
                self.clock.now(),
            )?)),
        }
    }

    pub async fn refresh_token(
        &self,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<GithubCredential> {
        let response = self
            .oauth_request(
                self.http
                    .post(format!("{}/oauth/access_token", self.oauth_base))
                    .form(&[
                        ("client_id", client_id),
                        ("grant_type", "refresh_token"),
                        ("refresh_token", refresh_token),
                    ]),
            )
            .send()
            .await
            .map_err(network_error)?;
        if !response.status().is_success() {
            return Err(oauth_response_error(response).await);
        }
        let raw: RawTokenResponse = response
            .json()
            .await
            .map_err(|_| malformed("unexpected GitHub refresh response"))?;
        if raw.error.as_deref() == Some("bad_refresh_token") {
            return Err(GlanceletError::AuthenticationRequired(
                "GitHub connection must be authorized again".into(),
            ));
        }
        if raw.error.is_some() {
            return Err(GlanceletError::AuthenticationRequired(
                "GitHub token refresh was rejected".into(),
            ));
        }
        GithubCredential::from_raw(raw, self.clock.now())
    }

    pub async fn authenticated_user(&self, token: &str) -> Result<GithubIdentity> {
        let raw: RawUser = self
            .send_rest(self.rest_request(self.http.get(self.api_url("user")), token))
            .await?;
        Ok(GithubIdentity {
            id: raw.id.to_string(),
            login: raw.login,
        })
    }

    pub async fn repositories(&self, token: &str) -> Result<Vec<GithubRepository>> {
        let installations = self.installations(token).await?;
        let mut repositories = HashMap::new();
        for installation_id in installations {
            let mut page = 1_u32;
            loop {
                let response: RepositoriesResponse = self
                    .send_rest(
                        self.rest_request(
                            self.http
                                .get(self.api_url(&format!(
                                    "user/installations/{installation_id}/repositories"
                                )))
                                .query(&[("per_page", 100_u32), ("page", page)]),
                            token,
                        ),
                    )
                    .await?;
                let count = response.repositories.len();
                for repository in response.repositories {
                    repositories.insert(repository.id, repository.normalized());
                }
                if count < 100 {
                    break;
                }
                page += 1;
            }
        }
        let mut repositories = repositories.into_values().collect::<Vec<_>>();
        repositories.sort_by(|left, right| left.full_name.cmp(&right.full_name));
        Ok(repositories)
    }

    async fn installations(&self, token: &str) -> Result<Vec<u64>> {
        let mut installations = Vec::new();
        let mut page = 1_u32;
        loop {
            let response: InstallationsResponse = self
                .send_rest(
                    self.rest_request(
                        self.http
                            .get(self.api_url("user/installations"))
                            .query(&[("per_page", 100_u32), ("page", page)]),
                        token,
                    ),
                )
                .await?;
            let count = response.installations.len();
            installations.extend(response.installations.into_iter().map(|value| value.id));
            if count < 100 {
                break;
            }
            page += 1;
        }
        Ok(installations)
    }

    pub(crate) async fn review_requests(&self, token: &str) -> Result<Vec<GithubIssue>> {
        let query = "is:open is:pr user-review-requested:@me";
        let mut items = Vec::new();
        let mut page = 1_u32;
        loop {
            let response: SearchResponse = self
                .send_rest(self.rest_request(
                    self.http.get(self.api_url("search/issues")).query(&[
                        ("q", query.to_owned()),
                        ("per_page", "100".to_owned()),
                        ("page", page.to_string()),
                    ]),
                    token,
                ))
                .await?;
            if response.incomplete_results {
                return Err(GlanceletError::Source(
                    "GitHub review search returned an incomplete result".into(),
                ));
            }
            if response.total_count > 1_000 {
                return Err(GlanceletError::Source(
                    "GitHub review search result set is too large for an authoritative snapshot"
                        .into(),
                ));
            }
            let count = response.items.len();
            items.extend(response.items);
            if items.len() >= response.total_count || count == 0 {
                if items.len() < response.total_count {
                    return Err(GlanceletError::Source(
                        "GitHub review search ended before the authoritative result was complete"
                            .into(),
                    ));
                }
                break;
            }
            page += 1;
        }
        Ok(items)
    }

    pub(crate) async fn assigned_issues(&self, token: &str) -> Result<Vec<GithubIssue>> {
        let mut items = Vec::new();
        let mut page = 1_u32;
        loop {
            let response: Vec<GithubIssue> = self
                .send_rest(self.rest_request(
                    self.http.get(self.api_url("issues")).query(&[
                        ("filter", "assigned"),
                        ("state", "open"),
                        ("per_page", "100"),
                        ("page", &page.to_string()),
                    ]),
                    token,
                ))
                .await?;
            let count = response.len();
            items.extend(
                response
                    .into_iter()
                    .filter(|issue| !issue.is_pull_request()),
            );
            if count < 100 {
                break;
            }
            page += 1;
        }
        Ok(items)
    }

    pub(crate) async fn workflows(
        &self,
        token: &str,
        repository: &GithubRepository,
    ) -> Result<Vec<GithubWorkflow>> {
        let mut workflows = Vec::new();
        let mut page = 1_u32;
        loop {
            let response: WorkflowsResponse = self
                .send_rest(
                    self.rest_request(
                        self.http
                            .get(self.api_url(&format!(
                                "repos/{}/actions/workflows",
                                repository.full_name
                            )))
                            .query(&[("per_page", 100_u32), ("page", page)]),
                        token,
                    ),
                )
                .await?;
            let count = response.workflows.len();
            workflows.extend(
                response
                    .workflows
                    .into_iter()
                    .filter(|workflow| workflow.state == "active"),
            );
            if count < 100 {
                break;
            }
            page += 1;
        }
        Ok(workflows)
    }

    pub(crate) async fn latest_completed_run(
        &self,
        token: &str,
        repository: &GithubRepository,
        workflow_id: u64,
    ) -> Result<Option<GithubWorkflowRun>> {
        let response: WorkflowRunsResponse = self
            .send_rest(
                self.rest_request(
                    self.http
                        .get(self.api_url(&format!(
                            "repos/{}/actions/workflows/{workflow_id}/runs",
                            repository.full_name
                        )))
                        .query(&[
                            ("branch", repository.default_branch.as_str()),
                            ("status", "completed"),
                            ("per_page", "1"),
                            ("page", "1"),
                        ]),
                    token,
                ),
            )
            .await?;
        Ok(response.workflow_runs.into_iter().next())
    }

    fn oauth_request(&self, builder: RequestBuilder) -> RequestBuilder {
        builder.header("Accept", "application/json")
    }

    fn rest_request(&self, builder: RequestBuilder, token: &str) -> RequestBuilder {
        builder
            .bearer_auth(token)
            .header("Accept", ACCEPT)
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", USER_AGENT)
    }

    async fn send_rest<T: DeserializeOwned>(&self, builder: RequestBuilder) -> Result<T> {
        let response = builder.send().await.map_err(network_error)?;
        if response.status().is_success() {
            return response
                .json()
                .await
                .map_err(|_| malformed("unexpected GitHub API response"));
        }
        Err(self.rest_error(response).await)
    }

    async fn rest_error(&self, response: Response) -> GlanceletError {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value >= 0);
        let remaining_is_zero = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok())
            == Some("0");
        let reset_after = response
            .headers()
            .get("x-ratelimit-reset")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok())
            .map(|epoch| epoch - self.clock.now().timestamp())
            .map(|seconds| seconds.max(1));
        let body = response.json::<ApiError>().await.ok();
        let message = body
            .as_ref()
            .map(|value| value.message.to_ascii_lowercase())
            .unwrap_or_default();
        let rate_limited = status == StatusCode::TOO_MANY_REQUESTS
            || retry_after.is_some()
            || remaining_is_zero
            || message.contains("rate limit")
            || message.contains("abuse detection");
        if rate_limited {
            return GlanceletError::RateLimited {
                provider: "GitHub".into(),
                retry_after_seconds: retry_after.or(reset_after).unwrap_or(60),
            };
        }
        match status {
            StatusCode::UNAUTHORIZED => GlanceletError::AuthenticationRequired(
                "GitHub connection must be authorized again".into(),
            ),
            StatusCode::FORBIDDEN => GlanceletError::Source(
                "The GitHub App does not have permission for this resource".into(),
            ),
            StatusCode::NOT_FOUND => GlanceletError::NotFound(
                "GitHub resource is unavailable to this App installation".into(),
            ),
            status if status.is_server_error() => {
                GlanceletError::Source("GitHub is temporarily unavailable".into())
            }
            _ => {
                GlanceletError::Source(format!("GitHub rejected the request ({})", status.as_u16()))
            }
        }
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/{path}", self.api_base)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GithubCredential {
    access_token: String,
    expires_at: Option<DateTime<Utc>>,
    refresh_token: Option<String>,
    refresh_token_expires_at: Option<DateTime<Utc>>,
}

impl GithubCredential {
    fn from_raw(raw: RawTokenResponse, now: DateTime<Utc>) -> Result<Self> {
        let access_token = raw
            .access_token
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| malformed("GitHub token response omitted access_token"))?;
        if raw.token_type.as_deref() != Some("bearer") {
            return Err(malformed(
                "GitHub token response used an unsupported token type",
            ));
        }
        Ok(Self {
            access_token,
            expires_at: raw
                .expires_in
                .map(|seconds| now + chrono::Duration::seconds(seconds)),
            refresh_token: raw.refresh_token,
            refresh_token_expires_at: raw
                .refresh_token_expires_in
                .map(|seconds| now + chrono::Duration::seconds(seconds)),
        })
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.expires_at
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }

    pub fn refresh_token_expires_at(&self) -> Option<DateTime<Utc>> {
        self.refresh_token_expires_at
    }
}

#[derive(Deserialize)]
pub(crate) struct GithubDeviceCode {
    pub(crate) device_code: String,
    pub(crate) user_code: String,
    pub(crate) verification_uri: String,
    pub(crate) expires_in: i64,
    pub(crate) interval: i64,
}

pub(crate) enum DeviceTokenPoll {
    Pending,
    SlowDown,
    AccessDenied,
    Expired,
    Authorized(GithubCredential),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIdentity {
    pub id: String,
    pub login: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubRepository {
    pub id: u64,
    pub node_id: String,
    pub full_name: String,
    pub default_branch: String,
}

#[derive(Clone, Deserialize)]
pub(crate) struct GithubIssue {
    pub node_id: String,
    pub title: String,
    pub html_url: String,
    pub number: u64,
    pub updated_at: DateTime<Utc>,
    pub repository_url: String,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

impl GithubIssue {
    pub(crate) fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }
}

#[derive(Clone, Deserialize)]
pub(crate) struct GithubWorkflow {
    pub id: u64,
    pub name: String,
    pub state: String,
}

#[derive(Clone, Deserialize)]
pub(crate) struct GithubWorkflowRun {
    pub id: u64,
    pub conclusion: Option<String>,
    pub html_url: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct RawTokenResponse {
    access_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
    refresh_token_expires_in: Option<i64>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct RawUser {
    id: u64,
    login: String,
}

#[derive(Deserialize)]
struct Installation {
    id: u64,
}

#[derive(Deserialize)]
struct InstallationsResponse {
    installations: Vec<Installation>,
}

#[derive(Deserialize)]
struct RawRepository {
    id: u64,
    node_id: String,
    full_name: String,
    default_branch: String,
}

impl RawRepository {
    fn normalized(self) -> GithubRepository {
        GithubRepository {
            id: self.id,
            node_id: self.node_id,
            full_name: self.full_name,
            default_branch: self.default_branch,
        }
    }
}

#[derive(Deserialize)]
struct RepositoriesResponse {
    repositories: Vec<RawRepository>,
}

#[derive(Deserialize)]
struct SearchResponse {
    total_count: usize,
    incomplete_results: bool,
    items: Vec<GithubIssue>,
}

#[derive(Deserialize)]
struct WorkflowsResponse {
    workflows: Vec<GithubWorkflow>,
}

#[derive(Deserialize)]
struct WorkflowRunsResponse {
    workflow_runs: Vec<GithubWorkflowRun>,
}

#[derive(Deserialize)]
struct ApiError {
    message: String,
}

async fn oauth_response_error(response: Response) -> GlanceletError {
    let status = response.status();
    let error = response.json::<RawTokenResponse>().await.ok();
    match error.as_ref().and_then(|value| value.error.as_deref()) {
        Some("bad_refresh_token") => GlanceletError::AuthenticationRequired(
            "GitHub connection must be authorized again".into(),
        ),
        Some("device_flow_disabled") => {
            GlanceletError::OAuth("GitHub App Device Flow is disabled".into())
        }
        _ => GlanceletError::OAuth(format!(
            "GitHub rejected the authorization request ({})",
            status.as_u16()
        )),
    }
}

fn malformed(message: &str) -> GlanceletError {
    GlanceletError::Source(message.into())
}

fn network_error(error: reqwest::Error) -> GlanceletError {
    if error.is_timeout() {
        GlanceletError::Source("GitHub request timed out".into())
    } else {
        GlanceletError::Source("GitHub network request failed".into())
    }
}
