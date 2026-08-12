use std::{collections::HashMap, time::Duration};

use reqwest::{header::RETRY_AFTER, Client, RequestBuilder, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::{GlanceletError, Result};

use super::{NotionPropertyMapping, NotionSourceSettings};

pub const API_VERSION: &str = "2026-03-11";

#[derive(Clone)]
pub struct NotionApiClient {
    http: Client,
    api_base: String,
}

impl NotionApiClient {
    pub fn production() -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(network_error)?;
        Ok(Self::new(http, "https://api.notion.com/v1"))
    }

    pub fn new(http: Client, api_base: impl Into<String>) -> Self {
        Self {
            http,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
        }
    }

    pub async fn identity(&self, token: &str) -> Result<NotionIdentity> {
        let raw: RawUser = self
            .send(self.request(self.http.get(self.url("users/me")), token))
            .await?;
        Ok(NotionIdentity {
            id: raw.id,
            name: raw.name.unwrap_or_else(|| "Notion user".into()),
        })
    }

    pub async fn retrieve_data_source(
        &self,
        token: &str,
        data_source_id: &str,
    ) -> Result<NotionDataSource> {
        let raw: RawDataSource = self
            .send(
                self.request(
                    self.http
                        .get(self.url(&format!("data_sources/{data_source_id}"))),
                    token,
                ),
            )
            .await?;
        raw.normalized()
    }

    pub async fn search_data_sources(
        &self,
        token: &str,
        query: Option<&str>,
    ) -> Result<Vec<NotionDataSourceSummary>> {
        let mut cursor: Option<String> = None;
        let mut sources = Vec::new();
        loop {
            let mut body = Map::new();
            body.insert("page_size".into(), json!(100));
            body.insert(
                "filter".into(),
                json!({ "property": "object", "value": "data_source" }),
            );
            if let Some(query) = query.filter(|value| !value.trim().is_empty()) {
                body.insert("query".into(), json!(query.trim()));
            }
            if let Some(value) = cursor.as_ref() {
                body.insert("start_cursor".into(), json!(value));
            }
            let page: SearchResponse = self
                .send(self.request(self.http.post(self.url("search")).json(&body), token))
                .await?;
            for value in page.results {
                if value.get("object").and_then(Value::as_str) != Some("data_source") {
                    continue;
                }
                let raw: RawDataSource = serde_json::from_value(value)
                    .map_err(|_| malformed("unexpected data source search result"))?;
                sources.push(NotionDataSourceSummary {
                    id: raw.id,
                    title: plain_text(&raw.title, "Untitled data source"),
                });
            }
            if !page.has_more {
                break;
            }
            cursor = Some(
                page.next_cursor
                    .ok_or_else(|| malformed("Notion search omitted next_cursor"))?,
            );
        }
        sources.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.id.cmp(&b.id)));
        Ok(sources)
    }

    pub(crate) async fn query_pages(
        &self,
        token: &str,
        settings: &NotionSourceSettings,
        schema: &NotionDataSource,
    ) -> Result<Vec<NotionPage>> {
        self.query_pages_limited(token, settings, schema, None, true)
            .await
    }

    pub(crate) async fn preview_pages(
        &self,
        token: &str,
        settings: &NotionSourceSettings,
        schema: &NotionDataSource,
        limit: usize,
    ) -> Result<Vec<NotionPage>> {
        self.query_pages_limited(token, settings, schema, Some(limit), false)
            .await
    }

    async fn query_pages_limited(
        &self,
        token: &str,
        settings: &NotionSourceSettings,
        schema: &NotionDataSource,
        max_results: Option<usize>,
        require_complete: bool,
    ) -> Result<Vec<NotionPage>> {
        if max_results == Some(0) {
            return Ok(Vec::new());
        }
        let filter = build_task_filter(settings, schema)?;
        let filter_properties = mapped_property_ids(settings);
        let mut cursor: Option<String> = None;
        let mut pages = Vec::new();
        loop {
            let remaining = max_results
                .map(|limit| limit.saturating_sub(pages.len()))
                .unwrap_or(100);
            if remaining == 0 {
                break;
            }
            let mut body = Map::new();
            body.insert("page_size".into(), json!(remaining.min(100)));
            body.insert("result_type".into(), json!("page"));
            if let Some(filter) = filter.clone() {
                body.insert("filter".into(), filter);
            }
            if let Some(value) = cursor.as_ref() {
                body.insert("start_cursor".into(), json!(value));
            }
            let query = filter_properties
                .iter()
                .map(|id| ("filter_properties[]", id.as_str()))
                .collect::<Vec<_>>();
            let response: QueryResponse = self
                .send(
                    self.request(
                        self.http
                            .post(
                                self.url(&format!(
                                    "data_sources/{}/query",
                                    settings.data_source_id
                                )),
                            )
                            .query(&query)
                            .json(&body),
                        token,
                    ),
                )
                .await?;
            if require_complete
                && response
                    .request_status
                    .as_ref()
                    .is_some_and(|status| status.kind == "incomplete")
            {
                return Err(GlanceletError::Source(
                    "Notion query exceeded the complete snapshot limit".into(),
                ));
            }
            pages.extend(response.results.into_iter().take(remaining));
            if max_results.is_some_and(|limit| pages.len() >= limit) || !response.has_more {
                break;
            }
            cursor = Some(
                response
                    .next_cursor
                    .ok_or_else(|| malformed("Notion query omitted next_cursor"))?,
            );
        }
        Ok(pages)
    }

    fn request(&self, builder: RequestBuilder, token: &str) -> RequestBuilder {
        builder
            .bearer_auth(token)
            .header("Notion-Version", API_VERSION)
            .header("Content-Type", "application/json")
    }

    async fn send<T: DeserializeOwned>(&self, builder: RequestBuilder) -> Result<T> {
        let response = builder.send().await.map_err(network_error)?;
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            let seconds = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|value| *value >= 0)
                .ok_or_else(|| malformed("Notion 429 omitted a valid Retry-After header"))?;
            return Err(GlanceletError::RateLimited {
                provider: "Notion".into(),
                retry_after_seconds: seconds,
            });
        }
        if status.is_success() {
            return response
                .json()
                .await
                .map_err(|_| malformed("unexpected Notion response"));
        }
        let error = response.json::<ApiError>().await.ok();
        let code = error
            .as_ref()
            .map(|error| error.code.as_str())
            .unwrap_or("unexpected_error");
        Err(api_error(status, code))
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.api_base)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionIdentity {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionDataSourceSummary {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionDataSource {
    pub id: String,
    pub title: String,
    pub properties: Vec<NotionPropertySchema>,
}

impl NotionDataSource {
    pub fn property(&self, id: &str) -> Option<&NotionPropertySchema> {
        self.properties.iter().find(|property| property.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionPropertySchema {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: Option<NotionStatusSchema>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionStatusSchema {
    pub options: Vec<NotionStatusOption>,
    pub groups: Vec<NotionStatusGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionStatusOption {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotionStatusGroup {
    pub id: String,
    pub name: String,
    #[serde(alias = "option_ids")]
    pub option_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct NotionPage {
    pub id: String,
    pub last_edited_time: String,
    pub url: String,
    #[serde(default)]
    pub properties: HashMap<String, NotionPageProperty>,
}

impl NotionPage {
    pub(crate) fn property(&self, id: &str) -> Option<&NotionPageProperty> {
        self.properties.values().find(|property| property.id == id)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct NotionPageProperty {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub title: Vec<RawRichText>,
    pub status: Option<NotionPageStatus>,
    pub date: Option<NotionPageDate>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct NotionPageStatus {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct NotionPageDate {
    pub start: String,
    pub time_zone: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawUser {
    id: String,
    name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawDataSource {
    id: String,
    #[serde(default)]
    title: Vec<RawRichText>,
    #[serde(default)]
    properties: HashMap<String, RawPropertySchema>,
}

impl RawDataSource {
    fn normalized(self) -> Result<NotionDataSource> {
        let mut properties = self
            .properties
            .into_iter()
            .map(|(key, property)| property.normalized(key))
            .collect::<Result<Vec<_>>>()?;
        properties.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        Ok(NotionDataSource {
            id: self.id,
            title: plain_text(&self.title, "Untitled data source"),
            properties,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawPropertySchema {
    id: String,
    name: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    status: Option<RawStatusSchema>,
}

impl RawPropertySchema {
    fn normalized(self, key: String) -> Result<NotionPropertySchema> {
        let status = self.status.map(|status| status.normalized());
        if self.kind == "status" && status.is_none() {
            return Err(malformed("Notion status schema is incomplete"));
        }
        Ok(NotionPropertySchema {
            id: self.id,
            name: self.name.unwrap_or(key),
            kind: self.kind,
            status,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawStatusSchema {
    #[serde(default)]
    options: Vec<NotionStatusOption>,
    #[serde(default)]
    groups: Vec<NotionStatusGroup>,
}

impl RawStatusSchema {
    fn normalized(self) -> NotionStatusSchema {
        NotionStatusSchema {
            options: self.options,
            groups: self.groups,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RawRichText {
    #[serde(default)]
    pub plain_text: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<Value>,
    has_more: bool,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct QueryResponse {
    #[serde(default)]
    results: Vec<NotionPage>,
    has_more: bool,
    next_cursor: Option<String>,
    request_status: Option<RequestStatus>,
}

#[derive(Deserialize)]
struct RequestStatus {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ApiError {
    code: String,
}

fn mapped_property_ids(settings: &NotionSourceSettings) -> Vec<String> {
    let mut ids = vec![settings.properties.title.id.clone()];
    for property in [
        settings.properties.assignee.as_ref(),
        settings.properties.status.as_ref(),
        settings.properties.due.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if !ids.contains(&property.id) {
            ids.push(property.id.clone());
        }
    }
    ids
}

fn build_task_filter(
    settings: &NotionSourceSettings,
    schema: &NotionDataSource,
) -> Result<Option<Value>> {
    let mut conditions = Vec::new();
    if settings.only_assigned_to_me {
        if let Some(assignee) = settings.properties.assignee.as_ref() {
            conditions.push(json!({
                "property": assignee.id,
                "people": { "contains": "me" }
            }));
        }
    }
    if let Some(mapping) = settings.properties.status.as_ref() {
        let property = schema
            .property(&mapping.id)
            .ok_or_else(|| needs_configuration("Status", "no longer exists"))?;
        let status = property
            .status
            .as_ref()
            .ok_or_else(|| needs_configuration("Status", "is no longer a status property"))?;
        let names = settings
            .active_status_ids
            .iter()
            .map(|id| {
                status
                    .options
                    .iter()
                    .find(|option| &option.id == id)
                    .map(|option| option.name.clone())
                    .ok_or_else(|| needs_configuration("Status", "contains a removed option"))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut status_conditions = names
            .into_iter()
            .map(|name| {
                json!({
                    "property": mapping.id,
                    "status": { "equals": name }
                })
            })
            .collect::<Vec<_>>();
        if status_conditions.len() == 1 {
            conditions.push(status_conditions.remove(0));
        } else if !status_conditions.is_empty() {
            conditions.push(json!({ "or": status_conditions }));
        }
    }
    Ok(match conditions.len() {
        0 => None,
        1 => conditions.pop(),
        _ => Some(json!({ "and": conditions })),
    })
}

pub(crate) fn plain_text(values: &[RawRichText], fallback: &str) -> String {
    let value = values
        .iter()
        .map(|value| value.plain_text.as_str())
        .collect::<String>();
    let value = value.trim();
    if value.is_empty() {
        fallback.into()
    } else {
        value.into()
    }
}

pub(crate) fn needs_configuration(field: &str, reason: &str) -> GlanceletError {
    GlanceletError::ConfigurationRequired(format!(
        "Notion source needs configuration: the mapped {field} property {reason}."
    ))
}

fn api_error(status: StatusCode, code: &str) -> GlanceletError {
    match status {
        StatusCode::UNAUTHORIZED => GlanceletError::AuthenticationRequired(
            "Notion connection must be authorized again".into(),
        ),
        StatusCode::FORBIDDEN => {
            GlanceletError::ConfigurationRequired("Notion denied access to this operation".into())
        }
        StatusCode::NOT_FOUND => GlanceletError::ConfigurationRequired(
            "Notion data source is unavailable or inaccessible".into(),
        ),
        StatusCode::BAD_REQUEST => {
            GlanceletError::ConfigurationRequired(format!("Notion rejected the request ({code})"))
        }
        StatusCode::CONFLICT => GlanceletError::ProviderFailure(
            "Notion could not complete the conflicting request".into(),
        ),
        status if status.is_server_error() => {
            GlanceletError::ProviderFailure("Notion is temporarily unavailable".into())
        }
        _ => GlanceletError::ProviderFailure(format!("Notion API error ({code})")),
    }
}

fn malformed(message: &str) -> GlanceletError {
    GlanceletError::ProviderFailure(message.into())
}

fn network_error(error: reqwest::Error) -> GlanceletError {
    if error.is_timeout() {
        GlanceletError::TransientNetwork("Notion request timed out".into())
    } else {
        GlanceletError::TransientNetwork("Notion network request failed".into())
    }
}

pub(crate) fn validate_mapping(
    schema: &NotionDataSource,
    mapping: &NotionPropertyMapping,
    expected_type: &str,
    field: &str,
) -> Result<()> {
    let property = schema
        .property(&mapping.id)
        .ok_or_else(|| needs_configuration(field, "no longer exists"))?;
    if property.kind != expected_type {
        return Err(needs_configuration(
            field,
            &format!("must have type {expected_type}"),
        ));
    }
    Ok(())
}
