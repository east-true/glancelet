use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use chrono::{TimeZone, Utc};
use glancelet_core::{
    application::{
        Clock, FixedClock, InMemorySecretStore, NavigationService, SecretStore,
        SourceChangeProcessor, SyncCoordinator, TimeContext, WorkAction, WorkCommandService,
        WorkReadService, WorkStore,
    },
    domain::{ProviderId, SourceTypeId, TemporalValue, WorkLifecycle},
    extension::{Connection, ExtensionRegistry, SourceConfig},
    sources::google::{
        self, GoogleApiClient, GoogleCalendarSettings, GoogleOAuthService, GoogleTokenProvider,
        CALENDAR_SCOPE, DEFAULT_SYNC_INTERVAL_SECONDS, PROVIDER_ID, SOURCE_TYPE,
    },
    storage::SqliteWorkStore,
};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct MockResponse {
    status: u16,
    headers: Vec<(&'static str, &'static str)>,
    body: String,
}

impl MockResponse {
    fn ok(body: Value) -> Self {
        Self {
            status: 200,
            headers: vec![],
            body: body.to_string(),
        }
    }

    fn status(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: vec![],
            body: body.to_string(),
        }
    }
}

struct MockGoogle {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockGoogle {
    async fn start(responses: Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                if responses.lock().unwrap().is_empty() {
                    break;
                }
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 4096];
                let header_end = loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(position) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        break position + 4;
                    }
                };
                let header = String::from_utf8_lossy(&bytes[..header_end]);
                let content_length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + content_length {
                    let read = socket.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&bytes).into_owned());
                let response = responses.lock().unwrap().pop_front().unwrap();
                let headers = response
                    .headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect::<String>();
                let wire = format!(
                    "HTTP/1.1 {} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                    response.status, response.body.len(), headers, response.body
                );
                socket.write_all(wire.as_bytes()).await.unwrap();
            }
        });
        Self {
            base_url: format!("http://{address}"),
            requests,
            task,
        }
    }

    fn client(&self) -> Arc<GoogleApiClient> {
        Arc::new(GoogleApiClient::new(
            Client::new(),
            &self.base_url,
            format!("{}/token", self.base_url),
            format!("{}/userinfo", self.base_url),
        ))
    }

    async fn requests(&self) -> Vec<String> {
        tokio::task::yield_now().await;
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockGoogle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Harness {
    store: Arc<SqliteWorkStore>,
    clock: Arc<FixedClock>,
    sync: SyncCoordinator,
    changes: SourceChangeProcessor,
    reads: WorkReadService,
    commands: WorkCommandService,
}

impl Harness {
    fn new(mock: &MockGoogle, timezone: &str) -> Self {
        let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
        let clock = Arc::new(FixedClock::new(now()));
        let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
        secrets
            .set(
                &google::credential_key("google-account"),
                &credential(now() + chrono::Duration::days(365)),
            )
            .unwrap();
        let client = mock.client();
        let clock_port: Arc<dyn Clock> = clock.clone();
        let tokens = Arc::new(GoogleTokenProvider::new(
            "client",
            client.clone(),
            secrets,
            clock_port.clone(),
        ));
        let time_context = TimeContext::named(timezone).unwrap();
        let mut registry = ExtensionRegistry::new();
        registry
            .register(google::registration(
                client,
                tokens,
                clock_port.clone(),
                time_context,
            ))
            .unwrap();
        let registry = Arc::new(registry);
        let store_port: Arc<dyn WorkStore> = store.clone();
        store
            .put_connection(&Connection {
                id: "google-account".into(),
                provider_id: ProviderId(PROVIDER_ID.into()),
                display_name: "user@example.com".into(),
                config: json!({"sub":"subject-1"}),
            })
            .unwrap();
        Self {
            sync: SyncCoordinator::new(store_port.clone(), registry.clone(), clock_port.clone()),
            changes: SourceChangeProcessor::new(
                store_port.clone(),
                registry.clone(),
                clock_port.clone(),
            ),
            reads: WorkReadService::new(store_port, registry, clock_port, time_context),
            commands: WorkCommandService::new(store.clone(), clock.clone()),
            store,
            clock,
        }
    }

    fn add_calendar(&self, id: &str, calendar_id: &str) {
        self.store
            .put_source_config(&SourceConfig {
                id: id.into(),
                connection_id: "google-account".into(),
                source_type_id: SourceTypeId(SOURCE_TYPE.into()),
                display_name: calendar_id.into(),
                enabled: true,
                removed_at: None,
                expected_sync_interval_seconds: DEFAULT_SYNC_INTERVAL_SECONDS,
                settings: serde_json::to_value(GoogleCalendarSettings {
                    calendar_id: calendar_id.into(),
                    display_name: calendar_id.into(),
                })
                .unwrap(),
            })
            .unwrap();
    }

    async fn sync_and_project(&self, id: &str) -> usize {
        let changed = self.sync.sync(id).await.unwrap();
        self.changes.process_pending(100).unwrap();
        changed
    }
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 11, 1, 0, 0).single().unwrap()
}

fn credential(expires_at: chrono::DateTime<Utc>) -> String {
    json!({
        "access_token": "access-secret", "refresh_token": "refresh-secret",
        "access_token_expires_at": expires_at, "refresh_token_expires_at": null,
        "granted_scopes": ["openid", "email", CALENDAR_SCOPE]
    })
    .to_string()
}

fn events(items: Value, page: Option<&str>, sync: Option<&str>) -> MockResponse {
    MockResponse::ok(json!({"items": items, "nextPageToken": page, "nextSyncToken": sync}))
}

fn timed(id: &str, title: &str, start: &str, end: &str) -> Value {
    json!({
        "id": id, "status": "confirmed", "summary": title,
        "updated": "2026-08-11T00:00:00Z", "eventType": "default",
        "htmlLink": format!("https://calendar.google.com/event?eid={id}"),
        "start": {"dateTime": start, "timeZone": "Asia/Seoul"},
        "end": {"dateTime": end, "timeZone": "Asia/Seoul"},
        "description": "PRIVATE-BODY-SHOULD-NOT-BE-STORED",
        "attendees": [{"email":"other@example.com", "responseStatus":"accepted"}]
    })
}

fn all_day(id: &str, title: &str, start: &str, end: &str) -> Value {
    json!({
        "id": id, "status": "confirmed", "summary": title,
        "updated": "2026-08-11T00:00:00Z", "eventType": "default",
        "htmlLink": format!("https://calendar.google.com/event?eid={id}"),
        "start": {"date": start}, "end": {"date": end}
    })
}

#[tokio::test]
async fn initial_full_snapshot_paginates_maps_temporal_values_and_opens_navigation() {
    let mock = MockGoogle::start(vec![
        events(
            json!([timed(
                "timed",
                "Standup",
                "2026-08-11T10:00:00+09:00",
                "2026-08-11T10:30:00+09:00"
            )]),
            Some("page-2"),
            None,
        ),
        events(
            json!([all_day("all-day", "Offsite", "2026-08-11", "2026-08-12")]),
            None,
            Some("sync-A"),
        ),
    ])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    assert_eq!(harness.sync_and_project("calendar-a").await, 2);

    let runtime = harness.store.source_runtime("calendar-a").unwrap();
    assert_eq!(runtime.checkpoint.as_ref().unwrap()["sync_token"], "sync-A");
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 2);
    assert!(work.iter().all(|stored| stored.binding.progress_authority
        == glancelet_core::domain::ProgressAuthority::None));
    assert!(matches!(
        work.iter()
            .find(|item| item.entry.title == "Offsite")
            .unwrap()
            .entry
            .start,
        Some(TemporalValue::Date { .. })
    ));
    let dashboard = harness.reads.dashboard().unwrap();
    assert_eq!(dashboard.today.len(), 2);
    assert!(dashboard.today.iter().all(|item| !item
        .available_actions
        .contains(&WorkAction::StartWork)
        && !item.available_actions.contains(&WorkAction::Complete)));
    let target = NavigationService::new(harness.store.clone())
        .open_source_target(&work[0].entry.id)
        .unwrap();
    assert!(target.starts_with("https://calendar.google.com/"));

    let requests = mock.requests().await;
    assert!(requests[0].contains("singleEvents=true"));
    assert!(requests[0].contains("showDeleted=true"));
    assert!(requests[0].contains("timeZone=Asia%2FSeoul"));
    assert!(requests[0].contains("timeMin="));
    assert!(requests[0].contains("timeMax="));
    assert!(requests[1].contains("pageToken=page-2"));
}

#[tokio::test]
async fn incremental_delta_updates_adds_and_deactivates_without_window_filters() {
    let mut updated = timed(
        "x",
        "Updated",
        "2026-08-11T11:00:00+09:00",
        "2026-08-11T12:00:00+09:00",
    );
    updated["updated"] = json!("2026-08-11T01:00:00Z");
    let mock = MockGoogle::start(vec![
        events(
            json!([
                timed(
                    "x",
                    "Original",
                    "2026-08-11T10:00:00+09:00",
                    "2026-08-11T11:00:00+09:00"
                ),
                timed(
                    "z",
                    "Cancelled later",
                    "2026-08-11T13:00:00+09:00",
                    "2026-08-11T14:00:00+09:00"
                )
            ]),
            None,
            Some("sync-A"),
        ),
        events(
            json!([
                updated,
                timed("y", "New", "2026-08-12T10:00:00+09:00", "2026-08-12T11:00:00+09:00"),
                {"id":"z", "status":"cancelled"}
            ]),
            None,
            Some("sync-B"),
        ),
    ])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;
    assert_eq!(harness.sync_and_project("calendar-a").await, 3);

    let stored = harness.store.stored_work().unwrap();
    assert_eq!(stored.len(), 3);
    assert_eq!(
        stored
            .iter()
            .find(|item| item.entry.title == "Updated")
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Active
    );
    assert_eq!(
        stored
            .iter()
            .find(|item| item.entry.title == "Cancelled later")
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Resolved
    );
    assert_eq!(
        harness
            .store
            .source_runtime("calendar-a")
            .unwrap()
            .checkpoint
            .unwrap()["sync_token"],
        "sync-B"
    );
    let requests = mock.requests().await;
    assert!(requests[1].contains("syncToken=sync-A"));
    assert!(!requests[1].contains("timeMin="));
    assert!(!requests[1].contains("timeMax="));
}

#[tokio::test]
async fn declined_delta_deactivates_and_accepting_again_reactivates_only_that_event() {
    let with_response = |id: &str, title: &str, response: &str| {
        let mut event = timed(
            id,
            title,
            "2026-08-11T10:00:00+09:00",
            "2026-08-11T11:00:00+09:00",
        );
        event["attendees"] = json!([{"self":true,"responseStatus":response}]);
        event
    };
    let recurring_declined = json!({
        "id":"occurrence-copy", "status":"confirmed", "eventType":"default",
        "recurringEventId":"series", "originalStartTime":{
            "dateTime":"2026-08-12T10:00:00+09:00", "timeZone":"Asia/Seoul"
        },
        "attendees":[{"self":true,"responseStatus":"declined"}]
    });
    let mock = MockGoogle::start(vec![
        events(
            json!([
                with_response("changing", "Changing", "accepted"),
                with_response("unrelated", "Unrelated", "accepted"),
                {
                    "id":"occurrence-copy", "status":"confirmed", "summary":"Recurring",
                    "eventType":"default", "updated":"2026-08-11T00:00:00Z",
                    "recurringEventId":"series", "originalStartTime":{
                        "dateTime":"2026-08-12T10:00:00+09:00", "timeZone":"Asia/Seoul"
                    },
                    "start":{"dateTime":"2026-08-12T10:00:00+09:00","timeZone":"Asia/Seoul"},
                    "end":{"dateTime":"2026-08-12T11:00:00+09:00","timeZone":"Asia/Seoul"},
                    "attendees":[{"self":true,"responseStatus":"accepted"}]
                }
            ]),
            None,
            Some("sync-A"),
        ),
        events(
            json!([
                with_response("changing", "Changing", "declined"),
                recurring_declined
            ]),
            None,
            Some("sync-B"),
        ),
        events(
            json!([with_response("changing", "Changing accepted", "accepted")]),
            None,
            Some("sync-C"),
        ),
    ])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;
    harness.sync_and_project("calendar-a").await;

    let after_decline = harness.store.stored_work().unwrap();
    assert_eq!(
        after_decline
            .iter()
            .find(|item| item.entry.title == "Changing")
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Resolved
    );
    assert_eq!(
        after_decline
            .iter()
            .find(|item| item.entry.title == "Recurring")
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Resolved
    );
    assert_eq!(
        after_decline
            .iter()
            .find(|item| item.entry.title == "Unrelated")
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Active
    );

    harness.sync_and_project("calendar-a").await;
    let after_accept = harness.store.stored_work().unwrap();
    assert_eq!(after_accept.len(), 3);
    assert_eq!(
        after_accept
            .iter()
            .find(|item| item.entry.title == "Changing accepted")
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Active
    );
}

#[tokio::test]
async fn unspecified_end_is_not_persisted_as_an_ongoing_range() {
    let mock = MockGoogle::start(vec![events(
        json!([{
            "id":"open-end", "status":"confirmed", "summary":"Open end",
            "eventType":"default", "updated":"2026-08-11T00:00:00Z",
            "htmlLink":"https://calendar.google.com/event?eid=open-end",
            "start":{"dateTime":"2026-08-11T10:00:00+09:00","timeZone":"Asia/Seoul"},
            "end":{"dateTime":"2026-08-12T10:00:00+09:00","timeZone":"Asia/Seoul"},
            "endTimeUnspecified":true
        }]),
        None,
        Some("sync-A"),
    )])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;

    let work = harness.store.stored_work().unwrap();
    assert!(work[0].entry.start.is_some());
    assert!(work[0].entry.end.is_none());
    assert_eq!(harness.reads.dashboard().unwrap().today.len(), 1);
    harness
        .clock
        .set(Utc.with_ymd_and_hms(2026, 8, 12, 1, 0, 0).single().unwrap());
    assert!(harness.reads.dashboard().unwrap().today.is_empty());
}

#[tokio::test]
async fn calendar_updates_preserve_dismiss_snooze_and_pin() {
    let initial = |id: &str| {
        timed(
            id,
            &format!("Initial {id}"),
            "2026-08-11T10:00:00+09:00",
            "2026-08-11T11:00:00+09:00",
        )
    };
    let updated = |id: &str| {
        timed(
            id,
            &format!("Updated {id}"),
            "2026-08-11T12:00:00+09:00",
            "2026-08-11T13:00:00+09:00",
        )
    };
    let mock = MockGoogle::start(vec![
        events(
            json!([initial("dismiss"), initial("snooze"), initial("pin")]),
            None,
            Some("sync-A"),
        ),
        events(
            json!([updated("dismiss"), updated("snooze"), updated("pin")]),
            None,
            Some("sync-B"),
        ),
    ])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;
    let work = harness.store.stored_work().unwrap();
    let id = |suffix: &str| {
        work.iter()
            .find(|item| item.entry.title == format!("Initial {suffix}"))
            .unwrap()
            .entry
            .id
            .clone()
    };
    harness.commands.dismiss(&id("dismiss")).unwrap();
    harness
        .commands
        .snooze(&id("snooze"), now() + chrono::Duration::hours(2))
        .unwrap();
    harness.commands.pin(&id("pin")).unwrap();
    harness.sync_and_project("calendar-a").await;
    let work = harness.store.stored_work().unwrap();
    assert_eq!(
        work.iter()
            .find(|item| item.entry.title == "Updated dismiss")
            .unwrap()
            .entry
            .disposition,
        glancelet_core::domain::LocalDisposition::Dismissed
    );
    assert_eq!(
        work.iter()
            .find(|item| item.entry.title == "Updated snooze")
            .unwrap()
            .entry
            .disposition,
        glancelet_core::domain::LocalDisposition::Snoozed
    );
    assert!(
        work.iter()
            .find(|item| item.entry.title == "Updated pin")
            .unwrap()
            .entry
            .pinned
    );
}

#[tokio::test]
async fn delta_page_failure_does_not_advance_checkpoint_or_apply_partial_mutations() {
    let mock = MockGoogle::start(vec![
        events(
            json!([timed(
                "x",
                "Original",
                "2026-08-11T10:00:00+09:00",
                "2026-08-11T11:00:00+09:00"
            )]),
            None,
            Some("sync-A"),
        ),
        events(
            json!([timed(
                "x",
                "Partial",
                "2026-08-11T12:00:00+09:00",
                "2026-08-11T13:00:00+09:00"
            )]),
            Some("page-2"),
            None,
        ),
        MockResponse::status(500, json!({"error":{"message":"temporary"}})),
    ])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;
    assert!(harness.sync.sync("calendar-a").await.is_err());
    assert_eq!(
        harness
            .store
            .source_runtime("calendar-a")
            .unwrap()
            .checkpoint
            .unwrap()["sync_token"],
        "sync-A"
    );
    assert_eq!(
        harness.store.stored_work().unwrap()[0].entry.title,
        "Original"
    );
    assert!(harness.store.pending_source_changes(10).unwrap().is_empty());
}

#[tokio::test]
async fn gone_recovers_with_atomic_full_snapshot_and_new_token() {
    let mock = MockGoogle::start(vec![
        events(
            json!([timed(
                "old",
                "Old",
                "2026-08-11T10:00:00+09:00",
                "2026-08-11T11:00:00+09:00"
            )]),
            None,
            Some("sync-A"),
        ),
        MockResponse::status(
            410,
            json!({"error":{"code":410,"message":"fullSyncRequired"}}),
        ),
        events(
            json!([timed(
                "new",
                "New",
                "2026-08-11T12:00:00+09:00",
                "2026-08-11T13:00:00+09:00"
            )]),
            None,
            Some("sync-B"),
        ),
    ])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;
    harness.sync_and_project("calendar-a").await;
    let work = harness.store.stored_work().unwrap();
    assert_eq!(
        work.iter()
            .find(|item| item.entry.title == "Old")
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Resolved
    );
    assert_eq!(
        work.iter()
            .find(|item| item.entry.title == "New")
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Active
    );
    assert_eq!(
        harness
            .store
            .source_runtime("calendar-a")
            .unwrap()
            .checkpoint
            .unwrap()["sync_token"],
        "sync-B"
    );
    let requests = mock.requests().await;
    assert!(requests[1].contains("syncToken=sync-A"));
    assert!(requests[2].contains("timeMin="));
}

#[tokio::test]
async fn failed_full_recovery_preserves_the_old_checkpoint_and_entities() {
    let mock = MockGoogle::start(vec![
        events(
            json!([timed(
                "old",
                "Preserved",
                "2026-08-11T10:00:00+09:00",
                "2026-08-11T11:00:00+09:00"
            )]),
            None,
            Some("sync-A"),
        ),
        MockResponse::status(410, json!({"error":{"message":"fullSyncRequired"}})),
        events(json!([]), Some("page-2"), None),
        MockResponse::status(500, json!({"error":{"message":"failed recovery"}})),
    ])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;
    assert!(harness.sync.sync("calendar-a").await.is_err());
    assert_eq!(
        harness
            .store
            .source_runtime("calendar-a")
            .unwrap()
            .checkpoint
            .unwrap()["sync_token"],
        "sync-A"
    );
    assert_eq!(
        harness.store.stored_work().unwrap()[0].entry.lifecycle,
        WorkLifecycle::Active
    );
}

#[tokio::test]
async fn local_date_change_forces_bounded_reconciliation() {
    let mock = MockGoogle::start(vec![
        events(json!([]), None, Some("sync-A")),
        events(
            json!([timed(
                "future",
                "Entered window",
                "2026-11-10T10:00:00+09:00",
                "2026-11-10T11:00:00+09:00"
            )]),
            None,
            Some("sync-B"),
        ),
    ])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;
    harness
        .clock
        .set(Utc.with_ymd_and_hms(2026, 8, 12, 1, 0, 0).single().unwrap());
    harness.sync_and_project("calendar-a").await;
    let requests = mock.requests().await;
    assert!(requests[1].contains("timeMin="));
    assert!(!requests[1].contains("syncToken="));
}

#[tokio::test]
async fn restoring_a_removed_calendar_discards_its_old_sync_token() {
    let mock = MockGoogle::start(vec![
        events(json!([]), None, Some("sync-A")),
        events(json!([]), None, Some("sync-B")),
    ])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;

    let mut config = harness.store.source_config("calendar-a").unwrap();
    config.enabled = false;
    config.removed_at = Some(now());
    harness.store.put_source_config(&config).unwrap();
    config.enabled = true;
    config.removed_at = None;
    harness.store.put_source_config(&config).unwrap();
    assert!(harness
        .store
        .source_runtime("calendar-a")
        .unwrap()
        .checkpoint
        .is_none());

    harness.sync_and_project("calendar-a").await;
    let requests = mock.requests().await;
    assert!(requests[1].contains("timeMin="));
    assert!(!requests[1].contains("syncToken=sync-A"));
    assert_eq!(
        harness
            .store
            .source_runtime("calendar-a")
            .unwrap()
            .checkpoint
            .unwrap()["sync_token"],
        "sync-B"
    );
}

#[tokio::test]
async fn recurring_occurrence_identity_survives_move_and_cancellation() {
    let occurrence = |id: &str, title: &str, start: &str, original: &str| {
        json!({
            "id": id, "status":"confirmed", "summary":title, "eventType":"default",
            "updated":"2026-08-11T00:00:00Z", "htmlLink":format!("https://calendar.google.com/event?eid={id}"),
            "recurringEventId":"series-1", "originalStartTime":{"dateTime":original,"timeZone":"Asia/Seoul"},
            "start":{"dateTime":start,"timeZone":"Asia/Seoul"},
            "end":{"dateTime":"2026-08-17T11:00:00+09:00","timeZone":"Asia/Seoul"}
        })
    };
    let mock = MockGoogle::start(vec![
        events(json!([
            occurrence("instance-a", "Weekly", "2026-08-17T10:00:00+09:00", "2026-08-17T10:00:00+09:00"),
            occurrence("instance-b", "Weekly", "2026-08-18T10:00:00+09:00", "2026-08-18T10:00:00+09:00")
        ]), None, Some("sync-A")),
        events(json!([
            occurrence("instance-a-moved", "Weekly moved", "2026-08-18T15:00:00+09:00", "2026-08-17T10:00:00+09:00"),
            {"id":"instance-b", "status":"cancelled", "recurringEventId":"series-1", "originalStartTime":{"dateTime":"2026-08-18T10:00:00+09:00","timeZone":"Asia/Seoul"}}
        ]), None, Some("sync-B")),
    ]).await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;
    harness.sync_and_project("calendar-a").await;
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 2);
    assert_eq!(
        work.iter()
            .find(|item| item.entry.title == "Weekly moved")
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Active
    );
    assert_eq!(
        work.iter()
            .find(|item| item.entry.title == "Weekly")
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Resolved
    );
}

#[tokio::test]
async fn calendars_have_independent_checkpoints_and_entity_identity_scopes() {
    let mock = MockGoogle::start(vec![
        events(
            json!([timed(
                "same-id",
                "Calendar A",
                "2026-08-11T10:00:00+09:00",
                "2026-08-11T11:00:00+09:00"
            )]),
            None,
            Some("sync-A"),
        ),
        events(
            json!([timed(
                "same-id",
                "Calendar B",
                "2026-08-11T12:00:00+09:00",
                "2026-08-11T13:00:00+09:00"
            )]),
            None,
            Some("sync-B"),
        ),
        MockResponse::status(500, json!({"error":{"message":"A failed"}})),
        events(
            json!([timed(
                "same-id",
                "Calendar B updated",
                "2026-08-11T14:00:00+09:00",
                "2026-08-11T15:00:00+09:00"
            )]),
            None,
            Some("sync-B2"),
        ),
    ])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "a@example.com");
    harness.add_calendar("calendar-b", "b@example.com");
    harness.sync_and_project("calendar-a").await;
    harness.sync_and_project("calendar-b").await;
    assert!(harness.sync.sync("calendar-a").await.is_err());
    harness.sync_and_project("calendar-b").await;
    assert_eq!(harness.store.stored_work().unwrap().len(), 2);
    assert_eq!(
        harness
            .store
            .source_runtime("calendar-a")
            .unwrap()
            .checkpoint
            .unwrap()["sync_token"],
        "sync-A"
    );
    assert_eq!(
        harness
            .store
            .source_runtime("calendar-b")
            .unwrap()
            .checkpoint
            .unwrap()["sync_token"],
        "sync-B2"
    );
}

#[tokio::test]
async fn dst_and_half_open_all_day_ranges_use_the_local_time_context() {
    let mock = MockGoogle::start(vec![events(
        json!([
            {
                "id":"dst", "status":"confirmed", "summary":"DST transition", "eventType":"default",
                "updated":"2026-03-08T08:00:00Z", "htmlLink":"https://calendar.google.com/event?eid=dst",
                "start":{"dateTime":"2026-03-08T01:30:00-05:00","timeZone":"America/New_York"},
                "end":{"dateTime":"2026-03-08T03:30:00-04:00","timeZone":"America/New_York"}
            },
            all_day("one-day", "One day", "2026-03-08", "2026-03-09"),
            all_day("multi-day", "Multi day", "2026-03-07", "2026-03-10")
        ]),
        None,
        Some("sync-DST"),
    )])
    .await;
    let harness = Harness::new(&mock, "America/New_York");
    harness
        .clock
        .set(Utc.with_ymd_and_hms(2026, 3, 8, 16, 0, 0).single().unwrap());
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync_and_project("calendar-a").await;
    assert_eq!(harness.reads.dashboard().unwrap().today.len(), 3);
    harness
        .clock
        .set(Utc.with_ymd_and_hms(2026, 3, 9, 16, 0, 0).single().unwrap());
    let titles = harness
        .reads
        .dashboard()
        .unwrap()
        .today
        .into_iter()
        .map(|work| work.title)
        .collect::<Vec<_>>();
    assert_eq!(titles, vec!["Multi day"]);
    let requests = mock.requests().await;
    assert!(requests[0].contains("timeMin=2026-03-01T05%3A00%3A00Z"));
    assert!(requests[0].contains("timeZone=America%2FNew_York"));
}

#[tokio::test]
async fn unsupported_declined_and_private_payload_fields_are_not_persisted() {
    let mock = MockGoogle::start(vec![events(
        json!([
            {
                "id":"busy", "status":"confirmed", "summary":"", "eventType":"default",
                "updated":"2026-08-11T00:00:00Z", "htmlLink":"https://calendar.google.com/event?eid=busy",
                "start":{"date":"2026-08-11"}, "end":{"date":"2026-08-12"},
                "description":"PRIVATE-BODY-SHOULD-NOT-BE-STORED",
                "attendees":[{"self":true,"email":"user@example.com","responseStatus":"accepted"}],
                "conferenceData":{"entryPoints":[{"uri":"PRIVATE-MEET-LINK"}]}
            },
            {
                "id":"declined", "status":"confirmed", "summary":"Declined", "eventType":"default",
                "start":{"date":"2026-08-11"}, "end":{"date":"2026-08-12"},
                "attendees":[{"self":true,"responseStatus":"declined"}]
            },
            {"id":"birthday", "status":"confirmed", "summary":"Birthday", "eventType":"birthday", "start":{"date":"2026-08-11"}, "end":{"date":"2026-08-12"}}
        ]),
        None,
        Some("sync-A"),
    )])
    .await;
    let harness = Harness::new(&mock, "Asia/Seoul");
    harness.add_calendar("calendar-a", "work@example.com");
    harness.sync.sync("calendar-a").await.unwrap();
    let changes = harness.store.pending_source_changes(10).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].source_entity.title, "Busy");
    let snapshot = serde_json::to_string(&changes[0].source_entity).unwrap();
    assert!(!snapshot.contains("PRIVATE-BODY"));
    assert!(!snapshot.contains("PRIVATE-MEET"));
    assert!(!snapshot.contains("user@example.com"));
    assert!(!snapshot.contains("access-secret"));
    assert!(!snapshot.contains("refresh-secret"));
}

#[tokio::test]
async fn oauth_exchange_uses_userinfo_sub_and_keeps_tokens_out_of_sqlite() {
    let mock = MockGoogle::start(vec![
        MockResponse::ok(json!({
            "access_token":"oauth-access-secret", "refresh_token":"oauth-refresh-secret",
            "expires_in":3600, "scope":format!("openid email {CALENDAR_SCOPE}"),
            "unexpected_field":"ignored"
        })),
        MockResponse::ok(json!({"sub":"stable-subject", "email":"display@example.com"})),
    ])
    .await;
    let clock = Arc::new(FixedClock::new(now()));
    let service = GoogleOAuthService::new(mock.client(), clock, "https://accounts.test/authorize");
    let start = service
        .begin("desktop-client", "http://127.0.0.1:49152")
        .unwrap();
    let authorization = service.finish(&start.state, "one-time-code").await.unwrap();
    assert_eq!(authorization.identity.sub, "stable-subject");
    assert_eq!(authorization.identity.email, "display@example.com");

    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("google.db");
    let store = SqliteWorkStore::open(&path).unwrap();
    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
    let client = mock.client();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let tokens = GoogleTokenProvider::new("desktop-client", client, secrets.clone(), clock);
    tokens
        .save("connection", &authorization.credential)
        .unwrap();
    store
        .put_connection(&Connection {
            id: "connection".into(),
            provider_id: ProviderId(PROVIDER_ID.into()),
            display_name: authorization.identity.email.clone(),
            config: json!({"sub":authorization.identity.sub,"email":authorization.identity.email}),
        })
        .unwrap();
    drop(store);
    let database = std::fs::read(&path).unwrap();
    let database = String::from_utf8_lossy(&database);
    assert!(!database.contains("oauth-access-secret"));
    assert!(!database.contains("oauth-refresh-secret"));
    assert!(secrets
        .get(&google::credential_key("connection"))
        .unwrap()
        .unwrap()
        .contains("oauth-refresh-secret"));
}

#[tokio::test]
async fn discovery_refresh_is_single_flight_and_invalid_grant_requires_reauthentication() {
    let mock = MockGoogle::start(vec![
        MockResponse::ok(json!({"items":[
            {"id":"a@example.com","summary":"A"}
        ], "nextPageToken":"calendar-page-2"})),
        MockResponse::ok(json!({"items":[
            {"id":"b@example.com","summary":"B","summaryOverride":"Work"}
        ]})),
        MockResponse::ok(json!({
            "access_token":"fresh", "expires_in":3600,
            "scope":format!("openid email {CALENDAR_SCOPE}")
        })),
        MockResponse::status(400, json!({"error":"invalid_grant"})),
    ])
    .await;
    let client = mock.client();
    let calendars = client.calendars("token").await.unwrap();
    assert_eq!(calendars[1].display_name(), "Work");
    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
    secrets
        .set(
            &google::credential_key("one"),
            &credential(now() - chrono::Duration::hours(1)),
        )
        .unwrap();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let tokens = Arc::new(GoogleTokenProvider::new(
        "client",
        client,
        secrets.clone(),
        clock,
    ));
    let (left, right) = tokio::join!(tokens.access_token("one"), tokens.access_token("one"));
    assert_eq!(left.unwrap(), "fresh");
    assert_eq!(right.unwrap(), "fresh");
    secrets
        .set(
            &google::credential_key("two"),
            &credential(now() - chrono::Duration::hours(1)),
        )
        .unwrap();
    assert!(matches!(
        tokens.access_token("two").await,
        Err(glancelet_core::GlanceletError::AuthenticationRequired(_))
    ));
    let requests = mock.requests().await;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("POST /token"))
            .count(),
        2
    );
    assert!(!requests
        .iter()
        .any(|request| request.contains("client_secret")));
}

#[test]
fn removed_calendar_still_matches_its_readd_identity() {
    let config = SourceConfig {
        id: "calendar-a".into(),
        connection_id: "google-account".into(),
        source_type_id: SourceTypeId(SOURCE_TYPE.into()),
        display_name: "Work".into(),
        enabled: false,
        removed_at: Some(now()),
        expected_sync_interval_seconds: DEFAULT_SYNC_INTERVAL_SECONDS,
        settings: serde_json::to_value(GoogleCalendarSettings {
            calendar_id: "work@example.com".into(),
            display_name: "Work".into(),
        })
        .unwrap(),
    };
    assert!(google::matches_source_config(
        &config,
        "google-account",
        "work@example.com"
    ));
    assert!(!google::matches_source_config(
        &config,
        "google-account",
        "personal@example.com"
    ));
}
