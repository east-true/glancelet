use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header::RETRY_AFTER, Client, Method, RequestBuilder};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::{GlanceletError, Result};

#[derive(Clone)]
pub struct SlackApiClient {
    http: Client,
    api_base: String,
}

impl SlackApiClient {
    pub fn production() -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(network_error)?;
        Ok(Self::new(http, "https://slack.com/api"))
    }

    pub fn new(http: Client, api_base: impl Into<String>) -> Self {
        Self {
            http,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
        }
    }

    pub async fn exchange_code(
        &self,
        client_id: &str,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        now: DateTime<Utc>,
    ) -> Result<SlackCredential> {
        let form = [
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
        ];
        let response: TokenResponse = self
            .send(self.http.post(self.url("oauth.v2.user.access")).form(&form))
            .await?;
        SlackCredential::from_response(response, now)
    }

    pub async fn refresh_token(
        &self,
        client_id: &str,
        refresh_token: &str,
        now: DateTime<Utc>,
    ) -> Result<SlackCredential> {
        let form = [
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ];
        let response: TokenResponse = self
            .send(self.http.post(self.url("oauth.v2.user.access")).form(&form))
            .await?;
        SlackCredential::from_response(response, now)
    }

    pub async fn auth_test(&self, token: &str) -> Result<SlackIdentity> {
        let response: AuthTestResponse = self
            .send(
                self.http
                    .request(Method::POST, self.url("auth.test"))
                    .bearer_auth(token),
            )
            .await?;
        Ok(SlackIdentity {
            team_id: response
                .team_id
                .ok_or_else(|| malformed("auth.test omitted team_id"))?,
            team_name: response.team.unwrap_or_else(|| "Slack workspace".into()),
            user_id: response
                .user_id
                .ok_or_else(|| malformed("auth.test omitted user_id"))?,
            user_name: response.user.unwrap_or_else(|| "Slack user".into()),
        })
    }

    pub async fn reactions_page(&self, token: &str, cursor: Option<&str>) -> Result<ReactionsPage> {
        let mut query = vec![("limit", "200")];
        if let Some(cursor) = cursor {
            query.push(("cursor", cursor));
        }
        self.send(
            self.http
                .get(self.url("reactions.list"))
                .bearer_auth(token)
                .query(&query),
        )
        .await
    }

    pub async fn permalink(&self, token: &str, channel: &str, message_ts: &str) -> Result<String> {
        let response: PermalinkResponse = self
            .send(
                self.http
                    .get(self.url("chat.getPermalink"))
                    .bearer_auth(token)
                    .query(&[("channel", channel), ("message_ts", message_ts)]),
            )
            .await?;
        response
            .permalink
            .ok_or_else(|| malformed("chat.getPermalink omitted permalink"))
    }

    async fn send<T: DeserializeOwned>(&self, builder: RequestBuilder) -> Result<T> {
        let response = builder.send().await.map_err(network_error)?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let seconds = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|value| *value >= 0)
                .ok_or_else(|| malformed("Slack 429 omitted a valid Retry-After header"))?;
            return Err(GlanceletError::RateLimited {
                provider: "Slack".into(),
                retry_after_seconds: seconds,
            });
        }
        if !response.status().is_success() {
            return Err(GlanceletError::Source(format!(
                "Slack HTTP request failed with status {}",
                response.status()
            )));
        }
        let value: Value = response.json().await.map_err(network_error)?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            let code = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unexpected_error");
            return Err(api_error(code));
        }
        serde_json::from_value(value).map_err(|_| malformed("unexpected Slack response"))
    }

    fn url(&self, method: &str) -> String {
        format!("{}/{method}", self.api_base)
    }
}

#[derive(Serialize, Deserialize)]
pub struct SlackCredential {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    scope: Option<String>,
}

impl SlackCredential {
    fn from_response(response: TokenResponse, now: DateTime<Utc>) -> Result<Self> {
        if response.token_type.as_deref() != Some("user") {
            return Err(malformed("OAuth response did not contain a user token"));
        }
        let access_token = response
            .access_token
            .ok_or_else(|| malformed("OAuth response omitted access token"))?;
        Ok(Self {
            access_token,
            refresh_token: response.refresh_token,
            expires_at: response
                .expires_in
                .map(|seconds| now + chrono::Duration::seconds(seconds)),
            scope: response
                .scope
                .or_else(|| response.authed_user.and_then(|user| user.scope)),
        })
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.refresh_token.as_deref()
    }

    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.expires_at
    }

    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SlackIdentity {
    pub team_id: String,
    pub team_name: String,
    pub user_id: String,
    pub user_name: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
    scope: Option<String>,
    authed_user: Option<TokenUser>,
}

#[derive(Deserialize)]
struct TokenUser {
    scope: Option<String>,
}

#[derive(Deserialize)]
struct AuthTestResponse {
    team: Option<String>,
    user: Option<String>,
    team_id: Option<String>,
    user_id: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct ReactionsPage {
    #[serde(default)]
    pub items: Vec<ReactionItem>,
    pub response_metadata: Option<ResponseMetadata>,
}

#[derive(Clone, Deserialize)]
pub struct ReactionItem {
    #[serde(rename = "type")]
    pub kind: String,
    pub channel: Option<String>,
    pub message: Option<ReactionMessage>,
}

#[derive(Clone, Deserialize)]
pub struct ReactionMessage {
    pub ts: Option<String>,
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
}

#[derive(Clone, Deserialize)]
pub struct Reaction {
    pub name: String,
    #[serde(default)]
    pub users: Vec<String>,
}

#[derive(Clone, Deserialize)]
pub struct ResponseMetadata {
    #[serde(default)]
    pub next_cursor: String,
}

#[derive(Deserialize)]
struct PermalinkResponse {
    permalink: Option<String>,
}

fn api_error(code: &str) -> GlanceletError {
    match code {
        "invalid_auth"
        | "not_authed"
        | "token_expired"
        | "token_revoked"
        | "account_inactive"
        | "invalid_refresh_token" => GlanceletError::AuthenticationRequired(
            "Slack connection must be authorized again".into(),
        ),
        "missing_scope" | "no_permission" => {
            GlanceletError::Source("Slack reactions:read permission is missing".into())
        }
        "ratelimited" => GlanceletError::Source("Slack rate limited the request".into()),
        _ => GlanceletError::Source(format!("Slack API error: {code}")),
    }
}

fn malformed(message: &str) -> GlanceletError {
    GlanceletError::Source(message.into())
}

fn network_error(error: reqwest::Error) -> GlanceletError {
    if error.is_timeout() {
        GlanceletError::Source("Slack request timed out".into())
    } else {
        GlanceletError::Source("Slack network request failed".into())
    }
}
