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
use chrono::{DateTime, Days, LocalResult, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{
    application::{Clock, SecretStore, TimeContext},
    domain::{
        ProgressAuthority, ProviderId, SourceBatch, SourceBatchKind, SourceChange, SourceEntity,
        SourceIdentity, SourceMutation, SourceRecord, SourceTypeId, TemporalValue, WorkBindingMode,
        WorkDraft, WorkKind,
    },
    extension::{
        ProviderRegistration, SourceAdapter, SourceConfig, SourceDescriptor, SourceRegistration,
        WorkProjector,
    },
    GlanceletError, Result,
};

pub const PROVIDER_ID: &str = "google";
pub const SOURCE_TYPE: &str = "google.calendar";
pub const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";
pub const DEFAULT_SYNC_INTERVAL_SECONDS: i64 = 300;
const WINDOW_PAST_DAYS: u64 = 7;
const WINDOW_FUTURE_DAYS: u64 = 90;
const CHECKPOINT_VERSION: u8 = 1;

pub fn credential_key(connection_id: &str) -> String {
    format!("google:{connection_id}")
}

pub fn registration(
    client: Arc<GoogleApiClient>,
    tokens: Arc<GoogleTokenProvider>,
    clock: Arc<dyn Clock>,
    time_context: TimeContext,
) -> ProviderRegistration {
    ProviderRegistration {
        provider_id: ProviderId(PROVIDER_ID.into()),
        display_name: "Google Calendar".into(),
        sources: vec![SourceRegistration {
            descriptor: SourceDescriptor {
                source_type_id: SourceTypeId(SOURCE_TYPE.into()),
                display_name: "Calendar".into(),
                description: "Mirror occurrences from a selected Google Calendar".into(),
            },
            adapter: Arc::new(GoogleCalendarAdapter {
                client,
                tokens,
                clock,
                time_context,
            }),
            projector: Arc::new(GoogleCalendarProjector),
        }],
    }
}

pub struct GoogleTokenProvider {
    client_id: String,
    client: Arc<GoogleApiClient>,
    secrets: Arc<dyn SecretStore>,
    clock: Arc<dyn Clock>,
    locks: std::sync::Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

impl GoogleTokenProvider {
    pub fn new(
        client_id: impl Into<String>,
        client: Arc<GoogleApiClient>,
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

    pub fn save(&self, connection_id: &str, credential: &GoogleCredential) -> Result<()> {
        let value = serde_json::to_string(credential).map_err(|_| {
            GlanceletError::SecretStoreUnavailable("cannot encode Google credential".into())
        })?;
        self.secrets.set(&credential_key(connection_id), &value)
    }

    pub fn delete(&self, connection_id: &str) -> Result<()> {
        self.secrets.delete(&credential_key(connection_id))
    }

    pub async fn access_token(&self, connection_id: &str) -> Result<String> {
        let key = credential_key(connection_id);
        let lock = {
            let mut locks = self.locks.lock().expect("Google token lock map poisoned");
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
            GlanceletError::AuthenticationRequired("Google credential is missing".into())
        })?;
        let credential: GoogleCredential = serde_json::from_str(&raw).map_err(|_| {
            GlanceletError::AuthenticationRequired("Google credential is invalid".into())
        })?;
        if credential.expires_at() > self.clock.now() + chrono::Duration::minutes(5) {
            return Ok(credential.access_token().to_owned());
        }
        if self.client_id.trim().is_empty() {
            return Err(GlanceletError::AuthenticationRequired(
                "Google OAuth client ID is required to refresh this connection".into(),
            ));
        }
        let replacement = self
            .client
            .refresh_token(&self.client_id, &credential, self.clock.now())
            .await?;
        let token = replacement.access_token().to_owned();
        self.save(connection_id, &replacement)?;
        Ok(token)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoogleCalendarSettings {
    pub calendar_id: String,
    pub display_name: String,
}

pub fn matches_source_config(
    config: &SourceConfig,
    connection_id: &str,
    calendar_id: &str,
) -> bool {
    config.connection_id == connection_id
        && config.source_type_id.0 == SOURCE_TYPE
        && config.settings["calendar_id"].as_str() == Some(calendar_id)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GoogleCalendarCheckpoint {
    version: u8,
    sync_token: String,
    window_start: NaiveDate,
    window_end: NaiveDate,
    query_timezone: String,
    last_full_reconcile_local_date: NaiveDate,
}

struct GoogleCalendarAdapter {
    client: Arc<GoogleApiClient>,
    tokens: Arc<GoogleTokenProvider>,
    clock: Arc<dyn Clock>,
    time_context: TimeContext,
}

#[async_trait]
impl SourceAdapter for GoogleCalendarAdapter {
    async fn fetch(&self, config: &SourceConfig, checkpoint: Option<Value>) -> Result<SourceBatch> {
        let settings: GoogleCalendarSettings = serde_json::from_value(config.settings.clone())
            .map_err(|_| GlanceletError::Source("invalid Google Calendar settings".into()))?;
        if settings.calendar_id.trim().is_empty() {
            return Err(GlanceletError::Source(
                "Google Calendar settings omitted calendar identity".into(),
            ));
        }
        let token = self.tokens.access_token(&config.connection_id).await?;
        let now = self.clock.now();
        let today = self.time_context.local_date(now);
        let timezone = self.time_context.timezone().name().to_owned();
        let checkpoint = checkpoint
            .map(serde_json::from_value::<GoogleCalendarCheckpoint>)
            .transpose()
            .map_err(|_| GlanceletError::Source("invalid Google Calendar checkpoint".into()))?;
        let requires_full = checkpoint.as_ref().is_none_or(|checkpoint| {
            checkpoint.version != CHECKPOINT_VERSION
                || checkpoint.query_timezone != timezone
                || checkpoint.last_full_reconcile_local_date != today
        });
        if requires_full {
            return self
                .full_snapshot(&token, &settings.calendar_id, today, &timezone)
                .await;
        }
        let checkpoint = checkpoint.expect("checkpoint checked above");
        match self
            .delta(&token, &settings.calendar_id, &checkpoint, &timezone)
            .await
        {
            Ok(batch) => Ok(batch),
            Err(GoogleEventsError::FullSyncRequired) => {
                self.full_snapshot(&token, &settings.calendar_id, today, &timezone)
                    .await
            }
            Err(GoogleEventsError::Other(error)) => Err(error),
        }
    }
}

impl GoogleCalendarAdapter {
    async fn full_snapshot(
        &self,
        token: &str,
        calendar_id: &str,
        today: NaiveDate,
        timezone: &str,
    ) -> Result<SourceBatch> {
        let window = projection_window(today, self.time_context)?;
        let mut page_token = None;
        let mut mutations = Vec::new();
        let next_sync_token = loop {
            let query = GoogleEventsQuery {
                timezone: timezone.into(),
                time_min: Some(rfc3339(window.start)),
                time_max: Some(rfc3339(window.end)),
                sync_token: None,
                page_token: page_token.clone(),
            };
            let page = self
                .client
                .events_page(token, calendar_id, &query)
                .await
                .map_err(|error| match error {
                    GoogleEventsError::FullSyncRequired => GlanceletError::Source(
                        "Google unexpectedly rejected a full Calendar sync".into(),
                    ),
                    GoogleEventsError::Other(error) => error,
                })?;
            for event in page.items {
                if let Some(record) = map_active_event(event, calendar_id, &window)? {
                    mutations.push(SourceMutation::Upsert(record));
                }
            }
            match page.next_page_token {
                Some(next) if !next.is_empty() => page_token = Some(next),
                _ => {
                    break page.next_sync_token.ok_or_else(|| {
                        GlanceletError::Source(
                            "Google Calendar full sync omitted nextSyncToken".into(),
                        )
                    })?;
                }
            }
        };
        sort_mutations(&mut mutations);
        let checkpoint = GoogleCalendarCheckpoint {
            version: CHECKPOINT_VERSION,
            sync_token: next_sync_token,
            window_start: window.start_date,
            window_end: window.end_date,
            query_timezone: timezone.into(),
            last_full_reconcile_local_date: today,
        };
        Ok(SourceBatch {
            kind: SourceBatchKind::FullSnapshot,
            mutations,
            next_checkpoint: Some(serde_json::to_value(checkpoint).map_err(|_| {
                GlanceletError::Source("cannot encode Google Calendar checkpoint".into())
            })?),
        })
    }

    async fn delta(
        &self,
        token: &str,
        calendar_id: &str,
        checkpoint: &GoogleCalendarCheckpoint,
        timezone: &str,
    ) -> std::result::Result<SourceBatch, GoogleEventsError> {
        let window = ProjectionWindow {
            start_date: checkpoint.window_start,
            end_date: checkpoint.window_end,
            start: local_midnight(checkpoint.window_start, self.time_context)
                .map_err(GoogleEventsError::Other)?,
            end: local_midnight(checkpoint.window_end, self.time_context)
                .map_err(GoogleEventsError::Other)?,
        };
        let mut page_token = None;
        let mut mutations = Vec::new();
        let next_sync_token = loop {
            let query = GoogleEventsQuery {
                timezone: timezone.into(),
                time_min: None,
                time_max: None,
                sync_token: Some(checkpoint.sync_token.clone()),
                page_token: page_token.clone(),
            };
            let page = self.client.events_page(token, calendar_id, &query).await?;
            for event in page.items {
                let identity = event_identity(&event).map_err(GoogleEventsError::Other)?;
                if event.status == "cancelled"
                    || !is_supported(&event)
                    || is_declined(&event)
                    || !event_in_window(&event, &window).map_err(GoogleEventsError::Other)?
                {
                    mutations.push(SourceMutation::Deactivate(identity));
                } else if let Some(record) = map_active_event(event, calendar_id, &window)
                    .map_err(GoogleEventsError::Other)?
                {
                    mutations.push(SourceMutation::Upsert(record));
                }
            }
            match page.next_page_token {
                Some(next) if !next.is_empty() => page_token = Some(next),
                _ => {
                    break page.next_sync_token.ok_or_else(|| {
                        GoogleEventsError::Other(GlanceletError::Source(
                            "Google Calendar delta omitted nextSyncToken".into(),
                        ))
                    })?;
                }
            }
        };
        sort_mutations(&mut mutations);
        let mut next = checkpoint.clone();
        next.sync_token = next_sync_token;
        Ok(SourceBatch {
            kind: SourceBatchKind::Delta,
            mutations,
            next_checkpoint: Some(serde_json::to_value(next).map_err(|_| {
                GoogleEventsError::Other(GlanceletError::Source(
                    "cannot encode Google Calendar checkpoint".into(),
                ))
            })?),
        })
    }
}

struct GoogleCalendarProjector;

impl WorkProjector for GoogleCalendarProjector {
    fn project(&self, entity: &SourceEntity, _change: &SourceChange) -> Result<WorkDraft> {
        let temporal: CalendarTemporalMetadata = serde_json::from_value(
            entity
                .metadata
                .get("temporal")
                .cloned()
                .unwrap_or(Value::Null),
        )
        .map_err(|_| GlanceletError::Source("invalid Google Calendar temporal metadata".into()))?;
        Ok(WorkDraft {
            kind: WorkKind::Event,
            title: entity.title.clone(),
            summary: None,
            priority: None,
            progress: None,
            start: Some(temporal.start),
            end: temporal.end,
            due: None,
            dimensions: json!({}),
            facets: json!({}),
            binding_mode: WorkBindingMode::Mirror,
            progress_authority: ProgressAuthority::None,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct CalendarTemporalMetadata {
    start: TemporalValue,
    end: Option<TemporalValue>,
}

#[derive(Clone, Copy)]
struct ProjectionWindow {
    start_date: NaiveDate,
    end_date: NaiveDate,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

fn projection_window(today: NaiveDate, time_context: TimeContext) -> Result<ProjectionWindow> {
    let start_date = today
        .checked_sub_days(Days::new(WINDOW_PAST_DAYS))
        .ok_or_else(|| GlanceletError::Source("Google Calendar window underflow".into()))?;
    let end_date = today
        .checked_add_days(Days::new(WINDOW_FUTURE_DAYS + 1))
        .ok_or_else(|| GlanceletError::Source("Google Calendar window overflow".into()))?;
    Ok(ProjectionWindow {
        start_date,
        end_date,
        start: local_midnight(start_date, time_context)?,
        end: local_midnight(end_date, time_context)?,
    })
}

fn local_midnight(date: NaiveDate, time_context: TimeContext) -> Result<DateTime<Utc>> {
    let start = date.and_hms_opt(0, 0, 0).expect("midnight is valid");
    // A small set of historical timezone transitions skipped local midnight—or
    // an entire local date. Calendar windows still need a deterministic boundary,
    // so advance to the first representable instant at or after the requested date.
    for minutes in 0..=48 * 60 {
        let local = start + chrono::Duration::minutes(minutes);
        match time_context.timezone().from_local_datetime(&local) {
            LocalResult::Single(value) => return Ok(value.with_timezone(&Utc)),
            LocalResult::Ambiguous(first, second) => {
                return Ok(first.min(second).with_timezone(&Utc));
            }
            LocalResult::None => {}
        }
    }
    Err(GlanceletError::Source(
        "local timezone has no valid Calendar window boundary".into(),
    ))
}

fn map_active_event(
    event: GoogleEvent,
    calendar_id: &str,
    window: &ProjectionWindow,
) -> Result<Option<SourceRecord>> {
    if event.status == "cancelled"
        || !is_supported(&event)
        || is_declined(&event)
        || !event_in_window(&event, window)?
    {
        return Ok(None);
    }
    let identity = event_identity(&event)?;
    let start = event
        .start
        .as_ref()
        .ok_or_else(|| malformed_event("start"))
        .and_then(temporal_value)?;
    // Google still supplies a compatibility end when endTimeUnspecified is true.
    // Persisting it as a real range would incorrectly make the event ongoing.
    let end = if event.end_time_unspecified {
        None
    } else {
        Some(
            event
                .end
                .as_ref()
                .ok_or_else(|| malformed_event("end"))
                .and_then(temporal_value)?,
        )
    };
    let title = event
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Busy")
        .to_owned();
    let navigation = event
        .html_link
        .as_deref()
        .filter(|link| link.starts_with("https://"))
        .map(|link| json!({ "web_url": link }))
        .unwrap_or_else(|| json!({}));
    let temporal = CalendarTemporalMetadata { start, end };
    let metadata = json!({
        "calendar_id": calendar_id,
        "event_type": event.event_type,
        "recurring_event_id": event.recurring_event_id,
        "temporal": temporal,
    });
    let revision_material = json!({
        "updated": event.updated,
        "title": title,
        "metadata": metadata,
        "navigation": navigation,
    });
    let encoded = serde_json::to_vec(&revision_material)
        .map_err(|_| GlanceletError::Source("cannot normalize Google Calendar event".into()))?;
    let revision = URL_SAFE_NO_PAD.encode(Sha256::digest(encoded));
    Ok(Some(SourceRecord {
        identity,
        title,
        revision,
        display: json!({}),
        metadata,
        navigation,
    }))
}

fn event_identity(event: &GoogleEvent) -> Result<SourceIdentity> {
    let external_id = match (&event.recurring_event_id, &event.original_start_time) {
        (Some(parent), Some(original)) => {
            format!("recurring:{parent}:{}", canonical_original_start(original)?)
        }
        _ => event.id.clone(),
    };
    if external_id.trim().is_empty() {
        return Err(malformed_event("identity"));
    }
    Ok(SourceIdentity {
        entity_type: "calendar_event".into(),
        external_id,
    })
}

fn canonical_original_start(value: &GoogleEventTime) -> Result<String> {
    if let Some(date) = value.date.as_deref() {
        let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|_| malformed_event("originalStartTime.date"))?;
        return Ok(format!("date:{date}"));
    }
    let temporal = temporal_value(value)?;
    match temporal {
        TemporalValue::DateTime { instant, .. } => Ok(format!(
            "instant:{}",
            instant.to_rfc3339_opts(SecondsFormat::Secs, true)
        )),
        TemporalValue::Date { .. } => unreachable!("date handled above"),
    }
}

fn temporal_value(value: &GoogleEventTime) -> Result<TemporalValue> {
    match (value.date.as_deref(), value.date_time.as_deref()) {
        (Some(date), None) => Ok(TemporalValue::Date {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map_err(|_| malformed_event("date"))?,
        }),
        (None, Some(date_time)) => {
            let instant = if let Ok(parsed) = DateTime::parse_from_rfc3339(date_time) {
                parsed.with_timezone(&Utc)
            } else {
                let timezone = value
                    .time_zone
                    .as_deref()
                    .ok_or_else(|| malformed_event("dateTime offset"))?
                    .parse::<chrono_tz::Tz>()
                    .map_err(|_| malformed_event("timeZone"))?;
                let local = NaiveDateTime::parse_from_str(date_time, "%Y-%m-%dT%H:%M:%S%.f")
                    .map_err(|_| malformed_event("dateTime"))?;
                match timezone.from_local_datetime(&local) {
                    LocalResult::Single(value) => value.with_timezone(&Utc),
                    _ => return Err(malformed_event("ambiguous dateTime")),
                }
            };
            Ok(TemporalValue::DateTime {
                instant,
                timezone: value.time_zone.clone(),
            })
        }
        _ => Err(malformed_event("date/dateTime")),
    }
}

fn event_in_window(event: &GoogleEvent, window: &ProjectionWindow) -> Result<bool> {
    let Some(start) = event.start.as_ref() else {
        return Ok(false);
    };
    if event.end_time_unspecified {
        return match temporal_value(start)? {
            TemporalValue::Date { date } => Ok(date >= window.start_date && date < window.end_date),
            TemporalValue::DateTime { instant, .. } => {
                Ok(instant >= window.start && instant < window.end)
            }
        };
    }
    let Some(end) = event.end.as_ref() else {
        return Ok(false);
    };
    match (temporal_value(start)?, temporal_value(end)?) {
        (TemporalValue::Date { date: start }, TemporalValue::Date { date: end }) => {
            Ok(start < window.end_date && end > window.start_date)
        }
        (
            TemporalValue::DateTime { instant: start, .. },
            TemporalValue::DateTime { instant: end, .. },
        ) => Ok(start < window.end && end > window.start),
        _ => Err(malformed_event("mixed start/end temporal types")),
    }
}

fn is_supported(event: &GoogleEvent) -> bool {
    matches!(
        event.event_type.as_str(),
        "default" | "focusTime" | "outOfOffice"
    )
}

fn is_declined(event: &GoogleEvent) -> bool {
    event
        .attendees
        .iter()
        .any(|attendee| attendee.self_ && attendee.response_status.as_deref() == Some("declined"))
}

fn rfc3339(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn sort_mutations(mutations: &mut [SourceMutation]) {
    mutations.sort_by(|left, right| mutation_identity(left).cmp(mutation_identity(right)));
}

fn mutation_identity(mutation: &SourceMutation) -> &str {
    match mutation {
        SourceMutation::Upsert(record) => &record.identity.external_id,
        SourceMutation::Deactivate(identity) => &identity.external_id,
    }
}

fn malformed_event(field: &str) -> GlanceletError {
    GlanceletError::Source(format!(
        "Google Calendar returned an event with invalid {field}"
    ))
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone, Utc};

    use super::local_midnight;
    use crate::application::TimeContext;

    #[test]
    fn missing_local_date_advances_to_the_first_valid_instant() {
        let boundary = local_midnight(
            NaiveDate::from_ymd_opt(2011, 12, 30).unwrap(),
            TimeContext::named("Pacific/Apia").unwrap(),
        )
        .unwrap();
        assert_eq!(
            boundary,
            Utc.with_ymd_and_hms(2011, 12, 30, 10, 0, 0)
                .single()
                .unwrap()
        );
    }
}
