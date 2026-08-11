use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;
use chrono::{NaiveDate, TimeZone, Utc};
use glancelet_core::{
    application::{
        Clock, FixedClock, InMemorySecretStore, SecretStore, SourceChangeProcessor,
        SyncCoordinator, TimeContext, WorkCommandService, WorkReadService, WorkStore,
    },
    domain::{
        LocalDisposition, ProgressAuthority, ProviderId, SourceBatch, SourceBatchKind,
        SourceChange, SourceIdentity, SourceMutation, SourceRecord, SourceTypeId, TemporalValue,
        WorkBindingMode, WorkDraft, WorkKind, WorkPlanning,
    },
    extension::{
        Connection, ExtensionRegistry, ProviderRegistration, SourceAdapter, SourceConfig,
        SourceDescriptor, SourceRegistration, WorkProjector,
    },
    sources::{google, notion, slack},
    storage::SqliteWorkStore,
    GlanceletError, Result,
};
use serde_json::json;
use tokio::sync::Notify;

const SLACK_SOURCE: &str = slack::SOURCE_TYPE;
const NOTION_SOURCE: &str = notion::SOURCE_TYPE;
const GOOGLE_SOURCE: &str = google::SOURCE_TYPE;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 11, 1, 0, 0).single().unwrap()
}

#[derive(Default)]
struct ConcurrencyState {
    running: Mutex<HashMap<String, usize>>,
    max_running: Mutex<HashMap<String, usize>>,
    google_c_started: AtomicBool,
    release_google_c: Notify,
}

impl ConcurrencyState {
    fn entered(&self, id: &str) -> RunningGuard<'_> {
        let current = {
            let mut running = self.running.lock().unwrap();
            let current = running.entry(id.to_owned()).or_default();
            *current += 1;
            *current
        };
        let mut maximum = self.max_running.lock().unwrap();
        let value = maximum.entry(id.to_owned()).or_default();
        *value = (*value).max(current);
        RunningGuard {
            state: self,
            id: id.to_owned(),
        }
    }

    fn max_for(&self, id: &str) -> usize {
        self.max_running
            .lock()
            .unwrap()
            .get(id)
            .copied()
            .unwrap_or_default()
    }
}

struct RunningGuard<'a> {
    state: &'a ConcurrencyState,
    id: String,
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        *self
            .state
            .running
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .unwrap() -= 1;
    }
}

struct GateAdapter {
    state: Arc<ConcurrencyState>,
}

#[async_trait]
impl SourceAdapter for GateAdapter {
    async fn fetch(
        &self,
        config: &SourceConfig,
        _checkpoint: Option<serde_json::Value>,
    ) -> Result<SourceBatch> {
        let _running = self.state.entered(&config.id);
        match config.id.as_str() {
            "slack" => return Err(GlanceletError::Source("Slack unavailable".into())),
            "notion" => {
                return Err(GlanceletError::RateLimited {
                    provider: "Notion".into(),
                    retry_after_seconds: 30,
                })
            }
            "google-c" => {
                self.state.google_c_started.store(true, Ordering::SeqCst);
                self.state.release_google_c.notified().await;
            }
            "google-d" => {
                while !self.state.google_c_started.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
                self.state.release_google_c.notify_one();
            }
            _ => {}
        }
        Ok(batch_for(config))
    }
}

struct GateProjector {
    kind: WorkKind,
    mode: WorkBindingMode,
    authority: ProgressAuthority,
}

impl WorkProjector for GateProjector {
    fn project(
        &self,
        entity: &glancelet_core::domain::SourceEntity,
        _: &SourceChange,
    ) -> Result<WorkDraft> {
        Ok(WorkDraft {
            kind: self.kind,
            title: entity.title.clone(),
            summary: None,
            priority: None,
            progress: match self.authority {
                ProgressAuthority::None => None,
                _ => Some(glancelet_core::domain::WorkProgress::Todo),
            },
            start: (self.kind == WorkKind::Event).then_some(TemporalValue::Date {
                date: NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
            }),
            end: (self.kind == WorkKind::Event).then_some(TemporalValue::Date {
                date: NaiveDate::from_ymd_opt(2026, 8, 12).unwrap(),
            }),
            due: None,
            dimensions: json!({}),
            facets: json!({}),
            binding_mode: self.mode,
            progress_authority: self.authority,
        })
    }
}

fn batch_for(config: &SourceConfig) -> SourceBatch {
    SourceBatch {
        kind: SourceBatchKind::FullSnapshot,
        mutations: vec![SourceMutation::Upsert(SourceRecord {
            identity: SourceIdentity {
                entity_type: "gate".into(),
                external_id: "same-external-id".into(),
            },
            title: config.display_name.clone(),
            revision: "1".into(),
            display: json!({}),
            metadata: json!({}),
            navigation: json!({"web_url":"https://example.test/work"}),
        })],
        next_checkpoint: Some(json!({"cursor":config.id})),
    }
}

fn registration(
    provider: &str,
    source_type: &str,
    adapter: Arc<dyn SourceAdapter>,
    kind: WorkKind,
    mode: WorkBindingMode,
    authority: ProgressAuthority,
) -> ProviderRegistration {
    ProviderRegistration {
        provider_id: ProviderId(provider.into()),
        display_name: provider.into(),
        sources: vec![SourceRegistration {
            descriptor: SourceDescriptor {
                source_type_id: SourceTypeId(source_type.into()),
                display_name: source_type.into(),
                description: "integration gate".into(),
            },
            adapter,
            projector: Arc::new(GateProjector {
                kind,
                mode,
                authority,
            }),
        }],
    }
}

fn registry(adapter: Arc<dyn SourceAdapter>) -> Arc<ExtensionRegistry> {
    let mut registry = ExtensionRegistry::new();
    registry
        .register(registration(
            slack::PROVIDER_ID,
            SLACK_SOURCE,
            Arc::clone(&adapter),
            WorkKind::Action,
            WorkBindingMode::Capture,
            ProgressAuthority::Local,
        ))
        .unwrap();
    registry
        .register(registration(
            notion::PROVIDER_ID,
            NOTION_SOURCE,
            Arc::clone(&adapter),
            WorkKind::Action,
            WorkBindingMode::Mirror,
            ProgressAuthority::External,
        ))
        .unwrap();
    registry
        .register(registration(
            google::PROVIDER_ID,
            GOOGLE_SOURCE,
            adapter,
            WorkKind::Event,
            WorkBindingMode::Mirror,
            ProgressAuthority::None,
        ))
        .unwrap();
    Arc::new(registry)
}

fn add_config(store: &SqliteWorkStore, id: &str, provider: &str, source_type: &str) {
    let connection_id = format!("{provider}-connection");
    if !store
        .connections()
        .unwrap()
        .iter()
        .any(|connection| connection.id == connection_id)
    {
        store
            .put_connection(&Connection {
                id: connection_id.clone(),
                provider_id: ProviderId(provider.into()),
                display_name: provider.into(),
                config: json!({"status":"connected"}),
            })
            .unwrap();
    }
    store
        .put_source_config(&SourceConfig {
            id: id.into(),
            connection_id,
            source_type_id: SourceTypeId(source_type.into()),
            display_name: id.into(),
            enabled: true,
            removed_at: None,
            expected_sync_interval_seconds: 300,
            settings: json!({}),
        })
        .unwrap();
}

#[tokio::test]
async fn multi_provider_sync_isolates_failures_and_keeps_per_source_single_flight() {
    let state = Arc::new(ConcurrencyState::default());
    let adapter: Arc<dyn SourceAdapter> = Arc::new(GateAdapter {
        state: Arc::clone(&state),
    });
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    add_config(&store, "slack", slack::PROVIDER_ID, SLACK_SOURCE);
    add_config(&store, "notion", notion::PROVIDER_ID, NOTION_SOURCE);
    add_config(&store, "google-c", google::PROVIDER_ID, GOOGLE_SOURCE);
    add_config(&store, "google-d", google::PROVIDER_ID, GOOGLE_SOURCE);
    let store_port: Arc<dyn WorkStore> = store.clone();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let registry = registry(adapter);
    let sync = Arc::new(SyncCoordinator::new(
        Arc::clone(&store_port),
        Arc::clone(&registry),
        Arc::clone(&clock),
    ));

    let results = sync
        .sync_many(vec![
            "slack".into(),
            "notion".into(),
            "google-c".into(),
            "google-d".into(),
        ])
        .await;
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        2
    );
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_err()).count(),
        2
    );
    assert_eq!(
        store
            .source_runtime("google-c")
            .unwrap()
            .checkpoint
            .unwrap()["cursor"],
        "google-c"
    );
    assert_eq!(
        store
            .source_runtime("google-d")
            .unwrap()
            .checkpoint
            .unwrap()["cursor"],
        "google-d"
    );
    assert!(store.source_runtime("slack").unwrap().checkpoint.is_none());
    assert!(store.source_runtime("notion").unwrap().checkpoint.is_none());

    let processor = SourceChangeProcessor::new(store_port, registry, clock);
    assert_eq!(processor.process_pending(100).unwrap(), 2);
    assert_eq!(store.stored_work().unwrap().len(), 2);

    let (first, second) = tokio::join!(sync.sync("google-d"), sync.sync("google-d"));
    assert!(first.is_ok() && second.is_ok());
    assert_eq!(state.max_for("google-d"), 1);
}

struct AuthAdapter {
    require_authentication: AtomicBool,
    calls: AtomicUsize,
}

#[async_trait]
impl SourceAdapter for AuthAdapter {
    async fn fetch(
        &self,
        config: &SourceConfig,
        _: Option<serde_json::Value>,
    ) -> Result<SourceBatch> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.require_authentication.load(Ordering::SeqCst) {
            Err(GlanceletError::AuthenticationRequired(
                "Reconnect account".into(),
            ))
        } else {
            Ok(batch_for(config))
        }
    }
}

#[tokio::test]
async fn authentication_required_suspends_polling_until_the_same_connection_reconnects() {
    let adapter = Arc::new(AuthAdapter {
        require_authentication: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    add_config(&store, "notion-auth", notion::PROVIDER_ID, NOTION_SOURCE);
    let mut extensions = ExtensionRegistry::new();
    extensions
        .register(registration(
            notion::PROVIDER_ID,
            NOTION_SOURCE,
            adapter.clone(),
            WorkKind::Action,
            WorkBindingMode::Mirror,
            ProgressAuthority::External,
        ))
        .unwrap();
    let extensions = Arc::new(extensions);
    let store_port: Arc<dyn WorkStore> = store.clone();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let sync = Arc::new(SyncCoordinator::new(
        store_port.clone(),
        extensions.clone(),
        clock.clone(),
    ));
    sync.sync("notion-auth").await.unwrap();
    SourceChangeProcessor::new(store_port, extensions, clock)
        .process_pending(10)
        .unwrap();
    let checkpoint = store.source_runtime("notion-auth").unwrap().checkpoint;

    adapter.require_authentication.store(true, Ordering::SeqCst);
    assert!(matches!(
        sync.sync("notion-auth").await,
        Err(GlanceletError::AuthenticationRequired(_))
    ));
    let runtime = store.source_runtime("notion-auth").unwrap();
    assert_eq!(runtime.checkpoint, checkpoint);
    assert!(runtime.next_sync_at.is_none());
    assert!(runtime.authentication_required());
    assert_eq!(store.stored_work().unwrap().len(), 1);
    assert!(store.pending_source_changes(10).unwrap().is_empty());

    // Scheduled and manual entry points both stop before the provider while the
    // connection needs user action.
    assert!(sync
        .sync_many(vec!["notion-auth".into()])
        .await
        .into_iter()
        .all(|(_, result)| matches!(result, Err(GlanceletError::AuthenticationRequired(_)))));
    assert!(matches!(
        sync.sync("notion-auth").await,
        Err(GlanceletError::AuthenticationRequired(_))
    ));
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 2);

    adapter
        .require_authentication
        .store(false, Ordering::SeqCst);
    sync.resume_connection("notion-connection").unwrap();
    let resumed = store.source_runtime("notion-auth").unwrap();
    assert_eq!(resumed.checkpoint, checkpoint);
    assert!(!resumed.authentication_required());
    sync.sync("notion-auth").await.unwrap();
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        store
            .source_configs()
            .unwrap()
            .into_iter()
            .filter(|config| config.connection_id == "notion-connection")
            .count(),
        1
    );
}

#[tokio::test]
async fn restart_recovers_provider_state_local_work_state_and_shared_secret_backend() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("restart.db");
    let secrets: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
    let secret_values = [
        (slack::credential_key("slack-connection"), "slack-token"),
        (notion::credential_key("notion-connection"), "notion-pat"),
        (
            google::credential_key("google-connection"),
            "google-refresh-token",
        ),
    ];
    for (key, value) in &secret_values {
        secrets.set(key, value).unwrap();
    }
    let state = Arc::new(ConcurrencyState::default());
    let adapter: Arc<dyn SourceAdapter> = Arc::new(GateAdapter { state });
    let extensions = registry(adapter);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    {
        let store = Arc::new(SqliteWorkStore::open(&path).unwrap());
        add_config(&store, "restart-slack", slack::PROVIDER_ID, SLACK_SOURCE);
        add_config(&store, "restart-notion", notion::PROVIDER_ID, NOTION_SOURCE);
        add_config(
            &store,
            "restart-google-a",
            google::PROVIDER_ID,
            GOOGLE_SOURCE,
        );
        add_config(
            &store,
            "restart-google-b",
            google::PROVIDER_ID,
            GOOGLE_SOURCE,
        );
        let store_port: Arc<dyn WorkStore> = store.clone();
        let sync = Arc::new(SyncCoordinator::new(
            store_port.clone(),
            extensions.clone(),
            clock.clone(),
        ));
        assert!(sync
            .sync_many(vec![
                "restart-slack".into(),
                "restart-notion".into(),
                "restart-google-a".into(),
                "restart-google-b".into(),
            ])
            .await
            .iter()
            .all(|(_, result)| result.is_ok()));
        SourceChangeProcessor::new(store_port.clone(), extensions.clone(), clock.clone())
            .process_pending(20)
            .unwrap();
        let work = store.stored_work().unwrap();
        let id_for = |source_id: &str| {
            work.iter()
                .find(|item| item.source_config.id == source_id)
                .unwrap()
                .entry
                .id
                .clone()
        };
        let commands = WorkCommandService::new(store_port, clock.clone());
        commands
            .plan(
                &id_for("restart-slack"),
                NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
            )
            .unwrap();
        commands
            .snooze(
                &id_for("restart-notion"),
                now() + chrono::Duration::hours(2),
            )
            .unwrap();
        commands.pin(&id_for("restart-google-a")).unwrap();
        commands.dismiss(&id_for("restart-google-b")).unwrap();
    }

    let database = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
    for (_, secret) in &secret_values {
        assert!(!database.contains(secret));
    }
    let reopened = Arc::new(SqliteWorkStore::open(&path).unwrap());
    let work = reopened.stored_work().unwrap();
    assert_eq!(work.len(), 4);
    let by_source = |id: &str| {
        work.iter()
            .find(|item| item.source_config.id == id)
            .unwrap()
    };
    assert_eq!(
        by_source("restart-slack").entry.planning,
        Some(WorkPlanning::Planned(
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap()
        ))
    );
    assert_eq!(
        by_source("restart-notion").entry.disposition,
        LocalDisposition::Snoozed
    );
    assert!(by_source("restart-google-a").entry.pinned);
    assert_eq!(
        by_source("restart-google-b").entry.disposition,
        LocalDisposition::Dismissed
    );
    assert!(reopened
        .source_runtime("restart-google-a")
        .unwrap()
        .checkpoint
        .is_some());
    for (key, value) in &secret_values {
        assert_eq!(secrets.get(key).unwrap().as_deref(), Some(*value));
    }
    let store_port: Arc<dyn WorkStore> = reopened;
    let dashboard = WorkReadService::new(
        store_port,
        extensions,
        clock,
        TimeContext::named("Asia/Seoul").unwrap(),
    )
    .dashboard()
    .unwrap();
    assert!(dashboard
        .today
        .iter()
        .any(|item| item.kind == WorkKind::Action));
    assert!(dashboard
        .today
        .iter()
        .any(|item| item.kind == WorkKind::Event));
}
