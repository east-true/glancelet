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
    domain::{ProgressAuthority, ProviderId, SourceTypeId, WorkKind, WorkLifecycle},
    extension::{Connection, ExtensionRegistry, SourceConfig},
    sources::gitlab::{
        self, GitlabApiClient, GitlabCredential, GitlabDeviceFlowService, GitlabDevicePollResult,
        GitlabInstance, GitlabTodoSettings, GitlabTokenProvider, DEFAULT_SYNC_INTERVAL_SECONDS,
        PROVIDER_ID, SOURCE_TYPE,
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

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

struct MockGitlab {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockGitlab {
    async fn start(responses: Vec<MockResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let responses = responses
            .into_iter()
            .map(|mut response| {
                response.body = response.body.replace("TARGET_ORIGIN", &base_url);
                for (_, value) in &mut response.headers {
                    *value = value.replace("TARGET_ORIGIN", &base_url);
                }
                response
            })
            .collect::<VecDeque<_>>();
        let responses = Arc::new(Mutex::new(responses));
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
            base_url,
            requests,
            task,
        }
    }

    fn instance(&self) -> GitlabInstance {
        GitlabInstance::parse(&self.base_url).unwrap()
    }

    fn client(&self, clock: Arc<dyn Clock>) -> Arc<GitlabApiClient> {
        Arc::new(GitlabApiClient::new(Client::new(), clock))
    }

    async fn requests(&self) -> Vec<String> {
        tokio::task::yield_now().await;
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockGitlab {
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
    fn new(mock: &MockGitlab) -> Self {
        let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
        let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
        secrets
            .set(
                &gitlab::credential_key("gitlab-account"),
                &serde_json::to_string(&GitlabCredential::personal_access_token(
                    "gitlab-test-pat".into(),
                ))
                .unwrap(),
            )
            .unwrap();
        let client = mock.client(Arc::clone(&clock));
        let tokens = Arc::new(GitlabTokenProvider::new(
            "client-id",
            Arc::clone(&client),
            secrets,
            Arc::clone(&clock),
        ));
        let mut registry = ExtensionRegistry::new();
        registry
            .register(gitlab::registration(client, tokens))
            .unwrap();
        let registry = Arc::new(registry);
        let store_port: Arc<dyn WorkStore> = store.clone();
        store
            .put_connection(&Connection {
                id: "gitlab-account".into(),
                provider_id: ProviderId(PROVIDER_ID.into()),
                display_name: "gitlab.test · alice".into(),
                config: json!({"instance_origin": mock.instance().origin(), "user_id":"42"}),
            })
            .unwrap();
        store
            .put_source_config(&SourceConfig {
                id: "todos".into(),
                connection_id: "gitlab-account".into(),
                source_type_id: SourceTypeId(SOURCE_TYPE.into()),
                display_name: "GitLab To-Dos".into(),
                enabled: true,
                removed_at: None,
                expected_sync_interval_seconds: DEFAULT_SYNC_INTERVAL_SECONDS,
                settings: serde_json::to_value(GitlabTodoSettings {
                    instance_origin: mock.instance().origin().into(),
                })
                .unwrap(),
            })
            .unwrap();
        Self {
            sync: SyncCoordinator::new(
                Arc::clone(&store_port),
                Arc::clone(&registry),
                Arc::clone(&clock),
            ),
            changes: SourceChangeProcessor::new(
                Arc::clone(&store_port),
                Arc::clone(&registry),
                Arc::clone(&clock),
            ),
            reads: WorkReadService::new(
                store_port,
                registry,
                clock,
                TimeContext::named("UTC").unwrap(),
            ),
            store,
        }
    }

    async fn sync_and_project(&self) -> usize {
        let changed = self.sync.sync("todos").await.unwrap();
        self.changes.process_pending(100).unwrap();
        changed
    }
}

#[test]
fn instance_normalization_requires_https_except_loopback() {
    assert_eq!(
        GitlabInstance::parse("https://gitlab.example.com/")
            .unwrap()
            .origin(),
        "https://gitlab.example.com"
    );
    assert!(GitlabInstance::parse("https://gitlab.example.com/api/v4").is_err());
    assert!(GitlabInstance::parse("http://gitlab.example.com").is_err());
    assert!(GitlabInstance::parse("http://127.0.0.1:8080").is_ok());
    let connection = Connection {
        id: "gitlab-com-user".into(),
        provider_id: ProviderId(PROVIDER_ID.into()),
        display_name: "alice".into(),
        config: json!({"instance_origin":"https://gitlab.com", "user_id":"42"}),
    };
    assert!(gitlab::matches_connection(
        &connection,
        &GitlabInstance::gitlab_com(),
        "42"
    ));
    assert!(!gitlab::matches_connection(
        &connection,
        &GitlabInstance::parse("https://gitlab.example.com").unwrap(),
        "42"
    ));
}

#[tokio::test]
async fn pat_validation_requires_user_identity_and_todos_read_access() {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let valid = MockGitlab::start(vec![
        MockResponse::ok(json!({"id":42,"username":"alice"})),
        MockResponse::ok(json!([])),
    ])
    .await;
    let client = valid.client(Arc::clone(&clock));
    let auth = gitlab::GitlabAuth::PersonalAccessToken("dummy-pat".into());
    assert_eq!(
        client
            .authenticated_user(&valid.instance(), &auth)
            .await
            .unwrap()
            .username,
        "alice"
    );
    assert!(client.todos(&valid.instance(), &auth).await.is_ok());

    let missing_scope = MockGitlab::start(vec![
        MockResponse::ok(json!({"id":42,"username":"alice"})),
        MockResponse::status(403, json!({"message":"Forbidden"})),
    ])
    .await;
    let client = missing_scope.client(clock);
    assert!(client
        .authenticated_user(&missing_scope.instance(), &auth)
        .await
        .is_ok());
    assert!(client
        .todos(&missing_scope.instance(), &auth)
        .await
        .is_err());
}

#[tokio::test]
async fn device_flow_honors_pending_and_slow_down_before_identity_validation() {
    let clock = Arc::new(FixedClock::new(now()));
    let clock_port: Arc<dyn Clock> = clock.clone();
    let mock = MockGitlab::start(vec![
        MockResponse::ok(json!({
            "device_code":"dummy-device-code", "user_code":"ABCD-1234",
            "verification_uri":"https://gitlab.com/oauth/device",
            "expires_in":300, "interval":5
        })),
        MockResponse::status(400, json!({"error":"authorization_pending"})),
        MockResponse::status(400, json!({"error":"slow_down"})),
        MockResponse::ok(json!({
            "access_token":"dummy-access", "refresh_token":"dummy-refresh",
            "expires_in":7200, "created_at":now().timestamp(), "scope":"read_api"
        })),
        MockResponse::ok(json!({"id":42,"username":"alice"})),
    ])
    .await;
    let service = GitlabDeviceFlowService::new(mock.client(Arc::clone(&clock_port)), clock_port);
    let challenge = service.begin(mock.instance(), "client-id").await.unwrap();
    assert!(matches!(
        service.poll(&challenge.session_id).await.unwrap(),
        GitlabDevicePollResult::Pending {
            retry_after_seconds: 5
        }
    ));
    clock.set(now() + chrono::Duration::seconds(5));
    assert!(matches!(
        service.poll(&challenge.session_id).await.unwrap(),
        GitlabDevicePollResult::Pending {
            retry_after_seconds: 10
        }
    ));
    clock.set(now() + chrono::Duration::seconds(15));
    match service.poll(&challenge.session_id).await.unwrap() {
        GitlabDevicePollResult::Authorized(value) => {
            assert_eq!(value.identity.id, "42");
            assert_eq!(value.identity.username, "alice");
        }
        _ => panic!("expected authorized device flow"),
    }
    assert!(mock.requests().await[0].contains("scope=read_api"));
}

#[tokio::test]
async fn device_flow_denial_and_expiry_are_terminal() {
    for error in ["access_denied", "expired_token", "device_flow_disabled"] {
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
        let mock = MockGitlab::start(vec![
            MockResponse::ok(json!({
                "device_code":"dummy-device-code", "user_code":"ABCD-1234",
                "verification_uri":"https://gitlab.com/oauth/device",
                "expires_in":300, "interval":5
            })),
            MockResponse::status(400, json!({"error":error})),
        ])
        .await;
        let service = GitlabDeviceFlowService::new(mock.client(Arc::clone(&clock)), clock);
        let challenge = service.begin(mock.instance(), "client-id").await.unwrap();
        assert!(service.poll(&challenge.session_id).await.is_err());
        assert!(service.poll(&challenge.session_id).await.is_err());
    }
}

#[tokio::test]
async fn oauth_refresh_rotates_the_entire_secret_bundle() {
    let mock = MockGitlab::start(vec![MockResponse::ok(json!({
        "access_token":"replacement-access", "refresh_token":"replacement-refresh",
        "expires_in":7200, "created_at":now().timestamp(), "scope":"read_api"
    }))])
    .await;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
    secrets
        .set(
            &gitlab::credential_key("connection"),
            &serde_json::to_string(&GitlabCredential::oauth(
                "expired-access".into(),
                Some("old-refresh".into()),
                Some(now()),
            ))
            .unwrap(),
        )
        .unwrap();
    let tokens = Arc::new(GitlabTokenProvider::new(
        "client-id",
        mock.client(Arc::clone(&clock)),
        Arc::clone(&secrets),
        clock,
    ));
    let instance = mock.instance();
    let (first, second) = tokio::join!(
        tokens.access("connection", &instance),
        tokens.access("connection", &instance)
    );
    assert!(first.is_ok());
    assert!(second.is_ok());
    assert_eq!(mock.requests().await.len(), 1);
    let stored = secrets
        .get(&gitlab::credential_key("connection"))
        .unwrap()
        .unwrap();
    assert!(stored.contains("replacement-refresh"));
    assert!(!stored.contains("old-refresh"));
}

#[tokio::test]
async fn refresh_rejection_requires_reauthentication_without_replacing_secret() {
    let mock = MockGitlab::start(vec![MockResponse::status(
        400,
        json!({"error":"invalid_grant"}),
    )])
    .await;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
    let original = serde_json::to_string(&GitlabCredential::oauth(
        "expired-access".into(),
        Some("old-refresh".into()),
        Some(now()),
    ))
    .unwrap();
    secrets
        .set(&gitlab::credential_key("connection"), &original)
        .unwrap();
    let tokens = GitlabTokenProvider::new(
        "client-id",
        mock.client(Arc::clone(&clock)),
        Arc::clone(&secrets),
        clock,
    );
    assert!(tokens.access("connection", &mock.instance()).await.is_err());
    assert_eq!(
        secrets
            .get(&gitlab::credential_key("connection"))
            .unwrap()
            .unwrap(),
        original
    );
}

#[tokio::test]
async fn transient_refresh_failure_does_not_become_authentication_required() {
    let mock = MockGitlab::start(vec![MockResponse::status(
        503,
        json!({"message":"temporarily unavailable"}),
    )])
    .await;
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
    secrets
        .set(
            &gitlab::credential_key("connection"),
            &serde_json::to_string(&GitlabCredential::oauth(
                "expired-access".into(),
                Some("old-refresh".into()),
                Some(now()),
            ))
            .unwrap(),
        )
        .unwrap();
    let tokens =
        GitlabTokenProvider::new("client-id", mock.client(Arc::clone(&clock)), secrets, clock);
    let error = match tokens.access("connection", &mock.instance()).await {
        Err(error) => error,
        Ok(_) => panic!("expected transient refresh failure"),
    };
    assert!(!matches!(
        error,
        glancelet_core::GlanceletError::AuthenticationRequired(_)
    ));
}

#[tokio::test]
async fn todos_follow_same_origin_link_and_map_actions_and_target_types() {
    let mock = paginated_mock(MockResponse::ok(json!([
        todo(
            4,
            "approval_required",
            "Vulnerability",
            "Approve fix",
            "security/project"
        ),
        todo(
            5,
            "unmergeable",
            "Project",
            "Resolve conflicts",
            "group/project"
        ),
        todo(
            6,
            "directly_addressed",
            "Alert",
            "Investigate alert",
            "ops/project"
        ),
        todo(
            7,
            "future_action",
            "FutureTarget",
            "Future work",
            "group/project"
        ),
    ])))
    .await;
    let harness = Harness::new(&mock);
    assert_eq!(harness.sync_and_project().await, 7);
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 7);
    assert!(work.iter().all(|item| item.entry.kind == WorkKind::Action));
    assert!(work
        .iter()
        .all(|item| item.binding.progress_authority == ProgressAuthority::None));
    assert_eq!(harness.reads.dashboard().unwrap().inbox.len(), 7);
    let requests = mock.requests().await;
    assert!(requests[0].starts_with("GET /api/v4/todos?state=pending&per_page=100"));
    assert!(requests[0]
        .to_ascii_lowercase()
        .contains("private-token: gitlab-test-pat"));
}

#[tokio::test]
async fn partial_or_cross_origin_pagination_never_returns_a_snapshot() {
    let failed = paginated_mock(MockResponse::status(500, json!({"message":"unavailable"}))).await;
    let harness = Harness::new(&failed);
    assert!(harness.sync.sync("todos").await.is_err());
    assert!(harness.store.stored_work().unwrap().is_empty());

    let cross_origin = MockGitlab::start(vec![MockResponse::ok(json!([todo(
        1,
        "assigned",
        "Issue",
        "Work",
        "group/project"
    )]))
    .header(
        "Link",
        "<https://evil.example/api/v4/todos?page=2>; rel=\"next\"",
    )])
    .await;
    let harness = Harness::new(&cross_origin);
    assert!(harness.sync.sync("todos").await.is_err());
    assert_eq!(cross_origin.requests().await.len(), 1);
}

#[tokio::test]
async fn successful_absence_resolves_and_a_new_todo_id_creates_new_work() {
    let mock = MockGitlab::start(vec![
        MockResponse::ok(json!([todo(
            1,
            "marked",
            "Issue",
            "First",
            "group/project"
        )])),
        MockResponse::ok(json!([])),
        MockResponse::ok(json!([todo(
            2,
            "marked",
            "Issue",
            "First",
            "group/project"
        )])),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.sync_and_project().await;
    harness.sync_and_project().await;
    assert_eq!(
        harness.store.stored_work().unwrap()[0].entry.lifecycle,
        WorkLifecycle::Resolved
    );
    harness.sync_and_project().await;
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 2);
    assert_eq!(
        work.iter()
            .filter(|item| item.entry.lifecycle == WorkLifecycle::Active)
            .count(),
        1
    );
}

#[tokio::test]
async fn rate_limit_and_auth_failure_preserve_active_work() {
    let reset = (now() + chrono::Duration::seconds(90))
        .timestamp()
        .to_string();
    let mock = MockGitlab::start(vec![
        MockResponse::ok(json!([todo(
            1,
            "assigned",
            "Issue",
            "Work",
            "group/project"
        )])),
        MockResponse::status(429, json!({"message":"retry"})).header("RateLimit-Reset", &reset),
        MockResponse::status(401, json!({"message":"Unauthorized"})),
    ])
    .await;
    let harness = Harness::new(&mock);
    harness.sync_and_project().await;
    assert!(harness.sync.sync("todos").await.is_err());
    assert_eq!(
        harness.store.source_runtime("todos").unwrap().next_sync_at,
        Some(now() + chrono::Duration::seconds(90))
    );
    assert!(harness.sync.sync("todos").await.is_err());
    assert!(harness
        .store
        .source_runtime("todos")
        .unwrap()
        .authentication_required());
    assert_eq!(
        harness.store.stored_work().unwrap()[0].entry.lifecycle,
        WorkLifecycle::Active
    );
}

#[tokio::test]
async fn navigation_and_sqlite_exclude_private_body_and_credentials() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("gitlab.db");
    let store = Arc::new(SqliteWorkStore::open(&path).unwrap());
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::default());
    secrets
        .set(
            &gitlab::credential_key("gitlab-account"),
            &serde_json::to_string(&GitlabCredential::personal_access_token(
                "dummy-private-pat".into(),
            ))
            .unwrap(),
        )
        .unwrap();
    let mock = MockGitlab::start(vec![MockResponse::ok(json!([todo_with_body(
        9,
        "Do not persist this private body"
    )]))])
    .await;
    let client = mock.client(Arc::clone(&clock));
    let tokens = Arc::new(GitlabTokenProvider::new(
        "client-id",
        Arc::clone(&client),
        secrets,
        Arc::clone(&clock),
    ));
    let mut registry = ExtensionRegistry::new();
    registry
        .register(gitlab::registration(client, tokens))
        .unwrap();
    let registry = Arc::new(registry);
    let port: Arc<dyn WorkStore> = store.clone();
    store
        .put_connection(&Connection {
            id: "gitlab-account".into(),
            provider_id: ProviderId(PROVIDER_ID.into()),
            display_name: "GitLab".into(),
            config: json!({}),
        })
        .unwrap();
    store
        .put_source_config(&SourceConfig {
            id: "todos".into(),
            connection_id: "gitlab-account".into(),
            source_type_id: SourceTypeId(SOURCE_TYPE.into()),
            display_name: "GitLab To-Dos".into(),
            enabled: true,
            removed_at: None,
            expected_sync_interval_seconds: 300,
            settings: serde_json::to_value(GitlabTodoSettings {
                instance_origin: mock.instance().origin().into(),
            })
            .unwrap(),
        })
        .unwrap();
    SyncCoordinator::new(Arc::clone(&port), Arc::clone(&registry), Arc::clone(&clock))
        .sync("todos")
        .await
        .unwrap();
    SourceChangeProcessor::new(Arc::clone(&port), registry, clock)
        .process_pending(10)
        .unwrap();
    let stored = store.stored_work().unwrap();
    assert!(NavigationService::new(port)
        .open_source_target(&stored[0].entry.id)
        .unwrap()
        .starts_with(mock.instance().origin()));
    drop(stored);
    drop(store);
    let bytes = fs::read(path).unwrap();
    for secret in [
        b"dummy-private-pat".as_slice(),
        b"Do not persist this private body".as_slice(),
    ] {
        assert!(!bytes.windows(secret.len()).any(|window| window == secret));
    }
}

#[test]
fn source_readd_identity_is_connection_scoped() {
    let config = SourceConfig {
        id: "todos".into(),
        connection_id: "instance-user".into(),
        source_type_id: SourceTypeId(SOURCE_TYPE.into()),
        display_name: "GitLab To-Dos".into(),
        enabled: false,
        removed_at: Some(now()),
        expected_sync_interval_seconds: 300,
        settings: json!({}),
    };
    assert!(gitlab::matches_source_config(&config, "instance-user"));
    assert!(!gitlab::matches_source_config(
        &config,
        "same-numeric-id-other-instance"
    ));
}

async fn paginated_mock(second: MockResponse) -> MockGitlab {
    MockGitlab::start(vec![
        MockResponse::ok(json!([
            todo(1, "assigned", "Issue", "Fix auth", "group/project"),
            todo(
                2,
                "mentioned",
                "MergeRequest",
                "Review release",
                "group/project"
            ),
            todo(3, "build_failed", "Commit", "Broken build", "group/project"),
        ]))
        .header(
            "Link",
            "<TARGET_ORIGIN/api/v4/todos?state=pending&per_page=100&page=2>; rel=\"next\"",
        ),
        second,
    ])
    .await
}

fn todo(id: u64, action: &str, target_type: &str, title: &str, project: &str) -> Value {
    json!({
        "id":id, "project":{"path_with_namespace":project}, "action_name":action,
        "target_type":target_type,
        "target":{"title":title,"description":"private target description"},
        "target_url":format!("TARGET_ORIGIN/group/project/-/issues/{id}"),
        "body":"private todo body", "state":"pending",
        "created_at":"2026-08-13T00:00:00Z", "updated_at":"2026-08-13T01:00:00Z"
    })
}

fn todo_with_body(id: u64, body: &str) -> Value {
    let mut value = todo(id, "assigned", "Issue", "Persisted title", "group/project");
    value["body"] = json!(body);
    value
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 13, 0, 0, 0).unwrap()
}
