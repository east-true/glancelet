use std::{
    collections::VecDeque,
    fs,
    sync::{Arc, Mutex},
};

use chrono::{TimeZone, Utc};
use glancelet_core::{
    application::{
        Clock, FixedClock, InMemorySecretStore, NavigationService, SecretStore,
        SourceChangeProcessor, SyncCoordinator, TimeContext, WorkReadService, WorkStore,
    },
    domain::{ProviderId, SourceTypeId, WorkKind, WorkLifecycle},
    extension::{Connection, ExtensionRegistry, SourceConfig},
    sources::github::{
        self, is_failure_conclusion, GithubApiClient, GithubDeviceFlowService,
        GithubDevicePollResult, GithubTokenProvider, GithubWorkflowSettings,
        ASSIGNED_ISSUES_SOURCE_TYPE, DEFAULT_SYNC_INTERVAL_SECONDS, PROVIDER_ID,
        REVIEW_REQUESTS_SOURCE_TYPE, WORKFLOW_FAILURES_SOURCE_TYPE,
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
    headers: Vec<(String, String)>,
    body: String,
}

impl MockResponse {
    fn ok(body: Value) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    fn status(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

struct MockGithub {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockGithub {
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
                    response.status,
                    response.body.len(),
                    headers,
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

    fn client(&self, clock: Arc<dyn Clock>) -> Arc<GithubApiClient> {
        Arc::new(GithubApiClient::new(
            Client::new(),
            &self.base_url,
            &self.base_url,
            clock,
        ))
    }

    async fn requests(&self) -> Vec<String> {
        tokio::task::yield_now().await;
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockGithub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Harness {
    store: Arc<SqliteWorkStore>,
    sync: SyncCoordinator,
    changes: SourceChangeProcessor,
    reads: WorkReadService,
}

impl Harness {
    fn new(mock: &MockGithub) -> Self {
        let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
        let clock = Arc::new(FixedClock::new(now()));
        let clock_port: Arc<dyn Clock> = clock;
        let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
        secrets
            .set(
                &github::credential_key("github-account"),
                &json!({
                    "access_token": "github-test-access",
                    "expires_at": null,
                    "refresh_token": null,
                    "refresh_token_expires_at": null
                })
                .to_string(),
            )
            .unwrap();
        let client = mock.client(Arc::clone(&clock_port));
        let tokens = Arc::new(GithubTokenProvider::new(
            "client-id",
            Arc::clone(&client),
            secrets,
            Arc::clone(&clock_port),
        ));
        let mut registry = ExtensionRegistry::new();
        registry
            .register(github::registration(client, tokens))
            .unwrap();
        let registry = Arc::new(registry);
        let store_port: Arc<dyn WorkStore> = store.clone();
        store
            .put_connection(&Connection {
                id: "github-account".into(),
                provider_id: ProviderId(PROVIDER_ID.into()),
                display_name: "octocat".into(),
                config: json!({"user_id":"1", "login":"octocat", "status":"connected"}),
            })
            .unwrap();
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
            reads: WorkReadService::new(
                store_port,
                registry,
                clock_port,
                TimeContext::named("UTC").unwrap(),
            ),
            store,
        }
    }

    fn add_global(&self, id: &str, source_type: &str) {
        self.store
            .put_source_config(&SourceConfig {
                id: id.into(),
                connection_id: "github-account".into(),
                source_type_id: SourceTypeId(source_type.into()),
                display_name: source_type.into(),
                enabled: true,
                removed_at: None,
                expected_sync_interval_seconds: DEFAULT_SYNC_INTERVAL_SECONDS,
                settings: json!({}),
            })
            .unwrap();
    }

    fn add_workflows(&self, id: &str, repository_id: u64, repository: &str) {
        self.store
            .put_source_config(&SourceConfig {
                id: id.into(),
                connection_id: "github-account".into(),
                source_type_id: SourceTypeId(WORKFLOW_FAILURES_SOURCE_TYPE.into()),
                display_name: repository.into(),
                enabled: true,
                removed_at: None,
                expected_sync_interval_seconds: DEFAULT_SYNC_INTERVAL_SECONDS,
                settings: serde_json::to_value(GithubWorkflowSettings {
                    repository_id,
                    repository_node_id: format!("R_{repository_id}"),
                    repository: repository.into(),
                    default_branch: "main".into(),
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
    Utc.with_ymd_and_hms(2026, 8, 13, 0, 0, 0).single().unwrap()
}

fn issue(node_id: &str, title: &str, number: u64, pull_request: bool) -> Value {
    let mut value = json!({
        "node_id": node_id,
        "title": title,
        "html_url": format!("https://github.com/acme/backend/issues/{number}"),
        "number": number,
        "updated_at": "2026-08-13T00:00:00Z",
        "repository_url": "https://api.github.com/repos/acme/backend",
        "body": "PRIVATE_BODY_MUST_NOT_PERSIST"
    });
    if pull_request {
        value["pull_request"] = json!({"url":"https://api.github.com/pulls/1"});
        value["html_url"] = json!(format!("https://github.com/acme/backend/pull/{number}"));
    }
    value
}

fn search(items: Vec<Value>, total: usize, incomplete: bool) -> MockResponse {
    MockResponse::ok(json!({
        "total_count": total,
        "incomplete_results": incomplete,
        "items": items
    }))
}

fn workflows(values: Vec<Value>) -> MockResponse {
    MockResponse::ok(json!({"total_count": values.len(), "workflows": values}))
}

fn runs(values: Vec<Value>) -> MockResponse {
    MockResponse::ok(json!({"total_count": values.len(), "workflow_runs": values}))
}

fn workflow(id: u64, name: &str) -> Value {
    json!({"id":id, "name":name, "state":"active"})
}

fn run(id: u64, conclusion: &str) -> Value {
    json!({
        "id": id,
        "conclusion": conclusion,
        "html_url": format!("https://github.com/acme/backend/actions/runs/{id}"),
        "updated_at": "2026-08-13T00:00:00Z",
        "logs_url": "https://api.github.com/private-logs"
    })
}

#[tokio::test]
async fn device_flow_honors_pending_and_slow_down_then_keeps_token_in_secret_store() {
    let mock = MockGithub::start(vec![
        MockResponse::ok(json!({
            "device_code":"device-secret", "user_code":"ABCD-EFGH",
            "verification_uri":"https://github.com/login/device",
            "expires_in":900, "interval":2
        })),
        MockResponse::ok(json!({"error":"authorization_pending"})),
        MockResponse::ok(json!({"error":"slow_down"})),
        MockResponse::ok(json!({
            "access_token":"ghu_test_secret", "token_type":"bearer",
            "expires_in":28800, "refresh_token":"ghr_test_secret",
            "refresh_token_expires_in":15897600
        })),
        MockResponse::ok(json!({"id":42,"login":"octocat"})),
    ])
    .await;
    let clock = Arc::new(FixedClock::new(now()));
    let clock_port: Arc<dyn Clock> = clock.clone();
    let client = mock.client(Arc::clone(&clock_port));
    let flow = GithubDeviceFlowService::new(Arc::clone(&client), clock_port.clone());
    let start = flow.begin("Iv1.test-client").await.unwrap();
    assert_eq!(start.user_code, "ABCD-EFGH");
    assert!(matches!(
        flow.poll(&start.session_id).await.unwrap(),
        GithubDevicePollResult::Pending {
            retry_after_seconds: 2
        }
    ));
    assert!(matches!(
        flow.poll(&start.session_id).await.unwrap(),
        GithubDevicePollResult::Pending {
            retry_after_seconds: 2
        }
    ));
    assert_eq!(mock.requests().await.len(), 2);
    clock.set(now() + chrono::Duration::seconds(2));
    assert!(matches!(
        flow.poll(&start.session_id).await.unwrap(),
        GithubDevicePollResult::Pending {
            retry_after_seconds: 7
        }
    ));
    clock.set(now() + chrono::Duration::seconds(9));
    let authorization = match flow.poll(&start.session_id).await.unwrap() {
        GithubDevicePollResult::Authorized(value) => value,
        GithubDevicePollResult::Pending { .. } => panic!("authorization should be complete"),
    };
    assert_eq!(authorization.identity.id, "42");
    assert_eq!(authorization.identity.login, "octocat");

    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
    let tokens = GithubTokenProvider::new("Iv1.test-client", client, secrets.clone(), clock_port);
    tokens
        .save("connection", &authorization.credential)
        .unwrap();
    let stored = secrets
        .get(&github::credential_key("connection"))
        .unwrap()
        .unwrap();
    assert!(stored.contains("ghu_test_secret"));
    assert!(stored.contains("ghr_test_secret"));
    assert!(!format!("{start:?}").contains("device-secret"));
    assert!(flow.poll(&start.session_id).await.is_err());

    let directory = TempDir::new().unwrap();
    let path = directory.path().join("github.db");
    let store = SqliteWorkStore::open(&path).unwrap();
    store
        .put_connection(&Connection {
            id: "connection".into(),
            provider_id: ProviderId(PROVIDER_ID.into()),
            display_name: "octocat".into(),
            config: json!({ "user_id": "42", "login": "octocat" }),
        })
        .unwrap();
    drop(store);
    let database = fs::read(path).unwrap();
    for secret in [b"ghu_test_secret".as_slice(), b"ghr_test_secret".as_slice()] {
        assert!(!database
            .windows(secret.len())
            .any(|window| window == secret));
    }
}

#[tokio::test]
async fn device_flow_terminal_responses_end_the_session() {
    let mock = MockGithub::start(vec![
        MockResponse::ok(json!({
            "device_code":"denied-device", "user_code":"DENY-CODE",
            "verification_uri":"https://github.com/login/device",
            "expires_in":900, "interval":1
        })),
        MockResponse::ok(json!({"error":"access_denied"})),
        MockResponse::ok(json!({
            "device_code":"expired-device", "user_code":"OLD-CODE",
            "verification_uri":"https://github.com/login/device",
            "expires_in":900, "interval":1
        })),
        MockResponse::ok(json!({"error":"expired_token"})),
        MockResponse::status(400, json!({"error":"device_flow_disabled"})),
    ])
    .await;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let client = mock.client(Arc::clone(&clock));
    let flow = GithubDeviceFlowService::new(Arc::clone(&client), clock);

    let denied = flow.begin("client").await.unwrap();
    assert!(flow.poll(&denied.session_id).await.is_err());
    assert!(flow.poll(&denied.session_id).await.is_err());

    let expired = flow.begin("client").await.unwrap();
    assert!(flow.poll(&expired.session_id).await.is_err());
    assert!(flow.poll(&expired.session_id).await.is_err());

    let disabled = flow.begin("client").await.unwrap_err();
    assert!(disabled.to_string().contains("Device Flow is disabled"));
}

#[tokio::test]
async fn refresh_replaces_expiring_bundle_and_rejection_requires_reauthentication() {
    let mock = MockGithub::start(vec![
        MockResponse::ok(json!({
            "access_token":"ghu_replaced", "token_type":"bearer",
            "expires_in":28800, "refresh_token":"ghr_replaced",
            "refresh_token_expires_in":15897600
        })),
        MockResponse::ok(json!({"error":"bad_refresh_token"})),
    ])
    .await;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let client = mock.client(Arc::clone(&clock));
    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
    let key = github::credential_key("connection");
    secrets
        .set(
            &key,
            &json!({
                "access_token":"old", "expires_at":now(),
                "refresh_token":"ghr_old", "refresh_token_expires_at":null
            })
            .to_string(),
        )
        .unwrap();
    let tokens = Arc::new(GithubTokenProvider::new(
        "client",
        Arc::clone(&client),
        Arc::clone(&secrets),
        Arc::clone(&clock),
    ));
    let (first, second) = tokio::join!(
        tokens.access_token("connection"),
        tokens.access_token("connection")
    );
    assert_eq!(first.unwrap(), "ghu_replaced");
    assert_eq!(second.unwrap(), "ghu_replaced");
    assert_eq!(mock.requests().await.len(), 1);
    assert!(secrets.get(&key).unwrap().unwrap().contains("ghr_replaced"));
    secrets
        .set(
            &key,
            &json!({
                "access_token":"old", "expires_at":now(),
                "refresh_token":"bad", "refresh_token_expires_at":null
            })
            .to_string(),
        )
        .unwrap();
    assert!(matches!(
        tokens.access_token("connection").await,
        Err(glancelet_core::GlanceletError::AuthenticationRequired(_))
    ));
}

#[tokio::test]
async fn installation_discovery_respects_user_app_repository_intersection() {
    let mock = MockGithub::start(vec![
        MockResponse::ok(json!({"total_count":0,"installations":[]})),
        MockResponse::ok(json!({"total_count":1,"installations":[{"id":7}]})),
        MockResponse::ok(json!({
            "total_count":1,
            "repositories":[{
                "id":99,"node_id":"R_99","full_name":"acme/backend","default_branch":"main",
                "private":true,"description":"PRIVATE_REPOSITORY_DESCRIPTION"
            }]
        })),
    ])
    .await;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let client = mock.client(clock);
    assert!(client.repositories("token").await.unwrap().is_empty());
    let repositories = client.repositories("token").await.unwrap();
    assert_eq!(repositories.len(), 1);
    assert_eq!(repositories[0].full_name, "acme/backend");
    assert_eq!(repositories[0].default_branch, "main");
}

#[tokio::test]
async fn review_requests_are_authoritative_paginated_mirror_actions() {
    let mock = MockGithub::start(vec![
        search(vec![issue("PR_1", "Review backend", 1, true)], 2, false),
        search(vec![issue("PR_2", "Review frontend", 2, true)], 2, false),
        search(Vec::new(), 0, false),
        search(
            vec![issue("PR_1", "Review backend again", 1, true)],
            1,
            false,
        ),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.add_global("reviews", REVIEW_REQUESTS_SOURCE_TYPE);
    assert_eq!(harness.sync_and_project("reviews").await, 2);
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 2);
    assert!(work.iter().all(|item| item.entry.kind == WorkKind::Action));
    assert!(work.iter().all(|item| item.entry.progress.is_none()));
    assert!(work
        .iter()
        .all(|item| item.binding.source_activation_seq == 1));
    let url = NavigationService::new(harness.store.clone())
        .open_source_target(&work[0].entry.id)
        .unwrap();
    assert!(url.starts_with("https://github.com/acme/backend/pull/"));

    assert_eq!(harness.sync_and_project("reviews").await, 2);
    assert!(harness
        .store
        .stored_work()
        .unwrap()
        .iter()
        .all(|item| item.entry.lifecycle == WorkLifecycle::Resolved));
    assert_eq!(harness.sync_and_project("reviews").await, 1);
    let active = harness
        .store
        .stored_work()
        .unwrap()
        .into_iter()
        .filter(|item| item.entry.lifecycle == WorkLifecycle::Active)
        .collect::<Vec<_>>();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].binding.source_activation_seq, 2);
    assert_eq!(active[0].entry.title, "Review backend again");

    let requests = mock.requests().await;
    assert!(requests[0].contains("user-review-requested%3A%40me"));
    assert!(requests[0].contains("x-github-api-version: 2026-03-10"));
    assert!(requests[1].contains("page=2"));
}

#[tokio::test]
async fn incomplete_or_failed_review_search_never_deactivates_existing_work() {
    let mock = MockGithub::start(vec![
        search(vec![issue("PR_1", "Review", 1, true)], 1, false),
        search(Vec::new(), 1, true),
        search(Vec::new(), 1_001, false),
        search(vec![issue("PR_1", "Review", 1, true)], 2, false),
        MockResponse::status(500, json!({"message":"server failed"})),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.add_global("reviews", REVIEW_REQUESTS_SOURCE_TYPE);
    harness.sync_and_project("reviews").await;
    assert!(harness.sync.sync("reviews").await.is_err());
    assert!(harness.sync.sync("reviews").await.is_err());
    assert!(harness.sync.sync("reviews").await.is_err());
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].entry.lifecycle, WorkLifecycle::Active);
}

#[tokio::test]
async fn assigned_issues_collect_every_page_before_snapshot() {
    let mut first_page = (0..99)
        .map(|number| {
            issue(
                &format!("PR_{number}"),
                "Assigned pull request",
                number,
                true,
            )
        })
        .collect::<Vec<_>>();
    first_page.push(issue("ISSUE_1", "First issue", 100, false));
    let mock = MockGithub::start(vec![
        MockResponse::ok(Value::Array(first_page)),
        MockResponse::ok(json!([issue("ISSUE_2", "Second issue", 101, false)])),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.add_global("issues", ASSIGNED_ISSUES_SOURCE_TYPE);
    assert_eq!(harness.sync_and_project("issues").await, 2);
    assert_eq!(harness.store.stored_work().unwrap().len(), 2);
    assert!(mock.requests().await[1].contains("page=2"));
}

#[tokio::test]
async fn assigned_issues_filter_pull_requests_and_reactivate_by_node_identity() {
    let mock = MockGithub::start(vec![
        MockResponse::ok(json!([
            issue("ISSUE_1", "Fix bug", 3, false),
            issue("PR_IGNORED", "Assigned pull request", 4, true)
        ])),
        MockResponse::ok(json!([])),
        MockResponse::ok(json!([issue("ISSUE_1", "Fix renamed bug", 3, false)])),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.add_global("issues", ASSIGNED_ISSUES_SOURCE_TYPE);
    harness.sync_and_project("issues").await;
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].entry.title, "Fix bug");
    assert_eq!(
        NavigationService::new(harness.store.clone())
            .open_source_target(&work[0].entry.id)
            .unwrap(),
        "https://github.com/acme/backend/issues/3"
    );
    assert!(!serde_json::to_string(&work[0].entry)
        .unwrap()
        .contains("PRIVATE_BODY"));
    harness.sync_and_project("issues").await;
    harness.sync_and_project("issues").await;
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].entry.lifecycle, WorkLifecycle::Active);
    assert_eq!(work[0].binding.source_activation_seq, 2);
    assert_eq!(work[0].entry.title, "Fix renamed bug");
    assert!(mock.requests().await[0].contains("filter=assigned"));
}

#[tokio::test]
async fn workflow_failure_tracks_latest_completed_run_per_workflow() {
    let mock = MockGithub::start(vec![
        workflows(vec![workflow(10, "CI")]),
        runs(vec![run(100, "failure")]),
        workflows(vec![workflow(10, "CI")]),
        runs(vec![run(101, "success")]),
        workflows(vec![workflow(10, "CI")]),
        runs(vec![run(102, "timed_out")]),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.add_workflows("workflow-a", 99, "acme/backend");
    harness.sync_and_project("workflow-a").await;
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].entry.kind, WorkKind::Attention);
    assert_eq!(work[0].entry.title, "CI failed");
    let work_id = work[0].entry.id.clone();
    assert_eq!(
        NavigationService::new(harness.store.clone())
            .open_source_target(&work_id)
            .unwrap(),
        "https://github.com/acme/backend/actions/runs/100"
    );

    harness.sync_and_project("workflow-a").await;
    assert_eq!(
        harness
            .store
            .stored_work_by_id(&work_id)
            .unwrap()
            .entry
            .lifecycle,
        WorkLifecycle::Resolved
    );
    harness.sync_and_project("workflow-a").await;
    let work = harness.store.stored_work_by_id(&work_id).unwrap();
    assert_eq!(work.entry.lifecycle, WorkLifecycle::Active);
    assert_eq!(work.binding.source_activation_seq, 2);
    assert_eq!(work.entry.title, "CI timed out");
    assert_eq!(work.entry.dimensions["github.repository"], "acme/backend");
    assert!(!serde_json::to_string(&work.entry)
        .unwrap()
        .contains("private-logs"));
    let requests = mock.requests().await;
    assert!(requests[1].contains("branch=main"));
    assert!(requests[1].contains("status=completed"));
    assert!(requests[1].contains("per_page=1"));
}

#[tokio::test]
async fn workflow_discovery_collects_every_page_before_snapshot() {
    let first_page = (0..100)
        .map(|id| json!({"id":id,"name":format!("Disabled {id}"),"state":"disabled_manually"}))
        .collect::<Vec<_>>();
    let mock = MockGithub::start(vec![
        workflows(first_page),
        workflows(vec![workflow(500, "CI")]),
        runs(vec![run(600, "failure")]),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.add_workflows("workflow-a", 99, "acme/backend");
    assert_eq!(harness.sync_and_project("workflow-a").await, 1);
    let requests = mock.requests().await;
    assert!(requests[1].contains("page=2"));
    assert!(requests[2].contains("workflows/500/runs"));
}

#[tokio::test]
async fn one_workflow_request_failure_preserves_the_entire_snapshot() {
    let mock = MockGithub::start(vec![
        workflows(vec![workflow(10, "CI"), workflow(20, "Deploy")]),
        runs(vec![run(100, "failure")]),
        runs(vec![run(200, "action_required")]),
        workflows(vec![workflow(10, "CI"), workflow(20, "Deploy")]),
        runs(vec![run(101, "success")]),
        MockResponse::status(500, json!({"message":"temporary"})),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.add_workflows("workflow-a", 99, "acme/backend");
    harness.sync_and_project("workflow-a").await;
    assert_eq!(harness.store.stored_work().unwrap().len(), 2);
    assert!(harness.sync.sync("workflow-a").await.is_err());
    assert!(harness
        .store
        .stored_work()
        .unwrap()
        .iter()
        .all(|work| work.entry.lifecycle == WorkLifecycle::Active));
}

#[test]
fn workflow_conclusions_are_conservative_and_future_safe() {
    for conclusion in ["failure", "timed_out", "startup_failure", "action_required"] {
        assert!(is_failure_conclusion(conclusion));
    }
    for conclusion in [
        "success",
        "neutral",
        "skipped",
        "cancelled",
        "stale",
        "future_conclusion",
    ] {
        assert!(!is_failure_conclusion(conclusion));
    }
}

#[tokio::test]
async fn github_source_types_share_one_connection_but_keep_runtime_isolated() {
    let mock = MockGithub::start(vec![
        search(vec![issue("PR_1", "Review", 1, true)], 1, false),
        MockResponse::ok(json!([issue("ISSUE_1", "Issue", 2, false)])),
        workflows(vec![workflow(10, "CI")]),
        runs(vec![run(100, "failure")]),
        MockResponse::status(500, json!({"message":"repo B unavailable"})),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.add_global("reviews", REVIEW_REQUESTS_SOURCE_TYPE);
    harness.add_global("issues", ASSIGNED_ISSUES_SOURCE_TYPE);
    harness.add_workflows("workflow-a", 99, "acme/backend");
    harness.add_workflows("workflow-b", 100, "acme/frontend");

    harness.sync_and_project("reviews").await;
    harness.sync_and_project("issues").await;
    harness.sync_and_project("workflow-a").await;
    assert!(harness.sync.sync("workflow-b").await.is_err());
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 3);
    assert_eq!(
        work.iter()
            .filter(|item| item.entry.kind == WorkKind::Action)
            .count(),
        2
    );
    assert_eq!(
        work.iter()
            .filter(|item| item.entry.kind == WorkKind::Attention)
            .count(),
        1
    );
    assert!(harness
        .store
        .source_runtime("reviews")
        .unwrap()
        .last_success_at
        .is_some());
    assert!(harness
        .store
        .source_runtime("workflow-b")
        .unwrap()
        .last_error
        .is_some());
    let dashboard = harness.reads.dashboard().unwrap();
    assert_eq!(dashboard.inbox.len(), 2);
    assert_eq!(dashboard.today.len(), 1);
}

#[tokio::test]
async fn github_rate_limit_uses_retry_headers_without_resolving_work() {
    let reset = (now() + chrono::Duration::seconds(120))
        .timestamp()
        .to_string();
    let mock = MockGithub::start(vec![
        search(vec![issue("PR_1", "Review", 1, true)], 1, false),
        MockResponse::status(403, json!({"message":"API rate limit exceeded"}))
            .with_header("X-RateLimit-Remaining", "0")
            .with_header("X-RateLimit-Reset", &reset),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.add_global("reviews", REVIEW_REQUESTS_SOURCE_TYPE);
    harness.sync_and_project("reviews").await;
    assert!(harness.sync.sync("reviews").await.is_err());
    let runtime = harness.store.source_runtime("reviews").unwrap();
    assert_eq!(
        runtime.next_sync_at,
        Some(now() + chrono::Duration::seconds(120))
    );
    assert_eq!(
        harness.store.stored_work().unwrap()[0].entry.lifecycle,
        WorkLifecycle::Active
    );
}

#[tokio::test]
async fn github_authentication_failure_suspends_sync_without_resolving_work() {
    let mock = MockGithub::start(vec![
        search(vec![issue("PR_1", "Review", 1, true)], 1, false),
        MockResponse::status(401, json!({"message":"Bad credentials"})),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.add_global("reviews", REVIEW_REQUESTS_SOURCE_TYPE);
    harness.sync_and_project("reviews").await;
    assert!(harness.sync.sync("reviews").await.is_err());
    let runtime = harness.store.source_runtime("reviews").unwrap();
    assert!(runtime.authentication_required());
    assert!(runtime.next_sync_at.is_none());
    assert_eq!(
        harness.store.stored_work().unwrap()[0].entry.lifecycle,
        WorkLifecycle::Active
    );
}

#[test]
fn source_readd_matching_is_scoped_by_type_and_repository() {
    let global = SourceConfig {
        id: "global".into(),
        connection_id: "connection".into(),
        source_type_id: SourceTypeId(REVIEW_REQUESTS_SOURCE_TYPE.into()),
        display_name: "reviews".into(),
        enabled: false,
        removed_at: Some(now()),
        expected_sync_interval_seconds: 300,
        settings: json!({}),
    };
    assert!(github::matches_global_source_config(
        &global,
        "connection",
        REVIEW_REQUESTS_SOURCE_TYPE
    ));
    assert!(!github::matches_global_source_config(
        &global,
        "connection",
        ASSIGNED_ISSUES_SOURCE_TYPE
    ));
    let workflow = SourceConfig {
        id: "workflow".into(),
        connection_id: "connection".into(),
        source_type_id: SourceTypeId(WORKFLOW_FAILURES_SOURCE_TYPE.into()),
        display_name: "repo".into(),
        enabled: false,
        removed_at: Some(now()),
        expected_sync_interval_seconds: 300,
        settings: serde_json::to_value(GithubWorkflowSettings {
            repository_id: 99,
            repository_node_id: "R_99".into(),
            repository: "acme/backend".into(),
            default_branch: "main".into(),
        })
        .unwrap(),
    };
    assert!(github::matches_workflow_source_config(
        &workflow,
        "connection",
        99
    ));
    assert!(!github::matches_workflow_source_config(
        &workflow,
        "connection",
        100
    ));
}
