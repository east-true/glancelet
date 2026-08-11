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
    sources::google::{
        self, GoogleApiClient, GoogleCalendar, GoogleCalendarSettings, GoogleOAuthService,
        GoogleTokenProvider, DEFAULT_SYNC_INTERVAL_SECONDS as GOOGLE_SYNC_INTERVAL_SECONDS,
        PROVIDER_ID as GOOGLE_PROVIDER_ID, SOURCE_TYPE as GOOGLE_SOURCE_TYPE,
    },
    sources::notion::{
        self, NotionApiClient, NotionDataSource, NotionDataSourceSummary, NotionPreviewRow,
        NotionSourceSettings, NotionTokenProvider, DEFAULT_SYNC_INTERVAL_SECONDS,
        PROVIDER_ID as NOTION_PROVIDER_ID, SOURCE_TYPE as NOTION_SOURCE_TYPE,
    },
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
    notion_client: Arc<NotionApiClient>,
    notion_tokens: Arc<NotionTokenProvider>,
    google_client: Arc<GoogleApiClient>,
    google_tokens: Arc<GoogleTokenProvider>,
    google_oauth: GoogleOAuthService,
    google_client_id: String,
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
        let time_context = TimeContext::system().map_err(|error| error.to_string())?;
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
        let notion_client =
            Arc::new(NotionApiClient::production().map_err(|error| error.to_string())?);
        let notion_tokens = Arc::new(NotionTokenProvider::new(Arc::clone(&secrets)));
        registry
            .register(notion::registration(
                Arc::clone(&notion_client),
                Arc::clone(&notion_tokens),
            ))
            .map_err(|error| error.to_string())?;
        let google_client_id = env::var("GLANCELET_GOOGLE_CLIENT_ID").unwrap_or_default();
        let google_client =
            Arc::new(GoogleApiClient::production().map_err(|error| error.to_string())?);
        let google_tokens = Arc::new(GoogleTokenProvider::new(
            google_client_id.clone(),
            Arc::clone(&google_client),
            Arc::clone(&secrets),
            Arc::clone(&clock),
        ));
        registry
            .register(google::registration(
                Arc::clone(&google_client),
                Arc::clone(&google_tokens),
                Arc::clone(&clock),
                time_context,
            ))
            .map_err(|error| error.to_string())?;
        let registry = Arc::new(registry);
        seed_fake_sources(&store)?;
        let sync = Arc::new(SyncCoordinator::new(
            Arc::clone(&store_port),
            Arc::clone(&registry),
            Arc::clone(&clock),
        ));
        let google_oauth =
            GoogleOAuthService::production(Arc::clone(&google_client), Arc::clone(&clock));
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
            notion_client,
            notion_tokens,
            google_oauth,
            google_client,
            google_tokens,
            google_client_id,
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
        let mut selected = Vec::new();
        for config in configs
            .into_iter()
            .filter(|config| config.enabled && config.removed_at.is_none())
        {
            if only_due {
                let runtime = self
                    .store
                    .source_runtime(&config.id)
                    .map_err(|error| error.to_string())?;
                if runtime.authentication_required()
                    || runtime
                        .next_sync_at
                        .is_some_and(|next_sync| next_sync > self.clock.now())
                {
                    continue;
                }
            }
            selected.push(config);
        }
        let names = selected
            .iter()
            .map(|config| (config.id.clone(), config.display_name.clone()))
            .collect::<std::collections::HashMap<_, _>>();
        let mut failures = Vec::new();
        for (source_id, result) in self
            .sync
            .sync_many(selected.into_iter().map(|config| config.id).collect())
            .await
        {
            if let Err(error) = result {
                let name = names.get(&source_id).map_or("Source", String::as_str);
                failures.push(format!("{name}: {error}"));
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
        if !config.enabled || config.removed_at.is_some() {
            return Err("source is disabled or removed".into());
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NotionConnectionView {
    connection_id: String,
    user: String,
    status: String,
    sources: Vec<NotionSourceView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NotionSourceView {
    source_id: String,
    data_source_id: String,
    name: String,
    enabled: bool,
    settings: NotionSourceSettings,
    last_sync: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GoogleConnectionView {
    connection_id: String,
    email: String,
    status: String,
    sources: Vec<GoogleSourceView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GoogleSourceView {
    source_id: String,
    calendar_id: String,
    name: String,
    enabled: bool,
    last_sync: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleCalendarSelection {
    calendar_id: String,
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
            let authentication_required = last_error
                .as_deref()
                .is_some_and(|error| error.starts_with("authentication is required"));
            let status = connection_status(&connection, authentication_required);
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
    let callback = tokio::time::timeout(
        Duration::from_secs(300),
        read_oauth_callback(listener, "/oauth/slack/callback", "Slack"),
    )
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
    services
        .slack_tokens
        .delete(&connection_id)
        .map_err(|error| error.to_string())?;
    disable_connection_and_sources(&services, &connection_id, SLACK_PROVIDER_ID, "Slack")
}

#[tauri::command]
fn notion_connections(
    services: State<'_, Arc<AppServices>>,
) -> Result<Vec<NotionConnectionView>, String> {
    let configs = services
        .store
        .source_configs()
        .map_err(|error| error.to_string())?;
    services
        .store
        .connections()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|connection| connection.provider_id.0 == NOTION_PROVIDER_ID)
        .map(|connection| {
            let mut sources = configs
                .iter()
                .filter(|config| {
                    config.connection_id == connection.id
                        && config.source_type_id.0 == NOTION_SOURCE_TYPE
                        && config.removed_at.is_none()
                })
                .map(|config| {
                    let settings: NotionSourceSettings =
                        serde_json::from_value(config.settings.clone())
                            .map_err(|_| "invalid saved Notion settings".to_owned())?;
                    let runtime = services
                        .store
                        .source_runtime(&config.id)
                        .map_err(|error| error.to_string())?;
                    Ok(NotionSourceView {
                        source_id: config.id.clone(),
                        data_source_id: settings.data_source_id.clone(),
                        name: config.display_name.clone(),
                        enabled: config.enabled,
                        settings,
                        last_sync: runtime.last_success_at,
                        last_error: runtime.last_error,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            sources.sort_by(|a, b| a.name.cmp(&b.name));
            let reauth = sources.iter().any(|source| {
                source
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.starts_with("authentication is required"))
            });
            let status = connection_status(&connection, reauth);
            Ok(NotionConnectionView {
                connection_id: connection.id,
                user: connection.config["user_name"]
                    .as_str()
                    .unwrap_or("Notion user")
                    .to_owned(),
                status: status.into(),
                sources,
            })
        })
        .collect()
}

#[tauri::command]
async fn connect_notion(
    services: State<'_, Arc<AppServices>>,
    token: String,
) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("Notion Personal Access Token is required".into());
    }
    let identity = services
        .notion_client
        .identity(token)
        .await
        .map_err(|error| error.to_string())?;
    let existing = services
        .store
        .connections()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|connection| {
            connection.provider_id.0 == NOTION_PROVIDER_ID
                && connection.config["user_id"] == identity.id
        });
    let connection_id = existing
        .as_ref()
        .map(|connection| connection.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let previous_secret = services
        .secrets
        .get(&notion::credential_key(&connection_id))
        .map_err(|error| error.to_string())?;
    services
        .notion_tokens
        .save(&connection_id, token)
        .map_err(|error| error.to_string())?;
    let result = (|| {
        services.store.put_connection(&Connection {
            id: connection_id.clone(),
            provider_id: ProviderId(NOTION_PROVIDER_ID.into()),
            display_name: identity.name.clone(),
            config: json!({
                "user_id": identity.id,
                "user_name": identity.name,
                "status": "connected"
            }),
        })?;
        services.sync.resume_connection(&connection_id)
    })();
    if let Err(error) = result {
        if let Some(previous) = previous_secret {
            let _ = services
                .secrets
                .set(&notion::credential_key(&connection_id), &previous);
        } else {
            let _ = services.notion_tokens.delete(&connection_id);
        }
        return Err(error.to_string());
    }
    Ok(())
}

#[tauri::command]
async fn search_notion_data_sources(
    services: State<'_, Arc<AppServices>>,
    connection_id: String,
    query: String,
) -> Result<Vec<NotionDataSourceSummary>, String> {
    let token = services
        .notion_tokens
        .token(&connection_id)
        .map_err(|error| error.to_string())?;
    services
        .notion_client
        .search_data_sources(&token, Some(&query))
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn notion_data_source_schema(
    services: State<'_, Arc<AppServices>>,
    connection_id: String,
    data_source_id: String,
) -> Result<NotionDataSource, String> {
    let token = services
        .notion_tokens
        .token(&connection_id)
        .map_err(|error| error.to_string())?;
    services
        .notion_client
        .retrieve_data_source(&token, data_source_id.trim())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn preview_notion_source(
    services: State<'_, Arc<AppServices>>,
    connection_id: String,
    settings: NotionSourceSettings,
) -> Result<Vec<NotionPreviewRow>, String> {
    let token = services
        .notion_tokens
        .token(&connection_id)
        .map_err(|error| error.to_string())?;
    notion::preview(&services.notion_client, &token, &settings, 10)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn save_notion_source(
    services: State<'_, Arc<AppServices>>,
    connection_id: String,
    source_id: Option<String>,
    settings: NotionSourceSettings,
) -> Result<String, String> {
    let token = services
        .notion_tokens
        .token(&connection_id)
        .map_err(|error| error.to_string())?;
    let schema = services
        .notion_client
        .retrieve_data_source(&token, &settings.data_source_id)
        .await
        .map_err(|error| error.to_string())?;
    notion::validate_settings(&schema, &settings).map_err(|error| error.to_string())?;
    let id = match source_id {
        Some(id) => id,
        None => services
            .store
            .source_configs()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|config| {
                notion::matches_source_config(config, &connection_id, &settings.data_source_id)
            })
            .map(|config| config.id)
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
    };
    if let Ok(existing) = services.store.source_config(&id) {
        if existing.connection_id != connection_id
            || existing.source_type_id.0 != NOTION_SOURCE_TYPE
        {
            return Err("source is not a Notion task source for this connection".into());
        }
    }
    services
        .store
        .put_source_config(&SourceConfig {
            id: id.clone(),
            connection_id,
            source_type_id: SourceTypeId(NOTION_SOURCE_TYPE.into()),
            display_name: format!("Notion — {}", schema.title),
            enabled: true,
            removed_at: None,
            expected_sync_interval_seconds: DEFAULT_SYNC_INTERVAL_SECONDS,
            settings: serde_json::to_value(settings)
                .map_err(|_| "cannot encode Notion settings".to_owned())?,
        })
        .map_err(|error| error.to_string())?;
    Ok(id)
}

#[tauri::command]
fn update_notion_source(
    services: State<'_, Arc<AppServices>>,
    source_id: String,
    enabled: bool,
) -> Result<(), String> {
    let mut config = services
        .store
        .source_config(&source_id)
        .map_err(|error| error.to_string())?;
    if config.source_type_id.0 != NOTION_SOURCE_TYPE {
        return Err("source is not a Notion task source".into());
    }
    if config.removed_at.is_some() {
        return Err("removed Notion source must be reconfigured before enabling".into());
    }
    config.enabled = enabled;
    services
        .store
        .put_source_config(&config)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn remove_notion_source(
    services: State<'_, Arc<AppServices>>,
    source_id: String,
) -> Result<(), String> {
    let mut config = services
        .store
        .source_config(&source_id)
        .map_err(|error| error.to_string())?;
    if config.source_type_id.0 != NOTION_SOURCE_TYPE {
        return Err("source is not a Notion task source".into());
    }
    config.enabled = false;
    config.removed_at = Some(services.clock.now());
    services
        .store
        .put_source_config(&config)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn disconnect_notion(
    services: State<'_, Arc<AppServices>>,
    connection_id: String,
) -> Result<(), String> {
    services
        .notion_tokens
        .delete(&connection_id)
        .map_err(|error| error.to_string())?;
    disable_connection_and_sources(&services, &connection_id, NOTION_PROVIDER_ID, "Notion")
}

#[tauri::command]
fn google_connections(
    services: State<'_, Arc<AppServices>>,
) -> Result<Vec<GoogleConnectionView>, String> {
    let configs = services
        .store
        .source_configs()
        .map_err(|error| error.to_string())?;
    services
        .store
        .connections()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|connection| connection.provider_id.0 == GOOGLE_PROVIDER_ID)
        .map(|connection| {
            let mut sources = configs
                .iter()
                .filter(|config| {
                    config.connection_id == connection.id
                        && config.source_type_id.0 == GOOGLE_SOURCE_TYPE
                        && config.removed_at.is_none()
                })
                .map(|config| {
                    let settings: GoogleCalendarSettings =
                        serde_json::from_value(config.settings.clone())
                            .map_err(|_| "invalid saved Google Calendar settings".to_owned())?;
                    let runtime = services
                        .store
                        .source_runtime(&config.id)
                        .map_err(|error| error.to_string())?;
                    Ok(GoogleSourceView {
                        source_id: config.id.clone(),
                        calendar_id: settings.calendar_id,
                        name: config.display_name.clone(),
                        enabled: config.enabled,
                        last_sync: runtime.last_success_at,
                        last_error: runtime.last_error,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            sources.sort_by(|left, right| left.name.cmp(&right.name));
            let authentication_required = sources.iter().any(|source| {
                source
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.starts_with("authentication is required"))
            });
            let status = connection_status(&connection, authentication_required);
            Ok(GoogleConnectionView {
                connection_id: connection.id,
                email: connection.config["email"]
                    .as_str()
                    .unwrap_or("Google account")
                    .to_owned(),
                status: status.into(),
                sources,
            })
        })
        .collect()
}

#[tauri::command]
async fn connect_google(
    app: tauri::AppHandle,
    services: State<'_, Arc<AppServices>>,
) -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "Google OAuth loopback listener is unavailable".to_owned())?;
    let port = listener
        .local_addr()
        .map_err(|_| "Google OAuth loopback port is unavailable".to_owned())?
        .port();
    // Google Desktop OAuth registers the random-port loopback origin itself as
    // the redirect URI. Unlike our Slack callback, it must not include a path.
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let start = services
        .google_oauth
        .begin(&services.google_client_id, &redirect_uri)
        .map_err(|error| error.to_string())?;
    if let Err(error) = app
        .opener()
        .open_url(&start.authorization_url, None::<&str>)
    {
        services.google_oauth.cancel(&start.state);
        return Err(error.to_string());
    }
    let callback = tokio::time::timeout(
        Duration::from_secs(300),
        read_oauth_callback(listener, "/", "Google"),
    )
    .await
    .map_err(|_| "Google OAuth callback timed out".to_owned())
    .and_then(|result| result);
    let (state, code) = match callback {
        Ok(callback) => callback,
        Err(error) => {
            services.google_oauth.cancel(&start.state);
            return Err(error);
        }
    };
    let authorization = services
        .google_oauth
        .finish(&state, &code)
        .await
        .map_err(|error| error.to_string())?;
    persist_google_connection(&services, authorization).map_err(|error| error.to_string())
}

#[tauri::command]
async fn google_calendars(
    services: State<'_, Arc<AppServices>>,
    connection_id: String,
) -> Result<Vec<GoogleCalendar>, String> {
    let token = services
        .google_tokens
        .access_token(&connection_id)
        .await
        .map_err(|error| error.to_string())?;
    services
        .google_client
        .calendars(&token)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn save_google_calendars(
    services: State<'_, Arc<AppServices>>,
    connection_id: String,
    selections: Vec<GoogleCalendarSelection>,
) -> Result<Vec<String>, String> {
    if selections.is_empty() {
        return Err("select at least one Google Calendar".into());
    }
    let token = services
        .google_tokens
        .access_token(&connection_id)
        .await
        .map_err(|error| error.to_string())?;
    let available = services
        .google_client
        .calendars(&token)
        .await
        .map_err(|error| error.to_string())?;
    let mut source_ids = Vec::new();
    for selection in selections {
        let calendar = available
            .iter()
            .find(|calendar| calendar.id == selection.calendar_id)
            .ok_or_else(|| "selected Google Calendar is no longer accessible".to_owned())?;
        let existing = services
            .store
            .source_configs()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|config| google::matches_source_config(config, &connection_id, &calendar.id));
        let id = existing
            .as_ref()
            .map(|config| config.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        services
            .store
            .put_source_config(&SourceConfig {
                id: id.clone(),
                connection_id: connection_id.clone(),
                source_type_id: SourceTypeId(GOOGLE_SOURCE_TYPE.into()),
                display_name: calendar.display_name().to_owned(),
                enabled: true,
                removed_at: None,
                expected_sync_interval_seconds: GOOGLE_SYNC_INTERVAL_SECONDS,
                settings: serde_json::to_value(GoogleCalendarSettings {
                    calendar_id: calendar.id.clone(),
                    display_name: calendar.display_name().to_owned(),
                })
                .map_err(|_| "cannot encode Google Calendar settings".to_owned())?,
            })
            .map_err(|error| error.to_string())?;
        source_ids.push(id);
    }
    Ok(source_ids)
}

#[tauri::command]
fn update_google_source(
    services: State<'_, Arc<AppServices>>,
    source_id: String,
    enabled: bool,
) -> Result<(), String> {
    let mut config = services
        .store
        .source_config(&source_id)
        .map_err(|error| error.to_string())?;
    if config.source_type_id.0 != GOOGLE_SOURCE_TYPE {
        return Err("source is not a Google Calendar source".into());
    }
    if config.removed_at.is_some() {
        return Err("removed Google Calendar must be added again before enabling".into());
    }
    config.enabled = enabled;
    services
        .store
        .put_source_config(&config)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn remove_google_source(
    services: State<'_, Arc<AppServices>>,
    source_id: String,
) -> Result<(), String> {
    let mut config = services
        .store
        .source_config(&source_id)
        .map_err(|error| error.to_string())?;
    if config.source_type_id.0 != GOOGLE_SOURCE_TYPE {
        return Err("source is not a Google Calendar source".into());
    }
    config.enabled = false;
    config.removed_at = Some(services.clock.now());
    services
        .store
        .put_source_config(&config)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn disconnect_google(
    services: State<'_, Arc<AppServices>>,
    connection_id: String,
) -> Result<(), String> {
    services
        .google_tokens
        .delete(&connection_id)
        .map_err(|error| error.to_string())?;
    disable_connection_and_sources(&services, &connection_id, GOOGLE_PROVIDER_ID, "Google")
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
            notion_connections,
            connect_notion,
            search_notion_data_sources,
            notion_data_source_schema,
            preview_notion_source,
            save_notion_source,
            update_notion_source,
            remove_notion_source,
            disconnect_notion,
            google_connections,
            connect_google,
            google_calendars,
            save_google_calendars,
            update_google_source,
            remove_google_source,
            disconnect_google,
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
            removed_at: None,
            expected_sync_interval_seconds: 120,
            settings: json!({
                "team_id": authorization.identity.team_id,
                "team_name": authorization.identity.team_name,
                "user_id": authorization.identity.user_id,
                "reaction_name": reaction
            }),
        })?;
        services.sync.resume_connection(&connection_id)
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

fn persist_google_connection(
    services: &AppServices,
    authorization: google::GoogleAuthorization,
) -> glancelet_core::Result<()> {
    let existing = services
        .store
        .connections()?
        .into_iter()
        .find(|connection| {
            connection.provider_id.0 == GOOGLE_PROVIDER_ID
                && connection.config["sub"] == authorization.identity.sub
        });
    let connection_id = existing
        .as_ref()
        .map(|connection| connection.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let previous_secret = services
        .secrets
        .get(&google::credential_key(&connection_id))?;
    services
        .google_tokens
        .save(&connection_id, &authorization.credential)?;
    let result = (|| {
        services.store.put_connection(&Connection {
            id: connection_id.clone(),
            provider_id: ProviderId(GOOGLE_PROVIDER_ID.into()),
            display_name: authorization.identity.email.clone(),
            config: json!({
                "sub": authorization.identity.sub,
                "email": authorization.identity.email,
                "status": "connected"
            }),
        })?;
        services.sync.resume_connection(&connection_id)
    })();
    if let Err(error) = result {
        if let Some(previous) = previous_secret {
            let _ = services
                .secrets
                .set(&google::credential_key(&connection_id), &previous);
        } else {
            let _ = services.google_tokens.delete(&connection_id);
        }
        return Err(error);
    }
    Ok(())
}

fn disable_connection_and_sources(
    services: &AppServices,
    connection_id: &str,
    provider_id: &str,
    provider_name: &str,
) -> Result<(), String> {
    let mut connection = services
        .store
        .connections()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|connection| {
            connection.id == connection_id && connection.provider_id.0 == provider_id
        })
        .ok_or_else(|| format!("{provider_name} connection was not found"))?;
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

fn connection_status(connection: &Connection, authentication_required: bool) -> &'static str {
    if connection.config["status"] == "disconnected" {
        "disconnected"
    } else if authentication_required {
        "reauth_required"
    } else {
        "connected"
    }
}

async fn read_oauth_callback(
    listener: TcpListener,
    expected_path: &str,
    provider_name: &str,
) -> Result<(String, String), String> {
    let (mut socket, _) = listener
        .accept()
        .await
        .map_err(|_| format!("{provider_name} OAuth callback could not be received"))?;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 2048];
    while request.len() < 16 * 1024 {
        let read = socket
            .read(&mut buffer)
            .await
            .map_err(|_| format!("{provider_name} OAuth callback could not be read"))?;
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
        .ok_or_else(|| format!("{provider_name} OAuth callback was empty"))?
        .to_owned();
    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("{provider_name} OAuth callback was malformed"))?;
    let url = Url::parse(&format!("http://localhost{path}"))
        .map_err(|_| format!("{provider_name} OAuth callback URL was malformed"))?;
    if url.path() != expected_path {
        return Err(format!("{provider_name} OAuth callback path was invalid"));
    }
    let values = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    let callback_error = values
        .get("error")
        .map(|_| format!("{provider_name} authorization was denied"));
    let state = values.get("state").map(ToString::to_string);
    let code = values.get("code").map(ToString::to_string);
    let success = callback_error.is_none() && state.is_some() && code.is_some();
    let body = if success {
        "<h1>Connected</h1><p>You can return to Glancelet.</p>"
    } else {
        "<h1>Connection failed</h1><p>Return to Glancelet and try again.</p>"
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    socket
        .write_all(response.as_bytes())
        .await
        .map_err(|_| format!("{provider_name} OAuth callback response could not be sent"))?;
    if let Some(error) = callback_error {
        return Err(error);
    }
    Ok((
        state.ok_or_else(|| format!("{provider_name} OAuth callback omitted state"))?,
        code.ok_or_else(|| format!("{provider_name} OAuth callback omitted code"))?,
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
            removed_at: None,
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
            removed_at: None,
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
