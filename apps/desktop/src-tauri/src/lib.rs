use std::{
    env, fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use chrono::{DateTime, NaiveDate, Utc};
use glancelet_core::{
    application::{
        Clock, NavigationService, SecretStore, SourceChangeProcessor, SyncCoordinator, SystemClock,
        TimeContext, WorkCommandService, WorkDashboard, WorkReadService, WorkStore,
    },
    domain::{ProviderId, SourceTypeId},
    extension::{Connection, ExtensionRegistry, SourceConfig},
    sources::fake::{self, CAPTURE_SOURCE_TYPE, MIRROR_SOURCE_TYPE},
    sources::slack::{
        self, SlackApiClient, SlackOAuthService, SlackTokenProvider, DEFAULT_REACTION,
        PROVIDER_ID as SLACK_PROVIDER_ID, SOURCE_TYPE as SLACK_SOURCE_TYPE,
    },
    storage::{KeyringSecretStore, SqliteWorkStore},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Manager, State};
use tauri_plugin_opener::OpenerExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;
use uuid::Uuid;

const SLACK_REDIRECT_URI: &str = "http://localhost:42813/oauth/slack/callback";
const SLACK_CALLBACK_ADDRESS: &str = "127.0.0.1:42813";

struct AppServices {
    store: Arc<SqliteWorkStore>,
    sync: Arc<SyncCoordinator>,
    changes: SourceChangeProcessor,
    reads: WorkReadService,
    commands: WorkCommandService,
    navigation: NavigationService,
    clock: Arc<dyn Clock>,
    secrets: Arc<dyn SecretStore>,
    slack_tokens: Arc<SlackTokenProvider>,
    slack_oauth: SlackOAuthService,
    slack_client_id: String,
    stopping: Arc<AtomicBool>,
}

impl AppServices {
    fn initialize(app: &tauri::App) -> Result<Arc<Self>, String> {
        let app_data = app
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        fs::create_dir_all(&app_data).map_err(|error| error.to_string())?;
        let store = Arc::new(
            SqliteWorkStore::open(app_data.join("glancelet.db"))
                .map_err(|error| error.to_string())?,
        );
        let mut registry = ExtensionRegistry::new();
        registry
            .register(fake::registration())
            .map_err(|error| error.to_string())?;
        let store_port: Arc<dyn WorkStore> = store.clone();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore::new("dev.glancelet.app"));
        let slack_client_id = env::var("GLANCELET_SLACK_CLIENT_ID").unwrap_or_default();
        let slack_client =
            Arc::new(SlackApiClient::production().map_err(|error| error.to_string())?);
        let slack_tokens = Arc::new(SlackTokenProvider::new(
            slack_client_id.clone(),
            Arc::clone(&slack_client),
            Arc::clone(&secrets),
            Arc::clone(&clock),
        ));
        registry
            .register(slack::registration(
                Arc::clone(&slack_client),
                Arc::clone(&slack_tokens),
            ))
            .map_err(|error| error.to_string())?;
        let registry = Arc::new(registry);
        seed_fake_sources(&store)?;
        let time_context = TimeContext::system().map_err(|error| error.to_string())?;
        let sync = Arc::new(SyncCoordinator::new(
            Arc::clone(&store_port),
            Arc::clone(&registry),
            Arc::clone(&clock),
        ));
        Ok(Arc::new(Self {
            store,
            sync,
            changes: SourceChangeProcessor::new(
                Arc::clone(&store_port),
                Arc::clone(&registry),
                Arc::clone(&clock),
            ),
            reads: WorkReadService::new(
                Arc::clone(&store_port),
                Arc::clone(&registry),
                Arc::clone(&clock),
                time_context,
            ),
            commands: WorkCommandService::new(Arc::clone(&store_port), Arc::clone(&clock)),
            navigation: NavigationService::new(store_port),
            clock,
            secrets,
            slack_tokens,
            slack_oauth: SlackOAuthService::production(slack_client, Arc::new(SystemClock)),
            slack_client_id,
            stopping: Arc::new(AtomicBool::new(false)),
        }))
    }

    async fn sync_all(&self) -> Result<(), String> {
        self.sync_selected(false).await
    }

    async fn sync_due(&self) -> Result<(), String> {
        self.sync_selected(true).await
    }

    async fn sync_selected(&self, only_due: bool) -> Result<(), String> {
        let configs = self
            .store
            .source_configs()
            .map_err(|error| error.to_string())?;
        let mut failures = Vec::new();
        for config in configs.into_iter().filter(|config| config.enabled) {
            if only_due {
                let runtime = self
                    .store
                    .source_runtime(&config.id)
                    .map_err(|error| error.to_string())?;
                if runtime
                    .next_sync_at
                    .is_some_and(|next_sync| next_sync > self.clock.now())
                {
                    continue;
                }
            }
            if let Err(error) = self.sync.sync(&config.id).await {
                failures.push(format!("{}: {error}", config.display_name));
            }
        }
        self.changes
            .process_pending(500)
            .map_err(|error| error.to_string())?;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    async fn sync_source(&self, source_id: &str) -> Result<(), String> {
        let config = self
            .store
            .source_config(source_id)
            .map_err(|error| error.to_string())?;
        if !config.enabled {
            return Err("source is disabled".into());
        }
        self.sync
            .sync(source_id)
            .await
            .map_err(|error| error.to_string())?;
        self.changes
            .process_pending(500)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SlackConnectionView {
    connection_id: String,
    source_id: Option<String>,
    workspace: String,
    user: String,
    reaction_name: String,
    enabled: bool,
    status: String,
    last_sync: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WorkCommand {
    Plan { date: NaiveDate },
    MoveToInbox,
    MoveToBacklog,
    Snooze { until: DateTime<Utc> },
    Dismiss,
    Pin,
    Unpin,
    StartWork,
    Complete,
}

#[tauri::command]
fn dashboard(services: State<'_, Arc<AppServices>>) -> Result<WorkDashboard, String> {
    services
        .reads
        .dashboard()
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn sync_all(services: State<'_, Arc<AppServices>>) -> Result<(), String> {
    services.sync_all().await
}

#[tauri::command]
fn slack_connections(
    services: State<'_, Arc<AppServices>>,
) -> Result<Vec<SlackConnectionView>, String> {
    let configs = services
        .store
        .source_configs()
        .map_err(|error| error.to_string())?;
    services
        .store
        .connections()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|connection| connection.provider_id.0 == SLACK_PROVIDER_ID)
        .map(|connection| {
            let source = configs.iter().find(|config| {
                config.connection_id == connection.id
                    && config.source_type_id.0 == SLACK_SOURCE_TYPE
            });
            let runtime = source
                .map(|config| services.store.source_runtime(&config.id))
                .transpose()
                .map_err(|error| error.to_string())?;
            let last_error = runtime.as_ref().and_then(|value| value.last_error.clone());
            let disconnected = connection.config["status"] == "disconnected";
            let status = if disconnected {
                "disconnected"
            } else if last_error
                .as_deref()
                .is_some_and(|error| error.starts_with("authentication is required"))
            {
                "reauth_required"
            } else {
                "connected"
            };
            Ok(SlackConnectionView {
                connection_id: connection.id,
                source_id: source.map(|config| config.id.clone()),
                workspace: connection.config["team_name"]
                    .as_str()
                    .unwrap_or("Slack workspace")
                    .to_owned(),
                user: connection.config["user_name"]
                    .as_str()
                    .unwrap_or("Slack user")
                    .to_owned(),
                reaction_name: source
                    .and_then(|config| config.settings["reaction_name"].as_str())
                    .unwrap_or(DEFAULT_REACTION)
                    .to_owned(),
                enabled: source.is_some_and(|config| config.enabled),
                status: status.into(),
                last_sync: runtime.as_ref().and_then(|value| value.last_success_at),
                last_error,
            })
        })
        .collect()
}

#[tauri::command]
async fn connect_slack(
    app: tauri::AppHandle,
    services: State<'_, Arc<AppServices>>,
) -> Result<(), String> {
    let listener = TcpListener::bind(SLACK_CALLBACK_ADDRESS)
        .await
        .map_err(|_| {
            "Slack OAuth callback port 42813 is unavailable; close the other process and retry"
                .to_owned()
        })?;
    let start = services
        .slack_oauth
        .begin(&services.slack_client_id, SLACK_REDIRECT_URI)
        .map_err(|error| error.to_string())?;
    if let Err(error) = app
        .opener()
        .open_url(&start.authorization_url, None::<&str>)
    {
        services.slack_oauth.cancel(&start.state);
        return Err(error.to_string());
    }
    let callback = tokio::time::timeout(Duration::from_secs(300), read_oauth_callback(listener))
        .await
        .map_err(|_| "Slack OAuth callback timed out".to_owned())
        .and_then(|result| result);
    let (state, code) = match callback {
        Ok(callback) => callback,
        Err(error) => {
            services.slack_oauth.cancel(&start.state);
            return Err(error);
        }
    };
    let authorization = match services.slack_oauth.finish(&state, &code).await {
        Ok(authorization) => authorization,
        Err(error) => {
            services.slack_oauth.cancel(&start.state);
            return Err(error.to_string());
        }
    };
    persist_slack_connection(&services, authorization).map_err(|error| error.to_string())
}

#[tauri::command]
async fn sync_source(
    services: State<'_, Arc<AppServices>>,
    source_id: String,
) -> Result<(), String> {
    let config = services
        .store
        .source_config(&source_id)
        .map_err(|error| error.to_string())?;
    if config.source_type_id.0 != SLACK_SOURCE_TYPE {
        return Err("source is not a Slack reaction capture".into());
    }
    services.sync_source(&source_id).await
}

#[tauri::command]
fn update_slack_source(
    services: State<'_, Arc<AppServices>>,
    source_id: String,
    reaction_name: String,
    enabled: bool,
) -> Result<(), String> {
    let mut config = services
        .store
        .source_config(&source_id)
        .map_err(|error| error.to_string())?;
    if config.source_type_id.0 != SLACK_SOURCE_TYPE {
        return Err("source is not a Slack reaction capture".into());
    }
    let reaction_name =
        slack::normalize_reaction_name(Some(&reaction_name)).map_err(|error| error.to_string())?;
    config.settings["reaction_name"] = Value::String(reaction_name.clone());
    config.display_name = format!("Slack :{reaction_name}:");
    config.enabled = enabled;
    services
        .store
        .put_source_config(&config)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn disconnect_slack(
    services: State<'_, Arc<AppServices>>,
    connection_id: String,
) -> Result<(), String> {
    let mut connection = services
        .store
        .connections()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|connection| {
            connection.id == connection_id && connection.provider_id.0 == SLACK_PROVIDER_ID
        })
        .ok_or_else(|| "Slack connection was not found".to_owned())?;
    services
        .slack_tokens
        .delete(&connection_id)
        .map_err(|error| error.to_string())?;
    connection.config["status"] = Value::String("disconnected".into());
    services
        .store
        .put_connection(&connection)
        .map_err(|error| error.to_string())?;
    for mut config in services
        .store
        .source_configs()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|config| config.connection_id == connection_id)
    {
        config.enabled = false;
        services
            .store
            .put_source_config(&config)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn run_work_command(
    services: State<'_, Arc<AppServices>>,
    work_id: String,
    command: WorkCommand,
) -> Result<(), String> {
    let result = match command {
        WorkCommand::Plan { date } => services.commands.plan(&work_id, date),
        WorkCommand::MoveToInbox => services.commands.move_to_inbox(&work_id),
        WorkCommand::MoveToBacklog => services.commands.move_to_backlog(&work_id),
        WorkCommand::Snooze { until } => services.commands.snooze(&work_id, until),
        WorkCommand::Dismiss => services.commands.dismiss(&work_id),
        WorkCommand::Pin => services.commands.pin(&work_id),
        WorkCommand::Unpin => services.commands.unpin(&work_id),
        WorkCommand::StartWork => services.commands.start_work(&work_id),
        WorkCommand::Complete => services.commands.complete(&work_id),
    };
    result.map_err(|error| error.to_string())
}

#[tauri::command]
fn open_source(
    app: tauri::AppHandle,
    services: State<'_, Arc<AppServices>>,
    work_id: String,
) -> Result<(), String> {
    let target = services
        .navigation
        .open_source_target(&work_id)
        .map_err(|error| error.to_string())?;
    app.opener()
        .open_url(target, None::<&str>)
        .map_err(|error| error.to_string())
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let services = AppServices::initialize(app).map_err(std::io::Error::other)?;
            let scheduler_services = Arc::clone(&services);
            tauri::async_runtime::spawn(async move {
                while !scheduler_services.stopping.load(Ordering::Relaxed) {
                    let _ = scheduler_services.sync_due().await;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
            });
            app.manage(services);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            dashboard,
            sync_all,
            slack_connections,
            connect_slack,
            sync_source,
            update_slack_source,
            disconnect_slack,
            run_work_command,
            open_source
        ]);

    builder
        .build(tauri::generate_context!())
        .expect("failed to build Glancelet")
        .run(|app, event| {
            if matches!(
                event,
                tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. }
            ) {
                app.state::<Arc<AppServices>>()
                    .stopping
                    .store(true, Ordering::Relaxed);
            }
        });
}

fn persist_slack_connection(
    services: &AppServices,
    authorization: slack::SlackAuthorization,
) -> glancelet_core::Result<()> {
    let existing = services
        .store
        .connections()?
        .into_iter()
        .find(|connection| {
            connection.provider_id.0 == SLACK_PROVIDER_ID
                && connection.config["team_id"] == authorization.identity.team_id
                && connection.config["user_id"] == authorization.identity.user_id
        });
    let connection_id = existing
        .as_ref()
        .map(|connection| connection.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let previous_secret = services
        .secrets
        .get(&slack::credential_key(&connection_id))?;
    services
        .slack_tokens
        .save(&connection_id, &authorization.credential)?;

    let result = (|| {
        services.store.put_connection(&Connection {
            id: connection_id.clone(),
            provider_id: ProviderId(SLACK_PROVIDER_ID.into()),
            display_name: format!(
                "{} — {}",
                authorization.identity.team_name, authorization.identity.user_name
            ),
            config: json!({
                "team_id": authorization.identity.team_id,
                "team_name": authorization.identity.team_name,
                "user_id": authorization.identity.user_id,
                "user_name": authorization.identity.user_name,
                "status": "connected"
            }),
        })?;
        let existing_source = services.store.source_configs()?.into_iter().find(|config| {
            config.connection_id == connection_id && config.source_type_id.0 == SLACK_SOURCE_TYPE
        });
        let reaction = existing_source
            .as_ref()
            .and_then(|config| config.settings["reaction_name"].as_str())
            .unwrap_or(DEFAULT_REACTION);
        services.store.put_source_config(&SourceConfig {
            id: existing_source
                .as_ref()
                .map(|config| config.id.clone())
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            connection_id: connection_id.clone(),
            source_type_id: SourceTypeId(SLACK_SOURCE_TYPE.into()),
            display_name: format!("Slack :{reaction}:"),
            enabled: true,
            expected_sync_interval_seconds: 120,
            settings: json!({
                "team_id": authorization.identity.team_id,
                "team_name": authorization.identity.team_name,
                "user_id": authorization.identity.user_id,
                "reaction_name": reaction
            }),
        })
    })();
    if let Err(error) = result {
        if let Some(previous) = previous_secret {
            let _ = services
                .secrets
                .set(&slack::credential_key(&connection_id), &previous);
        } else {
            let _ = services.slack_tokens.delete(&connection_id);
        }
        return Err(error);
    }
    Ok(())
}

async fn read_oauth_callback(listener: TcpListener) -> Result<(String, String), String> {
    let (mut socket, _) = listener
        .accept()
        .await
        .map_err(|_| "Slack OAuth callback could not be received".to_owned())?;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 2048];
    while request.len() < 16 * 1024 {
        let read = socket
            .read(&mut buffer)
            .await
            .map_err(|_| "Slack OAuth callback could not be read".to_owned())?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|part| part == b"\r\n\r\n") {
            break;
        }
    }
    let first_line = String::from_utf8_lossy(&request)
        .lines()
        .next()
        .ok_or_else(|| "Slack OAuth callback was empty".to_owned())?
        .to_owned();
    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "Slack OAuth callback was malformed".to_owned())?;
    let url = Url::parse(&format!("http://localhost{path}"))
        .map_err(|_| "Slack OAuth callback URL was malformed".to_owned())?;
    if url.path() != "/oauth/slack/callback" {
        return Err("Slack OAuth callback path was invalid".into());
    }
    let values = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    let callback_error = values
        .get("error")
        .map(|_| "Slack authorization was denied".to_owned());
    let state = values.get("state").map(ToString::to_string);
    let code = values.get("code").map(ToString::to_string);
    let success = callback_error.is_none() && state.is_some() && code.is_some();
    let body = if success {
        "<h1>Slack connected</h1><p>You can return to Glancelet.</p>"
    } else {
        "<h1>Slack connection failed</h1><p>Return to Glancelet and try again.</p>"
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    socket
        .write_all(response.as_bytes())
        .await
        .map_err(|_| "Slack OAuth callback response could not be sent".to_owned())?;
    if let Some(error) = callback_error {
        return Err(error);
    }
    Ok((
        state.ok_or_else(|| "Slack OAuth callback omitted state".to_owned())?,
        code.ok_or_else(|| "Slack OAuth callback omitted code".to_owned())?,
    ))
}

fn seed_fake_sources(store: &SqliteWorkStore) -> Result<(), String> {
    if !store
        .source_configs()
        .map_err(|error| error.to_string())?
        .is_empty()
    {
        return Ok(());
    }
    store
        .put_connection(&Connection {
            id: "fake-local".into(),
            provider_id: ProviderId("dev.glancelet.fake".into()),
            display_name: "Local demo".into(),
            config: json!({}),
        })
        .map_err(|error| error.to_string())?;
    for config in [
        SourceConfig {
            id: "fake-mirror".into(),
            connection_id: "fake-local".into(),
            source_type_id: SourceTypeId(MIRROR_SOURCE_TYPE.into()),
            display_name: "Mirror demo".into(),
            enabled: true,
            expected_sync_interval_seconds: 60,
            settings: json!({
                "records": [{
                    "identity": { "entity_type": "review", "external_id": "A" },
                    "title": "Review the Phase 0 boundary",
                    "revision": "1",
                    "display": {},
                    "metadata": { "kind": "attention", "priority": 1 },
                    "navigation": { "web_url": "https://example.com/glancelet/review/A" }
                }]
            }),
        },
        SourceConfig {
            id: "fake-capture".into(),
            connection_id: "fake-local".into(),
            source_type_id: SourceTypeId(CAPTURE_SOURCE_TYPE.into()),
            display_name: "Capture demo".into(),
            enabled: true,
            expected_sync_interval_seconds: 60,
            settings: json!({
                "records": [{
                    "identity": { "entity_type": "capture", "external_id": "B" },
                    "title": "Shape tomorrow's widget",
                    "revision": "1",
                    "display": {},
                    "metadata": { "kind": "action", "summary": "A locally completable capture" },
                    "navigation": { "web_url": "https://example.com/glancelet/capture/B" }
                }]
            }),
        },
    ] {
        store
            .put_source_config(&config)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}
