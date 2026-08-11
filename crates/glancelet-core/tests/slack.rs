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
    domain::{ProviderId, SourceTypeId, WorkLifecycle, WorkProgress},
    extension::{Connection, ExtensionRegistry, SourceConfig},
    sources::slack::{
        self, credential_key, SlackApiClient, SlackOAuthService, SlackTokenProvider, PROVIDER_ID,
        SOURCE_TYPE,
    },
    storage::SqliteWorkStore,
};
use reqwest::Client;
use serde_json::json;
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
    fn ok(body: serde_json::Value) -> Self {
        Self {
            status: 200,
            headers: vec![],
            body: body.to_string(),
        }
    }

    fn status(status: u16, body: serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![],
            body: body.to_string(),
        }
    }
}

struct MockSlack {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockSlack {
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
                let reason = if response.status == 200 {
                    "OK"
                } else {
                    "Error"
                };
                let extra_headers = response
                    .headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect::<String>();
                let wire = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                    response.status,
                    reason,
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

    fn client(&self) -> Arc<SlackApiClient> {
        Arc::new(SlackApiClient::new(Client::new(), &self.base_url))
    }

    async fn requests(&self) -> Vec<String> {
        tokio::task::yield_now().await;
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockSlack {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 11, 10, 0, 0)
        .single()
        .unwrap()
}

fn page(items: serde_json::Value, cursor: &str) -> MockResponse {
    MockResponse::ok(json!({
        "ok": true,
        "items": items,
        "response_metadata": { "next_cursor": cursor }
    }))
}

fn message_item(
    channel: &str,
    ts: &str,
    text: &str,
    reaction: &str,
    users: &[&str],
) -> serde_json::Value {
    json!({
        "type": "message",
        "channel": channel,
        "message": {
            "ts": ts,
            "text": text,
            "reactions": [{ "name": reaction, "users": users }]
        }
    })
}

fn permalink(channel: &str, ts: &str) -> MockResponse {
    MockResponse::ok(json!({
        "ok": true,
        "permalink": format!("https://workspace.slack.com/archives/{channel}/p{}", ts.replace('.', ""))
    }))
}

fn credential_json(token: &str, refresh: Option<&str>, expires_at: Option<&str>) -> String {
    json!({
        "access_token": token,
        "refresh_token": refresh,
        "expires_at": expires_at,
        "scope": "reactions:read"
    })
    .to_string()
}

struct SlackHarness {
    store: Arc<SqliteWorkStore>,
    sync: SyncCoordinator,
    changes: SourceChangeProcessor,
    commands: WorkCommandService,
    reads: WorkReadService,
    navigation: NavigationService,
}

impl SlackHarness {
    fn new(client: Arc<SlackApiClient>) -> Self {
        let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
        let secrets = Arc::new(InMemorySecretStore::new());
        secrets
            .set(
                &credential_key("slack-connection"),
                &credential_json("xoxp-test-secret", None, None),
            )
            .unwrap();
        let clock = Arc::new(FixedClock::new(now()));
        let tokens = Arc::new(SlackTokenProvider::new(
            "client-id",
            Arc::clone(&client),
            secrets.clone(),
            clock.clone(),
        ));
        let mut registry = ExtensionRegistry::new();
        registry
            .register(slack::registration(client, tokens))
            .unwrap();
        let registry = Arc::new(registry);
        store
            .put_connection(&Connection {
                id: "slack-connection".into(),
                provider_id: ProviderId(PROVIDER_ID.into()),
                display_name: "Workspace — tester".into(),
                config: json!({ "team_id": "T1", "user_id": "U1", "status": "connected" }),
            })
            .unwrap();
        store
            .put_source_config(&SourceConfig {
                id: "slack-source".into(),
                connection_id: "slack-connection".into(),
                source_type_id: SourceTypeId(SOURCE_TYPE.into()),
                display_name: "Slack :todo:".into(),
                enabled: true,
                expected_sync_interval_seconds: 120,
                settings: json!({
                    "team_id": "T1",
                    "team_name": "Workspace",
                    "user_id": "U1",
                    "reaction_name": "todo"
                }),
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
        let count = self.sync.sync("slack-source").await.unwrap();
        self.changes.process_pending(100).unwrap();
        count
    }
}

#[tokio::test]
async fn reactions_filter_messages_dedupe_and_keep_stable_identity() {
    let captured = message_item(
        "C1",
        "123.456",
        "  fix   this &amp; ship  ",
        "todo",
        &["U1"],
    );
    let other_reaction = message_item("C2", "124.000", "ignore", "eyes", &["U1"]);
    let other_user = message_item("C3", "125.000", "ignore", "todo", &["U2"]);
    let server = MockSlack::start(vec![
        page(
            json!([
                captured.clone(),
                captured,
                other_reaction,
                other_user,
                { "type": "file", "file": { "id": "F1" } }
            ]),
            "",
        ),
        permalink("C1", "123.456"),
    ])
    .await;
    let harness = SlackHarness::new(server.client());
    assert_eq!(harness.sync_and_project().await, 1);
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].entry.title, "fix this & ship");
    assert_eq!(work[0].binding.source_activation_seq, 1);
    assert!(work[0].navigation["web_url"]
        .as_str()
        .unwrap()
        .contains("workspace.slack.com"));
}

#[tokio::test]
async fn reactions_collect_every_page_and_page_two_failure_returns_no_batch() {
    let success = MockSlack::start(vec![
        page(
            json!([message_item("C1", "1.0", "one", "todo", &["U1"])]),
            "next",
        ),
        page(
            json!([message_item("C2", "2.0", "two", "todo", &["U1"])]),
            "",
        ),
        permalink("C1", "1.0"),
        permalink("C2", "2.0"),
    ])
    .await;
    let harness = SlackHarness::new(success.client());
    assert_eq!(harness.sync_and_project().await, 2);
    let requests = success.requests().await;
    assert!(requests[1].contains("cursor=next"));

    let failure = MockSlack::start(vec![
        page(
            json!([message_item("C1", "1.0", "existing", "todo", &["U1"])]),
            "",
        ),
        permalink("C1", "1.0"),
        page(json!([]), "next"),
        MockResponse::status(503, json!({ "ok": false })),
    ])
    .await;
    let failed = SlackHarness::new(failure.client());
    failed.sync_and_project().await;
    assert!(failed.sync.sync("slack-source").await.is_err());
    let existing = failed.store.stored_work().unwrap();
    assert_eq!(existing.len(), 1);
    assert_eq!(existing[0].entry.lifecycle, WorkLifecycle::Active);
    let runtime = failed.store.source_runtime("slack-source").unwrap();
    assert_eq!(runtime.failure_count, 1);
    assert_eq!(runtime.last_success_at, Some(now()));
    assert_eq!(
        runtime.next_sync_at,
        Some(now() + chrono::Duration::seconds(120))
    );
}

#[tokio::test]
async fn rate_limit_retry_after_controls_the_next_scheduled_sync() {
    let server = MockSlack::start(vec![MockResponse {
        status: 429,
        headers: vec![("Retry-After", "42")],
        body: json!({ "ok": false, "error": "ratelimited" }).to_string(),
    }])
    .await;
    let harness = SlackHarness::new(server.client());
    let error = harness.sync.sync("slack-source").await.unwrap_err();
    assert_eq!(error.retry_after_seconds(), Some(42));
    let runtime = harness.store.source_runtime("slack-source").unwrap();
    assert_eq!(
        runtime.next_sync_at,
        Some(now() + chrono::Duration::seconds(42))
    );
    assert!(runtime
        .last_error
        .unwrap()
        .contains("retry after 42 seconds"));
}

#[tokio::test]
async fn capture_snapshot_is_idempotent_and_recapture_creates_a_new_sqlite_binding() {
    let item = message_item("C1", "123.456", "captured", "todo", &["U1"]);
    let server = MockSlack::start(vec![
        page(json!([item.clone()]), ""),
        permalink("C1", "123.456"),
        page(json!([item.clone()]), ""),
        permalink("C1", "123.456"),
        page(json!([]), ""),
        page(json!([item]), ""),
        permalink("C1", "123.456"),
    ])
    .await;
    let harness = SlackHarness::new(server.client());

    assert_eq!(harness.sync_and_project().await, 1);
    let first = harness.store.stored_work().unwrap()[0].entry.id.clone();
    let dashboard = harness.reads.dashboard().unwrap();
    assert_eq!(dashboard.inbox.len(), 1);
    assert!(dashboard.inbox[0]
        .available_actions
        .contains(&WorkAction::OpenSource));
    assert!(harness
        .navigation
        .open_source_target(&first)
        .unwrap()
        .starts_with("https://workspace.slack.com/"));

    assert_eq!(harness.sync_and_project().await, 0);
    assert_eq!(harness.store.stored_work().unwrap().len(), 1);
    harness.commands.complete(&first).unwrap();
    assert_eq!(harness.sync_and_project().await, 1);
    let completed = harness.store.stored_work_by_id(&first).unwrap();
    assert_eq!(completed.entry.lifecycle, WorkLifecycle::Resolved);
    assert_eq!(completed.entry.progress, Some(WorkProgress::Done));

    assert_eq!(harness.sync_and_project().await, 1);
    let history = harness.store.stored_work().unwrap();
    assert_eq!(history.len(), 2);
    let active = history
        .iter()
        .find(|stored| stored.entry.id != first)
        .unwrap();
    assert_eq!(active.entry.lifecycle, WorkLifecycle::Active);
    assert_eq!(active.entry.progress, Some(WorkProgress::Todo));
    assert_eq!(active.binding.source_activation_seq, 2);
    assert_eq!(
        completed.binding.source_entity_id,
        active.binding.source_entity_id
    );
}

#[tokio::test]
async fn oauth_persists_bundle_only_in_secret_store_and_callback_is_one_time() {
    let server = MockSlack::start(vec![
        MockResponse::ok(json!({
            "ok": true,
            "access_token": "xoxp-never-in-sqlite",
            "token_type": "user",
            "scope": "reactions:read"
        })),
        MockResponse::ok(json!({
            "ok": true,
            "team": "Workspace",
            "team_id": "T1",
            "user": "tester",
            "user_id": "U1"
        })),
    ])
    .await;
    let clock = Arc::new(FixedClock::new(now()));
    let client = server.client();
    let oauth = SlackOAuthService::new(client.clone(), clock.clone(), "https://slack.test/auth");
    let start = oauth
        .begin("client-id", "http://localhost/callback")
        .unwrap();
    let authorization = oauth.finish(&start.state, "one-time-code").await.unwrap();
    assert_eq!(authorization.identity.team_id, "T1");
    assert!(oauth.finish(&start.state, "replay").await.is_err());

    let secrets = Arc::new(InMemorySecretStore::new());
    let tokens = SlackTokenProvider::new("client-id", client, secrets.clone(), clock);
    tokens
        .save("connection", &authorization.credential)
        .unwrap();
    assert!(secrets
        .get(&credential_key("connection"))
        .unwrap()
        .unwrap()
        .contains("xoxp-never-in-sqlite"));

    let directory = TempDir::new().unwrap();
    let path = directory.path().join("glancelet.db");
    let store = SqliteWorkStore::open(&path).unwrap();
    store
        .put_connection(&Connection {
            id: "connection".into(),
            provider_id: ProviderId(PROVIDER_ID.into()),
            display_name: "Workspace — tester".into(),
            config: json!({ "team_id": "T1", "user_id": "U1" }),
        })
        .unwrap();
    drop(store);
    let bytes = fs::read(path).unwrap();
    assert!(!bytes
        .windows(b"xoxp-never-in-sqlite".len())
        .any(|window| window == b"xoxp-never-in-sqlite"));
}

#[tokio::test]
async fn expiring_rotated_tokens_refresh_once_and_revocation_requires_auth() {
    let refresh_server = MockSlack::start(vec![MockResponse::ok(json!({
        "ok": true,
        "access_token": "xoxp-new",
        "token_type": "user",
        "expires_in": 43200,
        "refresh_token": "xoxe-new-refresh",
        "scope": "reactions:read"
    }))])
    .await;
    let clock = Arc::new(FixedClock::new(now()));
    let secrets = Arc::new(InMemorySecretStore::new());
    secrets
        .set(
            &credential_key("connection"),
            &credential_json(
                "xoxp-old",
                Some("xoxe-old-refresh"),
                Some("2026-08-11T10:01:00Z"),
            ),
        )
        .unwrap();
    let tokens = Arc::new(SlackTokenProvider::new(
        "client-id",
        refresh_server.client(),
        secrets.clone(),
        clock,
    ));
    let (first, second) = tokio::join!(
        tokens.access_token("connection"),
        tokens.access_token("connection")
    );
    assert_eq!(first.unwrap(), "xoxp-new");
    assert_eq!(second.unwrap(), "xoxp-new");
    assert_eq!(refresh_server.requests().await.len(), 1);
    let replacement = secrets.get(&credential_key("connection")).unwrap().unwrap();
    assert!(replacement.contains("xoxe-new-refresh"));
    assert!(!replacement.contains("xoxe-old-refresh"));

    let revoked_server = MockSlack::start(vec![MockResponse::ok(json!({
        "ok": false,
        "error": "token_revoked"
    }))])
    .await;
    let error = revoked_server
        .client()
        .auth_test("redacted")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("authorized again"));
    assert!(!error.to_string().contains("redacted"));
}

#[test]
fn source_identity_includes_workspace_channel_message_and_reaction() {
    assert_eq!(
        slack::external_id("T1", "C1", "123.456", "todo"),
        "T1/C1/123.456/todo"
    );
    assert_ne!(
        slack::external_id("T1", "C1", "123.456", "todo"),
        slack::external_id("T1", "C1", "123.456", "eyes")
    );
}
