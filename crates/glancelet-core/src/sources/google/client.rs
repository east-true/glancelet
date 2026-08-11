use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{header::RETRY_AFTER, Client, RequestBuilder, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use url::Url;

use crate::{GlanceletError, Result};

use super::CALENDAR_SCOPE;

#[derive(Clone)]
pub struct GoogleApiClient {
    http: Client,
    api_base: String,
    token_endpoint: String,
    userinfo_endpoint: String,
}

impl GoogleApiClient {
    pub fn production() -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(network_error)?;
        Ok(Self::new(
            http,
            "https://www.googleapis.com/calendar/v3",
            "https://oauth2.googleapis.com/token",
            "https://openidconnect.googleapis.com/v1/userinfo",
        ))
    }

    pub fn new(
        http: Client,
        api_base: impl Into<String>,
        token_endpoint: impl Into<String>,
        userinfo_endpoint: impl Into<String>,
    ) -> Self {
        Self {
            http,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            token_endpoint: token_endpoint.into(),
            userinfo_endpoint: userinfo_endpoint.into(),
        }
    }

    pub async fn exchange_code(
        &self,
        client_id: &str,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        now: DateTime<Utc>,
    ) -> Result<GoogleCredential> {
        let response: TokenResponse = self
            .send(
                self.http.post(&self.token_endpoint).form(&[
                    ("client_id", client_id),
                    ("code", code),
                    ("code_verifier", verifier),
                    ("redirect_uri", redirect_uri),
                    ("grant_type", "authorization_code"),
                ]),
                RequestKind::OAuth,
            )
            .await?;
        GoogleCredential::initial(response, now)
    }

    pub async fn refresh_token(
        &self,
        client_id: &str,
        credential: &GoogleCredential,
        now: DateTime<Utc>,
    ) -> Result<GoogleCredential> {
        let refresh_token = credential.refresh_token.as_deref().ok_or_else(|| {
            GlanceletError::AuthenticationRequired("Google refresh token is missing".into())
        })?;
        let response: TokenResponse = self
            .send(
                self.http.post(&self.token_endpoint).form(&[
                    ("client_id", client_id),
                    ("refresh_token", refresh_token),
                    ("grant_type", "refresh_token"),
                ]),
                RequestKind::OAuth,
            )
            .await?;
        credential.refreshed(response, now)
    }

    pub async fn userinfo(&self, token: &str) -> Result<GoogleIdentity> {
        self.send(
            self.http.get(&self.userinfo_endpoint).bearer_auth(token),
            RequestKind::Api,
        )
        .await
    }

    pub async fn calendars(&self, token: &str) -> Result<Vec<GoogleCalendar>> {
        let mut page_token: Option<String> = None;
        let mut calendars = Vec::new();
        loop {
            let mut request = self
                .http
                .get(format!("{}/users/me/calendarList", self.api_base))
                .bearer_auth(token)
                .query(&[("maxResults", "250")]);
            if let Some(page_token) = page_token.as_deref() {
                request = request.query(&[("pageToken", page_token)]);
            }
            let page: CalendarListPage = self.send(request, RequestKind::Api).await?;
            calendars.extend(page.items);
            match page.next_page_token {
                Some(next) if !next.is_empty() => page_token = Some(next),
                _ => break,
            }
        }
        calendars.sort_by(|left, right| left.display_name().cmp(right.display_name()));
        Ok(calendars)
    }

    pub(crate) async fn events_page(
        &self,
        token: &str,
        calendar_id: &str,
        query: &GoogleEventsQuery,
    ) -> std::result::Result<GoogleEventsPage, GoogleEventsError> {
        let mut url = Url::parse(&self.api_base).map_err(|_| {
            GoogleEventsError::Other(GlanceletError::Source(
                "invalid Google Calendar API endpoint".into(),
            ))
        })?;
        url.path_segments_mut()
            .map_err(|_| {
                GoogleEventsError::Other(GlanceletError::Source(
                    "invalid Google Calendar API endpoint".into(),
                ))
            })?
            .push("calendars")
            .push(calendar_id)
            .push("events");
        let mut request = self.http.get(url).bearer_auth(token).query(&[
            ("maxResults", "2500"),
            ("singleEvents", "true"),
            ("showDeleted", "true"),
            ("timeZone", query.timezone.as_str()),
        ]);
        if let Some(value) = query.time_min.as_deref() {
            request = request.query(&[("timeMin", value)]);
        }
        if let Some(value) = query.time_max.as_deref() {
            request = request.query(&[("timeMax", value)]);
        }
        if let Some(value) = query.sync_token.as_deref() {
            request = request.query(&[("syncToken", value)]);
        }
        if let Some(value) = query.page_token.as_deref() {
            request = request.query(&[("pageToken", value)]);
        }

        let response = request
            .send()
            .await
            .map_err(|error| GoogleEventsError::Other(network_error(error)))?;
        if response.status() == StatusCode::GONE {
            return Err(GoogleEventsError::FullSyncRequired);
        }
        decode_response(response, RequestKind::Api)
            .await
            .map_err(GoogleEventsError::Other)
    }

    async fn send<T: DeserializeOwned>(
        &self,
        builder: RequestBuilder,
        kind: RequestKind,
    ) -> Result<T> {
        let response = builder.send().await.map_err(network_error)?;
        decode_response(response, kind).await
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GoogleCredential {
    access_token: String,
    refresh_token: Option<String>,
    access_token_expires_at: DateTime<Utc>,
    refresh_token_expires_at: Option<DateTime<Utc>>,
    granted_scopes: Vec<String>,
}

impl GoogleCredential {
    fn initial(response: TokenResponse, now: DateTime<Utc>) -> Result<Self> {
        let credential = Self {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            access_token_expires_at: now + chrono::Duration::seconds(response.expires_in),
            refresh_token_expires_at: response
                .refresh_token_expires_in
                .map(|seconds| now + chrono::Duration::seconds(seconds)),
            granted_scopes: split_scopes(response.scope.as_deref().unwrap_or_default()),
        };
        credential.validate(true)?;
        Ok(credential)
    }

    fn refreshed(&self, response: TokenResponse, now: DateTime<Utc>) -> Result<Self> {
        let credential = Self {
            access_token: response.access_token,
            refresh_token: response
                .refresh_token
                .or_else(|| self.refresh_token.clone()),
            access_token_expires_at: now + chrono::Duration::seconds(response.expires_in),
            refresh_token_expires_at: response
                .refresh_token_expires_in
                .map(|seconds| now + chrono::Duration::seconds(seconds))
                .or(self.refresh_token_expires_at),
            granted_scopes: response
                .scope
                .as_deref()
                .map(split_scopes)
                .unwrap_or_else(|| self.granted_scopes.clone()),
        };
        credential.validate(false)?;
        Ok(credential)
    }

    fn validate(&self, require_refresh_token: bool) -> Result<()> {
        if self.access_token.trim().is_empty()
            || require_refresh_token && self.refresh_token.as_deref().is_none_or(str::is_empty)
        {
            return Err(malformed("Google OAuth response omitted a required token"));
        }
        if !self
            .granted_scopes
            .iter()
            .any(|scope| scope == CALENDAR_SCOPE)
        {
            return Err(GlanceletError::OAuth(
                "Google Calendar read permission was not granted".into(),
            ));
        }
        Ok(())
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.access_token_expires_at
    }

    pub fn granted_scopes(&self) -> &[String] {
        &self.granted_scopes
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GoogleIdentity {
    pub sub: String,
    pub email: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleCalendar {
    pub id: String,
    pub summary: String,
    #[serde(default)]
    pub summary_override: Option<String>,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub selected: bool,
}

impl GoogleCalendar {
    pub fn display_name(&self) -> &str {
        self.summary_override.as_deref().unwrap_or(&self.summary)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GoogleEventsQuery {
    pub timezone: String,
    pub time_min: Option<String>,
    pub time_max: Option<String>,
    pub sync_token: Option<String>,
    pub page_token: Option<String>,
}

#[derive(Debug)]
pub(crate) enum GoogleEventsError {
    FullSyncRequired,
    Other(GlanceletError),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoogleEventsPage {
    #[serde(default)]
    pub items: Vec<GoogleEvent>,
    pub next_page_token: Option<String>,
    pub next_sync_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoogleEvent {
    pub id: String,
    #[serde(default = "confirmed")]
    pub status: String,
    pub summary: Option<String>,
    pub updated: Option<String>,
    pub html_link: Option<String>,
    #[serde(default = "default_event_type")]
    pub event_type: String,
    pub start: Option<GoogleEventTime>,
    pub end: Option<GoogleEventTime>,
    #[serde(default)]
    pub end_time_unspecified: bool,
    pub recurring_event_id: Option<String>,
    pub original_start_time: Option<GoogleEventTime>,
    #[serde(default)]
    pub attendees: Vec<GoogleAttendee>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoogleEventTime {
    pub date: Option<String>,
    pub date_time: Option<String>,
    pub time_zone: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GoogleAttendee {
    #[serde(default)]
    #[serde(rename = "self")]
    pub self_: bool,
    pub response_status: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
    refresh_token_expires_in: Option<i64>,
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalendarListPage {
    #[serde(default)]
    items: Vec<GoogleCalendar>,
    next_page_token: Option<String>,
}

#[derive(Clone, Copy)]
enum RequestKind {
    OAuth,
    Api,
}

async fn decode_response<T: DeserializeOwned>(
    response: reqwest::Response,
    kind: RequestKind,
) -> Result<T> {
    let status = response.status();
    if status.is_success() {
        return response.json().await.map_err(network_error);
    }
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok());
    let error = response.json::<serde_json::Value>().await.ok();
    let oauth_code = error.as_ref().and_then(|value| {
        value["error"]
            .as_str()
            .or_else(|| value["error"]["error"].as_str())
    });
    let rate_limited = status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::FORBIDDEN
            && error.as_ref().is_some_and(|value| {
                value["error"]["errors"].as_array().is_some_and(|errors| {
                    errors.iter().any(|detail| {
                        matches!(
                            detail["reason"].as_str(),
                            Some("rateLimitExceeded" | "quotaExceeded")
                        )
                    })
                })
            });
    if rate_limited {
        return Err(GlanceletError::RateLimited {
            provider: "Google Calendar".into(),
            retry_after_seconds: retry_after.unwrap_or(60),
        });
    }
    if status == StatusCode::UNAUTHORIZED
        || matches!(oauth_code, Some("invalid_grant" | "invalid_client"))
    {
        return Err(GlanceletError::AuthenticationRequired(
            "Google connection must be authorized again".into(),
        ));
    }
    if matches!(kind, RequestKind::OAuth) {
        return Err(GlanceletError::OAuth(format!(
            "Google rejected the OAuth request ({})",
            oauth_code.unwrap_or("oauth_error")
        )));
    }
    if status == StatusCode::FORBIDDEN {
        return Err(GlanceletError::Source(
            "Google denied access to this Calendar operation".into(),
        ));
    }
    if status == StatusCode::NOT_FOUND {
        return Err(GlanceletError::Source(
            "Google Calendar is unavailable or inaccessible".into(),
        ));
    }
    if status.is_server_error() {
        return Err(GlanceletError::Source(
            "Google Calendar is temporarily unavailable".into(),
        ));
    }
    Err(GlanceletError::Source(format!(
        "Google Calendar rejected the request ({status})"
    )))
}

fn split_scopes(scopes: &str) -> Vec<String> {
    scopes.split_whitespace().map(str::to_owned).collect()
}

fn confirmed() -> String {
    "confirmed".into()
}

fn default_event_type() -> String {
    "default".into()
}

fn malformed(message: &str) -> GlanceletError {
    GlanceletError::Source(message.into())
}

fn network_error(error: reqwest::Error) -> GlanceletError {
    if error.is_timeout() {
        GlanceletError::Source("Google request timed out".into())
    } else {
        GlanceletError::Source("Google network request failed".into())
    }
}
