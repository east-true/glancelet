use std::{sync::Arc, time::Duration};

use chrono::{DateTime, TimeZone, Utc};
use reqwest::{
    header::{HeaderMap, LINK, RETRY_AFTER},
    redirect::Policy,
    Client, RequestBuilder, Response, StatusCode,
};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{application::Clock, GlanceletError, Result};

const USER_AGENT: &str = "Glancelet/0.1";
pub const GITLAB_COM_ORIGIN: &str = "https://gitlab.com";
pub const REQUIRED_SCOPE: &str = "read_api";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GitlabInstance(String);

impl GitlabInstance {
    pub fn parse(value: &str) -> Result<Self> {
        let mut url = Url::parse(value.trim()).map_err(|_| {
            GlanceletError::ConfigurationRequired("invalid GitLab instance URL".into())
        })?;
        if url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(GlanceletError::ConfigurationRequired(
                "GitLab instance URL must contain only an origin".into(),
            ));
        }
        let secure = url.scheme() == "https";
        let local_http = url.scheme() == "http"
            && url
                .host_str()
                .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"));
        if !secure && !local_http {
            return Err(GlanceletError::ConfigurationRequired(
                "GitLab instance must use HTTPS".into(),
            ));
        }
        url.set_path("");
        Ok(Self(url.origin().ascii_serialization()))
    }

    pub fn gitlab_com() -> Self {
        Self(GITLAB_COM_ORIGIN.into())
    }

    pub fn origin(&self) -> &str {
        &self.0
    }

    pub fn host_label(&self) -> String {
        Url::parse(&self.0)
            .ok()
            .and_then(|url| url.host_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| self.0.clone())
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.0, path)
    }

    fn accepts_url(&self, url: &Url) -> bool {
        Url::parse(&self.0).is_ok_and(|origin| origin.origin() == url.origin())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitlabCredential {
    OAuth {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<DateTime<Utc>>,
    },
    PersonalAccessToken {
        token: String,
    },
}

impl GitlabCredential {
    pub fn oauth(
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self::OAuth {
            access_token,
            refresh_token,
            expires_at,
        }
    }

    pub fn personal_access_token(token: String) -> Self {
        Self::PersonalAccessToken { token }
    }

    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::OAuth { expires_at, .. } => *expires_at,
            Self::PersonalAccessToken { .. } => None,
        }
    }

    pub fn refresh_token(&self) -> Option<&str> {
        match self {
            Self::OAuth { refresh_token, .. } => refresh_token.as_deref(),
            Self::PersonalAccessToken { .. } => None,
        }
    }

    pub fn auth(&self) -> GitlabAuth {
        match self {
            Self::OAuth { access_token, .. } => GitlabAuth::OAuth(access_token.clone()),
            Self::PersonalAccessToken { token } => GitlabAuth::PersonalAccessToken(token.clone()),
        }
    }
}

#[derive(Clone)]
pub enum GitlabAuth {
    OAuth(String),
    PersonalAccessToken(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GitlabIdentity {
    pub id: String,
    pub username: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitlabTodo {
    pub id: u64,
    pub action_name: String,
    pub target_type: String,
    pub target_title: Option<String>,
    pub target_url: String,
    pub project_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub(crate) enum DeviceTokenPoll {
    Pending,
    SlowDown,
    AccessDenied,
    Expired,
    Authorized(GitlabCredential),
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct GitlabDeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: i64,
    pub interval: i64,
}

#[derive(Clone)]
pub struct GitlabApiClient {
    http: Client,
    clock: Arc<dyn Clock>,
}

impl GitlabApiClient {
    pub fn production(clock: Arc<dyn Clock>) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(Policy::none())
            .build()
            .map_err(network_error)?;
        Ok(Self::new(http, clock))
    }

    pub fn new(http: Client, clock: Arc<dyn Clock>) -> Self {
        Self { http, clock }
    }

    pub(crate) async fn request_device_code(
        &self,
        instance: &GitlabInstance,
        client_id: &str,
    ) -> Result<GitlabDeviceCode> {
        if client_id.trim().is_empty() {
            return Err(GlanceletError::OAuth(
                "GLANCELET_GITLAB_CLIENT_ID is not configured".into(),
            ));
        }
        let response = self
            .http
            .post(instance.endpoint("/oauth/authorize_device"))
            .header("User-Agent", USER_AGENT)
            .form(&[("client_id", client_id), ("scope", REQUIRED_SCOPE)])
            .send()
            .await
            .map_err(network_error)?;
        if !response.status().is_success() {
            return Err(oauth_error(response, false).await);
        }
        response
            .json()
            .await
            .map_err(|_| malformed("unexpected GitLab device authorization response"))
    }

    pub(crate) async fn poll_device_token(
        &self,
        instance: &GitlabInstance,
        client_id: &str,
        device_code: &str,
    ) -> Result<DeviceTokenPoll> {
        let response = self
            .http
            .post(instance.endpoint("/oauth/token"))
            .header("User-Agent", USER_AGENT)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
                ("client_id", client_id),
            ])
            .send()
            .await
            .map_err(network_error)?;
        let raw: RawTokenResponse = response
            .json()
            .await
            .map_err(|_| malformed("unexpected GitLab device token response"))?;
        match raw.error.as_deref() {
            Some("authorization_pending") => Ok(DeviceTokenPoll::Pending),
            Some("slow_down") => Ok(DeviceTokenPoll::SlowDown),
            Some("access_denied") => Ok(DeviceTokenPoll::AccessDenied),
            Some("expired_token") => Ok(DeviceTokenPoll::Expired),
            Some("unauthorized_client" | "device_flow_disabled" | "unsupported_grant_type") => Err(
                GlanceletError::OAuth("GitLab OAuth application does not allow Device Flow".into()),
            ),
            Some(_) => Err(GlanceletError::OAuth(
                "GitLab rejected the device authorization request".into(),
            )),
            None => Ok(DeviceTokenPoll::Authorized(self.credential(raw)?)),
        }
    }

    pub async fn refresh_token(
        &self,
        instance: &GitlabInstance,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<GitlabCredential> {
        let response = self
            .http
            .post(instance.endpoint("/oauth/token"))
            .header("User-Agent", USER_AGENT)
            .form(&[
                ("client_id", client_id),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(network_error)?;
        if !response.status().is_success() {
            if response.status() == StatusCode::TOO_MANY_REQUESTS
                || response.status() == StatusCode::REQUEST_TIMEOUT
                || response.status().is_server_error()
            {
                return Err(rest_error(response, self.clock.now()).await);
            }
            return Err(oauth_error(response, true).await);
        }
        let raw: RawTokenResponse = response
            .json()
            .await
            .map_err(|_| malformed("unexpected GitLab refresh response"))?;
        if raw.error.is_some() {
            return Err(GlanceletError::AuthenticationRequired(
                "GitLab connection must be authorized again".into(),
            ));
        }
        self.credential(raw)
    }

    pub async fn authenticated_user(
        &self,
        instance: &GitlabInstance,
        auth: &GitlabAuth,
    ) -> Result<GitlabIdentity> {
        let response = self
            .send_rest(self.authenticated(self.http.get(instance.endpoint("/api/v4/user")), auth))
            .await?;
        let raw: RawUser = response
            .json()
            .await
            .map_err(|_| malformed("unexpected GitLab user response"))?;
        Ok(GitlabIdentity {
            id: raw.id.to_string(),
            username: raw.username,
        })
    }

    pub async fn todos(
        &self,
        instance: &GitlabInstance,
        auth: &GitlabAuth,
    ) -> Result<Vec<GitlabTodo>> {
        let mut next = Url::parse(&instance.endpoint("/api/v4/todos"))
            .map_err(|_| malformed("invalid GitLab To-Dos endpoint"))?;
        next.query_pairs_mut()
            .append_pair("state", "pending")
            .append_pair("per_page", "100");
        let mut todos = Vec::new();
        loop {
            if !instance.accepts_url(&next) || !next.path().starts_with("/api/v4/") {
                return Err(GlanceletError::ConfigurationRequired(
                    "GitLab pagination attempted to leave the configured instance".into(),
                ));
            }
            let response = self
                .send_rest(self.authenticated(self.http.get(next.clone()), auth))
                .await?;
            let next_page = next_link(response.headers(), instance)?;
            let raw: Vec<RawTodo> = response
                .json()
                .await
                .map_err(|_| malformed("unexpected GitLab To-Dos response"))?;
            todos.extend(raw.into_iter().map(RawTodo::normalized));
            match next_page {
                Some(url) => next = url,
                None => break,
            }
        }
        Ok(todos)
    }

    pub fn navigation_url(&self, instance: &GitlabInstance, value: &str) -> Result<String> {
        let url = Url::parse(value)
            .map_err(|_| GlanceletError::Source("GitLab To-Do target URL is invalid".into()))?;
        if !instance.accepts_url(&url)
            || (url.scheme() != "https"
                && !(url.scheme() == "http"
                    && url
                        .host_str()
                        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"))))
        {
            return Err(GlanceletError::Source(
                "GitLab To-Do target URL left the configured instance".into(),
            ));
        }
        Ok(url.into())
    }

    fn credential(&self, raw: RawTokenResponse) -> Result<GitlabCredential> {
        let access_token = raw
            .access_token
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| malformed("GitLab token response omitted access token"))?;
        let scopes = raw.scope.unwrap_or_default();
        if !scopes
            .split_whitespace()
            .any(|scope| scope == REQUIRED_SCOPE)
        {
            return Err(GlanceletError::ConfigurationRequired(
                "GitLab connection did not grant read_api scope".into(),
            ));
        }
        let issued_at = raw
            .created_at
            .and_then(|value| Utc.timestamp_opt(value, 0).single())
            .unwrap_or_else(|| self.clock.now());
        let expires_at = raw
            .expires_in
            .filter(|seconds| *seconds > 0)
            .map(|seconds| issued_at + chrono::Duration::seconds(seconds));
        Ok(GitlabCredential::oauth(
            access_token,
            raw.refresh_token.filter(|token| !token.trim().is_empty()),
            expires_at,
        ))
    }

    fn authenticated(&self, builder: RequestBuilder, auth: &GitlabAuth) -> RequestBuilder {
        let builder = builder
            .header("Accept", "application/json")
            .header("User-Agent", USER_AGENT);
        match auth {
            GitlabAuth::OAuth(token) => builder.bearer_auth(token),
            GitlabAuth::PersonalAccessToken(token) => builder.header("PRIVATE-TOKEN", token),
        }
    }

    async fn send_rest(&self, builder: RequestBuilder) -> Result<Response> {
        let response = builder.send().await.map_err(network_error)?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(rest_error(response, self.clock.now()).await)
        }
    }
}

#[derive(Deserialize)]
struct RawUser {
    id: u64,
    username: String,
}

#[derive(Deserialize)]
struct RawTodo {
    id: u64,
    action_name: String,
    target_type: String,
    target: Option<RawTarget>,
    target_url: String,
    project: Option<RawProject>,
    created_at: String,
    updated_at: String,
}

impl RawTodo {
    fn normalized(self) -> GitlabTodo {
        GitlabTodo {
            id: self.id,
            action_name: self.action_name,
            target_type: self.target_type,
            target_title: self.target.and_then(|target| target.title),
            target_url: self.target_url,
            project_path: self.project.map(|project| project.path_with_namespace),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Deserialize)]
struct RawTarget {
    title: Option<String>,
}

#[derive(Deserialize)]
struct RawProject {
    path_with_namespace: String,
}

#[derive(Deserialize)]
struct RawTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    created_at: Option<i64>,
    scope: Option<String>,
    error: Option<String>,
}

fn next_link(headers: &HeaderMap, instance: &GitlabInstance) -> Result<Option<Url>> {
    let Some(value) = headers.get(LINK) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| GlanceletError::Source("GitLab pagination Link header is invalid".into()))?;
    for link in value.split(',') {
        let mut parts = link.trim().split(';');
        let target = parts.next().unwrap_or_default().trim();
        let is_next = parts.any(|part| part.trim() == "rel=\"next\"");
        if !is_next {
            continue;
        }
        let url = target
            .strip_prefix('<')
            .and_then(|target| target.strip_suffix('>'))
            .and_then(|target| Url::parse(target).ok())
            .ok_or_else(|| GlanceletError::Source("GitLab next-page Link is invalid".into()))?;
        if !instance.accepts_url(&url) || !url.path().starts_with("/api/v4/") {
            return Err(GlanceletError::ConfigurationRequired(
                "GitLab pagination attempted to leave the configured instance".into(),
            ));
        }
        return Ok(Some(url));
    }
    Ok(None)
}

async fn rest_error(response: Response, now: DateTime<Utc>) -> GlanceletError {
    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS {
        let retry_after_seconds = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok())
            .or_else(|| {
                response
                    .headers()
                    .get("RateLimit-Reset")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<i64>().ok())
                    .map(|reset| (reset - now.timestamp()).max(1))
            })
            .unwrap_or(60)
            .max(1);
        return GlanceletError::RateLimited {
            provider: "GitLab".into(),
            retry_after_seconds,
        };
    }
    match status {
        StatusCode::UNAUTHORIZED => GlanceletError::AuthenticationRequired(
            "GitLab credential is invalid, expired, or revoked".into(),
        ),
        StatusCode::FORBIDDEN => GlanceletError::ConfigurationRequired(
            "GitLab denied read_api access to this operation".into(),
        ),
        StatusCode::REQUEST_TIMEOUT => {
            GlanceletError::TransientNetwork("GitLab request timed out".into())
        }
        status if status.is_server_error() => {
            GlanceletError::ProviderFailure(format!("GitLab service failed ({status})"))
        }
        _ => GlanceletError::Source(format!("GitLab rejected the request ({status})")),
    }
}

async fn oauth_error(response: Response, refresh: bool) -> GlanceletError {
    let status = response.status();
    let code = response
        .json::<RawTokenResponse>()
        .await
        .ok()
        .and_then(|body| body.error);
    if refresh || matches!(code.as_deref(), Some("invalid_grant")) {
        return GlanceletError::AuthenticationRequired(
            "GitLab connection must be authorized again".into(),
        );
    }
    match code.as_deref() {
        Some("unauthorized_client") => {
            GlanceletError::OAuth("GitLab OAuth application does not allow Device Flow".into())
        }
        _ => GlanceletError::OAuth(format!("GitLab rejected the OAuth request ({status})")),
    }
}

fn network_error(error: reqwest::Error) -> GlanceletError {
    GlanceletError::TransientNetwork(if error.is_timeout() {
        "GitLab request timed out".into()
    } else {
        "GitLab instance is unavailable".into()
    })
}

fn malformed(message: &str) -> GlanceletError {
    GlanceletError::ProviderFailure(message.into())
}
