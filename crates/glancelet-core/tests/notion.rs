use std::{
    collections::VecDeque,
    fs,
    sync::{Arc, Mutex},
};

use chrono::{TimeZone, Utc};
use glancelet_core::{
    application::{
        Clock, FixedClock, InMemorySecretStore, NavigationService, SecretStore,
        SourceChangeProcessor, SyncCoordinator, TimeContext, WorkAction, WorkCommandService,
        WorkReadService, WorkStore,
    },
    domain::{
        LocalDisposition, ProviderId, SourceTypeId, TemporalValue, WorkLifecycle, WorkPlanning,
        WorkProgress,
    },
    extension::{Connection, ExtensionRegistry, SourceConfig},
    sources::notion::{
        self, credential_key, NotionApiClient, NotionPropertyMapping, NotionSourceSettings,
        NotionTaskProperties, NotionTokenProvider, API_VERSION, PROVIDER_ID, SOURCE_TYPE,
    },
    storage::SqliteWorkStore,
};
use reqwest::Client;
use serde_json::{json, Value};
use tempfile::TempDir;
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

    fn rate_limited(seconds: &'static str) -> Self {
        Self {
            status: 429,
            headers: vec![("Retry-After", seconds)],
            body: json!({ "object": "error", "code": "rate_limited" }).to_string(),
        }
    }
}

struct MockNotion {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockNotion {
    async fn start(responses: Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_for_task = Arc::clone(&requests);
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
                requests_for_task
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&bytes).into_owned());
                let response = responses.lock().unwrap().pop_front().unwrap();
                let extra_headers = response
                    .headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect::<String>();
                let wire = format!(
                    "HTTP/1.1 {} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                    response.status,
                    response.body.len(),
                    extra_headers,
                    response.body
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

    fn client(&self) -> Arc<NotionApiClient> {
        Arc::new(NotionApiClient::new(Client::new(), &self.base_url))
    }

    async fn requests(&self) -> Vec<String> {
        tokio::task::yield_now().await;
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockNotion {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 11, 10, 0, 0)
        .single()
        .unwrap()
}

fn mapping(id: &str, name: &str) -> NotionPropertyMapping {
    NotionPropertyMapping {
        id: id.into(),
        name: name.into(),
    }
}

fn settings() -> NotionSourceSettings {
    NotionSourceSettings {
        data_source_id: "ds-1".into(),
        data_source_name: "Tasks".into(),
        properties: NotionTaskProperties {
            title: mapping("title", "Task"),
            assignee: Some(mapping("people-id", "Owner")),
            status: Some(mapping("status-id", "Status")),
            due: Some(mapping("due-id", "Due")),
        },
        only_assigned_to_me: true,
        active_status_ids: vec!["todo-option".into(), "doing-option".into()],
    }
}

fn schema(title_name: &str) -> MockResponse {
    MockResponse::ok(json!({
        "object": "data_source",
        "id": "ds-1",
        "title": [{ "plain_text": "Tasks" }],
        "properties": {
            title_name: { "id": "title", "name": title_name, "type": "title", "title": {} },
            "Owner": { "id": "people-id", "name": "Owner", "type": "people", "people": {} },
            "Status": {
                "id": "status-id",
                "name": "Status",
                "type": "status",
                "status": {
                    "options": [
                        { "id": "todo-option", "name": "Open" },
                        { "id": "doing-option", "name": "Working" },
                        { "id": "done-option", "name": "Done" }
                    ],
                    "groups": [
                        { "id": "todo-group", "name": "To-do", "option_ids": ["todo-option"] },
                        { "id": "doing-group", "name": "In progress", "option_ids": ["doing-option"] },
                        { "id": "done-group", "name": "Complete", "option_ids": ["done-option"] }
                    ]
                }
            },
            "Due": { "id": "due-id", "name": "Due", "type": "date", "date": {} },
            "Priority": { "id": "priority-id", "name": "Priority", "type": "select", "select": {} }
        }
    }))
}

fn task_page(
    id: &str,
    title_key: &str,
    title: &str,
    status_id: &str,
    status_name: &str,
    due: Value,
) -> Value {
    json!({
        "object": "page",
        "id": id,
        "last_edited_time": "2026-08-11T09:00:00.000Z",
        "url": format!("https://www.notion.so/{id}"),
        "properties": {
            title_key: {
                "id": "title",
                "type": "title",
                "title": [{ "plain_text": title }]
            },
            "Owner": {
                "id": "people-id",
                "type": "people",
                "people": [{ "id": "user-1" }]
            },
            "Status": {
                "id": "status-id",
                "type": "status",
                "status": { "id": status_id, "name": status_name }
            },
            "Due": { "id": "due-id", "type": "date", "date": due }
        },
        "raw_body_that_must_not_be_stored": "PRIVATE_PAGE_BODY"
    })
}

fn query(results: Vec<Value>, has_more: bool, cursor: Option<&str>) -> MockResponse {
    MockResponse::ok(json!({
        "object": "list",
        "type": "page_or_data_source",
        "results": results,
        "has_more": has_more,
        "next_cursor": cursor,
        "request_status": { "type": "complete" }
    }))
}

struct NotionHarness {
    store: Arc<SqliteWorkStore>,
    sync: SyncCoordinator,
    changes: SourceChangeProcessor,
    commands: WorkCommandService,
    reads: WorkReadService,
    navigation: NavigationService,
}

impl NotionHarness {
    fn new(client: Arc<NotionApiClient>, settings: NotionSourceSettings) -> Self {
        let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
        let secrets = Arc::new(InMemorySecretStore::new());
        let tokens = Arc::new(NotionTokenProvider::new(secrets.clone()));
        tokens.save("notion-connection", "secret_test_pat").unwrap();
        let clock = Arc::new(FixedClock::new(now()));
        let mut registry = ExtensionRegistry::new();
        registry
            .register(notion::registration(client, tokens))
            .unwrap();
        let registry = Arc::new(registry);
        store
            .put_connection(&Connection {
                id: "notion-connection".into(),
                provider_id: ProviderId(PROVIDER_ID.into()),
                display_name: "Tester".into(),
                config: json!({ "user_id": "user-1", "user_name": "Tester" }),
            })
            .unwrap();
        store
            .put_source_config(&SourceConfig {
                id: "notion-source".into(),
                connection_id: "notion-connection".into(),
                source_type_id: SourceTypeId(SOURCE_TYPE.into()),
                display_name: "Notion — Tasks".into(),
                enabled: true,
                removed_at: None,
                expected_sync_interval_seconds: 300,
                settings: serde_json::to_value(settings).unwrap(),
            })
            .unwrap();
        let store_port: Arc<dyn WorkStore> = store.clone();
        let clock_port: Arc<dyn Clock> = clock;
        Self {
            sync: SyncCoordinator::new(
                Arc::clone(&store_port),
                Arc::clone(&registry),
                Arc::clone(&clock_port),
            ),
            changes: SourceChangeProcessor::new(
                Arc::clone(&store_port),
                Arc::clone(&registry),
                Arc::clone(&clock_port),
            ),
            commands: WorkCommandService::new(Arc::clone(&store_port), Arc::clone(&clock_port)),
            reads: WorkReadService::new(
                Arc::clone(&store_port),
                registry,
                clock_port,
                TimeContext::named("UTC").unwrap(),
            ),
            navigation: NavigationService::new(store_port),
            store,
        }
    }

    async fn sync_and_project(&self) -> usize {
        let count = self.sync.sync("notion-source").await.unwrap();
        self.changes.process_pending(100).unwrap();
        count
    }
}

#[tokio::test]
async fn pat_identity_uses_current_version_and_secret_store_only() {
    let server = MockNotion::start(vec![MockResponse::ok(json!({
        "object": "user",
        "id": "user-1",
        "type": "person",
        "name": "Tester"
    }))])
    .await;
    let identity = server
        .client()
        .identity("secret_identity_pat")
        .await
        .unwrap();
    assert_eq!(identity.id, "user-1");
    let request = server.requests().await.remove(0);
    assert!(request.starts_with("GET /users/me "));
    let lower_request = request.to_ascii_lowercase();
    assert!(lower_request.contains(&format!("notion-version: {API_VERSION}")));
    assert!(lower_request.contains("authorization: bearer secret_identity_pat"));

    let secrets = Arc::new(InMemorySecretStore::new());
    let tokens = NotionTokenProvider::new(secrets.clone());
    tokens.save("connection", "secret_never_in_sqlite").unwrap();
    assert_eq!(
        secrets
            .get(&credential_key("connection"))
            .unwrap()
            .as_deref(),
        Some("secret_never_in_sqlite")
    );
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("glancelet.db");
    let store = SqliteWorkStore::open(&path).unwrap();
    store
        .put_connection(&Connection {
            id: "connection".into(),
            provider_id: ProviderId(PROVIDER_ID.into()),
            display_name: "Tester".into(),
            config: json!({ "user_id": "user-1" }),
        })
        .unwrap();
    drop(store);
    let bytes = fs::read(path).unwrap();
    assert!(!bytes
        .windows(b"secret_never_in_sqlite".len())
        .any(|window| window == b"secret_never_in_sqlite"));
}

#[tokio::test]
async fn discovery_schema_and_property_ids_survive_renames() {
    let server = MockNotion::start(vec![
        MockResponse::ok(json!({
            "object": "list",
            "type": "page_or_data_source",
            "results": [{
                "object": "data_source",
                "id": "ds-1",
                "title": [{ "plain_text": "Tasks" }],
                "properties": {}
            }],
            "has_more": false,
            "next_cursor": null
        })),
        schema("Renamed Task"),
    ])
    .await;
    let client = server.client();
    let found = client
        .search_data_sources("secret", Some("task"))
        .await
        .unwrap();
    assert_eq!(found[0].title, "Tasks");
    let retrieved = client.retrieve_data_source("secret", "ds-1").await.unwrap();
    assert_eq!(retrieved.property("title").unwrap().name, "Renamed Task");
    notion::validate_settings(&retrieved, &settings()).unwrap();
    let requests = server.requests().await;
    assert!(requests[0].contains("\"value\":\"data_source\""));
    assert!(requests[0].contains("\"query\":\"task\""));
}

#[tokio::test]
async fn query_filters_all_pages_maps_temporal_values_and_external_actions() {
    let first = task_page(
        "page-1",
        "Task",
        "First task",
        "todo-option",
        "Open",
        json!({ "start": "2026-08-15", "end": null, "time_zone": null }),
    );
    let second = task_page(
        "page-2",
        "Task",
        "Second task",
        "doing-option",
        "Working",
        json!({
            "start": "2026-08-15T14:00:00+09:00",
            "end": null,
            "time_zone": "Asia/Seoul"
        }),
    );
    let server = MockNotion::start(vec![
        schema("Task"),
        query(vec![first], true, Some("opaque-cursor")),
        query(vec![second], false, None),
    ])
    .await;
    let harness = NotionHarness::new(server.client(), settings());
    assert_eq!(harness.sync_and_project().await, 2);
    let mut work = harness.store.stored_work().unwrap();
    work.sort_by(|a, b| a.entry.title.cmp(&b.entry.title));
    assert_eq!(work.len(), 2);
    assert_eq!(
        work[0].entry.due,
        Some(TemporalValue::Date {
            date: chrono::NaiveDate::from_ymd_opt(2026, 8, 15).unwrap()
        })
    );
    assert_eq!(work[0].entry.progress, Some(WorkProgress::Todo));
    assert_eq!(work[1].entry.progress, Some(WorkProgress::Doing));
    assert!(matches!(
        work[1].entry.due,
        Some(TemporalValue::DateTime {
            timezone: Some(ref timezone),
            ..
        }) if timezone == "Asia/Seoul"
    ));
    assert_eq!(
        harness
            .navigation
            .open_source_target(&work[0].entry.id)
            .unwrap(),
        "https://www.notion.so/page-1"
    );
    let dashboard = harness.reads.dashboard().unwrap();
    let actions = &dashboard.inbox[0].available_actions;
    assert!(!actions.contains(&WorkAction::StartWork));
    assert!(!actions.contains(&WorkAction::Complete));
    assert!(actions.contains(&WorkAction::OpenSource));
    assert!(!format!("{:?}", work).contains("PRIVATE_PAGE_BODY"));

    let requests = server.requests().await;
    assert_eq!(requests.len(), 3);
    assert!(requests[1].contains("filter_properties%5B%5D=title"));
    assert!(requests[1].contains("\"contains\":\"me\""));
    assert!(requests[1].contains("\"equals\":\"Open\""));
    assert!(requests[1].contains("\"equals\":\"Working\""));
    assert!(requests[2].contains("\"start_cursor\":\"opaque-cursor\""));
}

#[tokio::test]
async fn second_page_failure_never_deactivates_or_returns_a_partial_snapshot() {
    let page = task_page(
        "page-1",
        "Task",
        "Kept task",
        "todo-option",
        "Open",
        Value::Null,
    );
    let server = MockNotion::start(vec![
        schema("Task"),
        query(vec![page.clone()], false, None),
        schema("Task"),
        query(vec![page], true, Some("page-2")),
        MockResponse::status(
            503,
            json!({ "object": "error", "code": "service_unavailable" }),
        ),
    ])
    .await;
    let harness = NotionHarness::new(server.client(), settings());
    harness.sync_and_project().await;
    let error = harness.sync.sync("notion-source").await.unwrap_err();
    assert!(error.to_string().contains("temporarily unavailable"));
    harness.changes.process_pending(100).unwrap();
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].entry.lifecycle, WorkLifecycle::Active);
    assert!(harness
        .store
        .source_runtime("notion-source")
        .unwrap()
        .last_error
        .unwrap()
        .contains("temporarily unavailable"));
}

#[tokio::test]
async fn mirror_updates_preserve_local_state_and_reactivation_resets_it() {
    let initial = task_page(
        "page-1",
        "Task",
        "Initial",
        "todo-option",
        "Open",
        json!({ "start": "2026-08-15", "time_zone": null }),
    );
    let updated = task_page(
        "page-1",
        "Task",
        "Updated",
        "doing-option",
        "Working",
        json!({ "start": "2026-08-16", "time_zone": null }),
    );
    let reactivated = task_page(
        "page-1",
        "Task",
        "Reactivated",
        "todo-option",
        "Open",
        json!({ "start": "2026-08-17", "time_zone": null }),
    );
    let server = MockNotion::start(vec![
        schema("Task"),
        query(vec![initial], false, None),
        schema("Renamed Task"),
        query(vec![updated], false, None),
        schema("Renamed Task"),
        query(vec![], false, None),
        schema("Renamed Task"),
        query(vec![reactivated], false, None),
    ])
    .await;
    let harness = NotionHarness::new(server.client(), settings());
    harness.sync_and_project().await;
    let work_id = harness.store.stored_work().unwrap()[0].entry.id.clone();
    harness
        .commands
        .plan(
            &work_id,
            chrono::NaiveDate::from_ymd_opt(2026, 8, 13).unwrap(),
        )
        .unwrap();
    harness
        .commands
        .snooze(
            &work_id,
            Utc.with_ymd_and_hms(2026, 8, 12, 10, 0, 0)
                .single()
                .unwrap(),
        )
        .unwrap();
    harness.commands.pin(&work_id).unwrap();

    harness.sync_and_project().await;
    let updated = harness.store.stored_work_by_id(&work_id).unwrap().entry;
    assert_eq!(updated.title, "Updated");
    assert_eq!(updated.progress, Some(WorkProgress::Doing));
    assert_eq!(
        updated.planning,
        Some(WorkPlanning::Planned(
            chrono::NaiveDate::from_ymd_opt(2026, 8, 13).unwrap()
        ))
    );
    assert_eq!(updated.disposition, LocalDisposition::Snoozed);
    assert!(updated.pinned);

    harness.sync_and_project().await;
    assert_eq!(
        harness
            .store
            .stored_work_by_id(&work_id)
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Resolved
    );
    harness.sync_and_project().await;
    let active = harness.store.stored_work_by_id(&work_id).unwrap().entry;
    assert_eq!(active.lifecycle, WorkLifecycle::Active);
    assert_eq!(active.title, "Reactivated");
    assert_eq!(active.planning, Some(WorkPlanning::Inbox));
    assert_eq!(active.disposition, LocalDisposition::Normal);
    assert!(active.snoozed_until.is_none());
    assert!(active.pinned);
}

#[tokio::test]
async fn optional_filters_are_omitted_and_rate_limit_controls_next_sync() {
    let mut unfiltered = settings();
    unfiltered.properties.assignee = None;
    unfiltered.only_assigned_to_me = false;
    unfiltered.properties.status = None;
    unfiltered.properties.due = None;
    unfiltered.active_status_ids.clear();
    let server = MockNotion::start(vec![
        schema("Task"),
        query(vec![], false, None),
        schema("Task"),
        MockResponse::rate_limited("19"),
    ])
    .await;
    let harness = NotionHarness::new(server.client(), unfiltered);
    harness.sync_and_project().await;
    let requests = server.requests().await;
    assert!(!requests[1].contains("\"filter\""));
    let error = harness.sync.sync("notion-source").await.unwrap_err();
    assert!(error.to_string().contains("retry after 19 seconds"));
    assert_eq!(
        harness
            .store
            .source_runtime("notion-source")
            .unwrap()
            .next_sync_at,
        Some(now() + chrono::Duration::seconds(19))
    );
}

#[tokio::test]
async fn schema_validation_rejects_missing_or_wrong_properties() {
    let server = MockNotion::start(vec![schema("Task")]).await;
    let schema = server
        .client()
        .retrieve_data_source("secret", "ds-1")
        .await
        .unwrap();
    let mut missing = settings();
    missing.properties.title.id = "missing".into();
    assert!(notion::validate_settings(&schema, &missing)
        .unwrap_err()
        .to_string()
        .contains("needs configuration"));
    let mut wrong = settings();
    wrong.properties.title.id = "due-id".into();
    assert!(notion::validate_settings(&schema, &wrong)
        .unwrap_err()
        .to_string()
        .contains("type title"));
}

#[test]
fn removed_notion_source_still_matches_its_readd_identity() {
    let config = SourceConfig {
        id: "source".into(),
        connection_id: "connection".into(),
        source_type_id: SourceTypeId(SOURCE_TYPE.into()),
        display_name: "Tasks".into(),
        enabled: false,
        removed_at: Some(now()),
        expected_sync_interval_seconds: 300,
        settings: serde_json::to_value(settings()).unwrap(),
    };
    assert!(notion::matches_source_config(&config, "connection", "ds-1"));
    assert!(!notion::matches_source_config(
        &config,
        "another-connection",
        "ds-1"
    ));
}
