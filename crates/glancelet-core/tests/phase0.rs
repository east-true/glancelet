use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use glancelet_core::{
    application::{
        Clock, FixedClock, Freshness, SourceChangeProcessor, SyncCoordinator, TimeContext,
        WorkCommandService, WorkReadService, WorkStore,
    },
    domain::{
        LocalDisposition, ProviderId, SourceBatch, SourceChange, SourceEntity, SourceTypeId,
        WorkDraft, WorkLifecycle, WorkPlanning, WorkProgress,
    },
    extension::{
        Connection, ExtensionRegistry, ProviderRegistration, SourceAdapter, SourceConfig,
        SourceDescriptor, SourceRegistration, WorkProjector,
    },
    sources::fake::{self, CAPTURE_SOURCE_TYPE, MIRROR_SOURCE_TYPE},
    storage::SqliteWorkStore,
    Result,
};
use serde_json::{json, Value};
use tempfile::TempDir;

struct Harness {
    store: Arc<SqliteWorkStore>,
    registry: Arc<ExtensionRegistry>,
    clock: Arc<FixedClock>,
    sync: SyncCoordinator,
    changes: SourceChangeProcessor,
    commands: WorkCommandService,
    reads: WorkReadService,
}

impl Harness {
    fn memory() -> Self {
        Self::with_store(Arc::new(SqliteWorkStore::in_memory().unwrap()))
    }

    fn with_store(store: Arc<SqliteWorkStore>) -> Self {
        Self::with_store_and_timezone(store, "UTC")
    }

    fn with_store_and_timezone(store: Arc<SqliteWorkStore>, timezone: &str) -> Self {
        let mut registry = ExtensionRegistry::new();
        registry.register(fake::registration()).unwrap();
        let registry = Arc::new(registry);
        let clock = Arc::new(FixedClock::new(at(10, 0)));
        let store_port: Arc<dyn WorkStore> = store.clone();
        let clock_port: Arc<dyn Clock> = clock.clone();
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
                Arc::clone(&registry),
                clock_port,
                TimeContext::named(timezone).unwrap(),
            ),
            store,
            registry,
            clock,
        }
    }

    fn add_source(&self, id: &str, source_type: &str, settings: Value) {
        self.store
            .put_connection(&Connection {
                id: "connection".into(),
                provider_id: ProviderId("dev.glancelet.fake".into()),
                display_name: "Fake account".into(),
                config: json!({}),
            })
            .unwrap();
        self.store
            .put_source_config(&SourceConfig {
                id: id.into(),
                connection_id: "connection".into(),
                source_type_id: SourceTypeId(source_type.into()),
                display_name: id.into(),
                enabled: true,
                removed_at: None,
                expected_sync_interval_seconds: 60,
                settings,
            })
            .unwrap();
    }

    fn settings(&self, id: &str, settings: Value) {
        let mut config = self.store.source_config(id).unwrap();
        config.settings = settings;
        self.store.put_source_config(&config).unwrap();
    }

    async fn sync_and_project(&self, id: &str) -> usize {
        let changes = self.sync.sync(id).await.unwrap();
        self.changes.process_pending(100).unwrap();
        changes
    }
}

#[test]
fn migration_baseline_handles_fresh_legacy_and_reopened_databases() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("migration.db");
    {
        let store = SqliteWorkStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 5);
    }
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute("DROP TABLE schema_migrations", [])
            .unwrap();
    }
    assert_eq!(
        SqliteWorkStore::open(&path)
            .unwrap()
            .schema_version()
            .unwrap(),
        4
    );
    assert_eq!(
        SqliteWorkStore::open(&path)
            .unwrap()
            .schema_version()
            .unwrap(),
        4
    );
}

#[test]
fn source_lifecycle_migration_preserves_history_and_moves_legacy_remove_marker() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("lifecycle.db");
    {
        let store = SqliteWorkStore::open(&path).unwrap();
        store
            .put_connection(&Connection {
                id: "connection".into(),
                provider_id: ProviderId("provider".into()),
                display_name: "Account".into(),
                config: json!({}),
            })
            .unwrap();
        store
            .put_source_config(&SourceConfig {
                id: "source".into(),
                connection_id: "connection".into(),
                source_type_id: SourceTypeId("source.type".into()),
                display_name: "Source".into(),
                enabled: true,
                removed_at: None,
                expected_sync_interval_seconds: 60,
                settings: json!({}),
            })
            .unwrap();
    }
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute("DELETE FROM schema_migrations WHERE version=2", [])
            .unwrap();
        connection
            .execute(
                "UPDATE source_configs
                 SET settings_json='{\"_removed\":true}', removed_at=NULL, enabled=1
                 WHERE id='source'",
                [],
            )
            .unwrap();
    }
    let store = SqliteWorkStore::open(&path).unwrap();
    let config = store.source_config("source").unwrap();
    assert_eq!(store.schema_version().unwrap(), 5);
    assert!(!config.enabled);
    assert!(config.removed_at.is_some());
    assert!(config.settings.get("_removed").is_none());
    assert_eq!(store.source_configs().unwrap().len(), 1);
}

#[tokio::test]
async fn disabled_and_removed_sources_do_not_sync_and_restore_forces_reconciliation() {
    let harness = Harness::memory();
    let mut settings = snapshot(vec![record("A", "Alpha", "1")]);
    settings["checkpoint"] = json!({"cursor":"old"});
    harness.add_source("source", MIRROR_SOURCE_TYPE, settings);
    harness.sync_and_project("source").await;
    assert!(harness
        .store
        .source_runtime("source")
        .unwrap()
        .checkpoint
        .is_some());

    let mut config = harness.store.source_config("source").unwrap();
    config.enabled = false;
    harness.store.put_source_config(&config).unwrap();
    assert!(harness.sync.sync("source").await.is_err());
    assert_eq!(harness.store.stored_work().unwrap().len(), 1);

    config.removed_at = Some(at(11, 0));
    harness.store.put_source_config(&config).unwrap();
    assert!(harness.sync.sync("source").await.is_err());
    assert_eq!(harness.store.stored_work().unwrap().len(), 1);

    config.enabled = true;
    config.removed_at = None;
    harness.store.put_source_config(&config).unwrap();
    let runtime = harness.store.source_runtime("source").unwrap();
    assert!(runtime.checkpoint.is_none());
    assert!(runtime.next_sync_at.is_none());
    assert_eq!(runtime.failure_count, 0);
    harness.sync_and_project("source").await;
    assert_eq!(harness.store.source_configs().unwrap().len(), 1);
    assert_eq!(harness.store.stored_work().unwrap().len(), 1);
}

#[tokio::test]
async fn asia_seoul_local_date_drives_planned_today_across_utc_boundary() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    let harness = Harness::with_store_and_timezone(store, "Asia/Seoul");
    harness.clock.set(
        Utc.with_ymd_and_hms(2026, 8, 11, 16, 30, 0)
            .single()
            .unwrap(),
    );
    harness.add_source(
        "capture",
        CAPTURE_SOURCE_TYPE,
        snapshot(vec![record("local-day", "Local Wednesday", "1")]),
    );
    harness.sync_and_project("capture").await;
    let id = harness.store.stored_work().unwrap()[0].entry.id.clone();
    harness
        .commands
        .plan(&id, NaiveDate::from_ymd_opt(2026, 8, 12).unwrap())
        .unwrap();

    let dashboard = harness.reads.dashboard().unwrap();
    assert_eq!(dashboard.today.len(), 1);
    assert_eq!(dashboard.today[0].title, "Local Wednesday");
}

#[test]
fn time_context_uses_iana_dst_rules() {
    let new_york = TimeContext::named("America/New_York").unwrap();
    let before_fall_back = Utc
        .with_ymd_and_hms(2026, 11, 1, 5, 30, 0)
        .single()
        .unwrap();
    let after_fall_back = Utc
        .with_ymd_and_hms(2026, 11, 1, 6, 30, 0)
        .single()
        .unwrap();
    assert_eq!(
        new_york.local_date(before_fall_back),
        new_york.local_date(after_fall_back)
    );
}

#[tokio::test]
async fn mirror_is_idempotent_and_preserves_local_state_until_reactivation() {
    let harness = Harness::memory();
    harness.add_source(
        "mirror",
        MIRROR_SOURCE_TYPE,
        snapshot(vec![record("A", "Alpha", "1")]),
    );

    assert_eq!(harness.sync_and_project("mirror").await, 1);
    let original = harness.store.stored_work().unwrap().pop().unwrap();
    let planned = NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
    harness.commands.plan(&original.entry.id, planned).unwrap();
    harness.commands.pin(&original.entry.id).unwrap();
    harness
        .commands
        .snooze(&original.entry.id, at(12, 0))
        .unwrap();

    harness.settings("mirror", snapshot(vec![record("A", "Alpha changed", "2")]));
    harness.sync_and_project("mirror").await;
    let updated = harness.store.stored_work().unwrap().pop().unwrap().entry;
    assert_eq!(updated.title, "Alpha changed");
    assert_eq!(updated.planning, Some(WorkPlanning::Planned(planned)));
    assert_eq!(updated.disposition, LocalDisposition::Snoozed);
    assert!(updated.pinned);
    assert!(harness.commands.complete(&original.entry.id).is_err());
    harness.commands.dismiss(&original.entry.id).unwrap();
    assert_eq!(
        harness
            .store
            .stored_work_by_id(&original.entry.id)
            .unwrap()
            .entry
            .disposition,
        LocalDisposition::Dismissed
    );

    harness.settings("mirror", snapshot(vec![]));
    harness.sync_and_project("mirror").await;
    assert_eq!(
        harness.store.stored_work().unwrap()[0].entry.lifecycle,
        WorkLifecycle::Resolved
    );

    harness.settings("mirror", snapshot(vec![record("A", "Alpha returns", "3")]));
    harness.sync_and_project("mirror").await;
    let reactivated = harness.store.stored_work().unwrap();
    assert_eq!(reactivated.len(), 1);
    assert_eq!(reactivated[0].entry.lifecycle, WorkLifecycle::Active);
    assert_eq!(reactivated[0].entry.planning, Some(WorkPlanning::Inbox));
    assert_eq!(reactivated[0].entry.disposition, LocalDisposition::Normal);
    assert_eq!(reactivated[0].entry.snoozed_until, None);
    assert!(reactivated[0].entry.pinned);

    assert_eq!(harness.sync_and_project("mirror").await, 0);
    assert_eq!(harness.store.stored_work().unwrap().len(), 1);
}

#[tokio::test]
async fn failed_fetch_does_not_deactivate_and_delta_only_applies_explicit_mutations() {
    let harness = Harness::memory();
    harness.add_source(
        "mirror",
        MIRROR_SOURCE_TYPE,
        snapshot(vec![record("A", "Alpha", "1"), record("B", "Beta", "1")]),
    );
    harness.sync_and_project("mirror").await;

    harness.settings("mirror", json!({ "fail": true }));
    assert!(harness.sync.sync("mirror").await.is_err());
    assert_eq!(harness.store.stored_work().unwrap().len(), 2);
    assert!(harness
        .store
        .stored_work()
        .unwrap()
        .iter()
        .all(|work| work.entry.lifecycle == WorkLifecycle::Active));
    assert_eq!(
        harness
            .store
            .source_runtime("mirror")
            .unwrap()
            .failure_count,
        1
    );
    assert_eq!(
        harness.store.source_runtime("mirror").unwrap().next_sync_at,
        Some(at(10, 1))
    );

    harness.settings(
        "mirror",
        json!({
            "batch_kind": "delta",
            "records": [record("A", "Alpha delta", "2")]
        }),
    );
    harness.sync_and_project("mirror").await;
    let work = harness.store.stored_work().unwrap();
    assert_eq!(work.len(), 2);
    assert!(work.iter().any(|item| item.entry.title == "Beta"));
    assert!(work
        .iter()
        .all(|item| item.entry.lifecycle == WorkLifecycle::Active));
}

#[tokio::test]
async fn source_change_is_durable_across_restart_and_reprocessing_is_idempotent() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("glancelet.db");
    let harness = Harness::with_store(Arc::new(SqliteWorkStore::open(&path).unwrap()));
    harness.add_source(
        "capture",
        CAPTURE_SOURCE_TYPE,
        snapshot(vec![record("B", "Captured", "1")]),
    );
    harness.sync.sync("capture").await.unwrap();
    let pending = harness.store.pending_source_changes(10).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(harness.store.stored_work().unwrap().is_empty());
    drop(harness);

    let restarted = Harness::with_store(Arc::new(SqliteWorkStore::open(&path).unwrap()));
    let change = restarted
        .store
        .pending_source_changes(10)
        .unwrap()
        .pop()
        .unwrap();
    let config = restarted
        .store
        .source_config(&change.source_entity.source_config_id)
        .unwrap();
    let projector = restarted
        .registry
        .projector(&config.source_type_id)
        .unwrap();
    let draft = projector.project(&change.source_entity, &change).unwrap();
    restarted
        .store
        .apply_projection(&change, &draft, projector.version(), restarted.clock.now())
        .unwrap();
    restarted
        .store
        .apply_projection(&change, &draft, projector.version(), restarted.clock.now())
        .unwrap();
    assert_eq!(restarted.store.stored_work().unwrap().len(), 1);
    assert!(restarted
        .store
        .pending_source_changes(10)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn capture_marker_lifetime_is_independent_and_recapture_creates_history() {
    let harness = Harness::memory();
    harness.add_source(
        "capture",
        CAPTURE_SOURCE_TYPE,
        snapshot(vec![record("B", "Captured", "1")]),
    );
    harness.sync_and_project("capture").await;
    let first_id = harness.store.stored_work().unwrap()[0].entry.id.clone();
    harness.commands.start_work(&first_id).unwrap();
    assert_eq!(
        harness
            .store
            .stored_work_by_id(&first_id)
            .unwrap()
            .entry
            .progress,
        Some(WorkProgress::Doing)
    );
    harness.settings("capture", snapshot(vec![]));
    harness.sync_and_project("capture").await;
    let still_active = harness.store.stored_work_by_id(&first_id).unwrap().entry;
    assert_eq!(still_active.progress, Some(WorkProgress::Doing));
    assert_eq!(still_active.lifecycle, WorkLifecycle::Active);

    harness.commands.complete(&first_id).unwrap();
    let completed = harness.store.stored_work_by_id(&first_id).unwrap().entry;
    assert_eq!(completed.progress, Some(WorkProgress::Done));
    assert_eq!(completed.lifecycle, WorkLifecycle::Resolved);

    harness.settings(
        "capture",
        snapshot(vec![record("B", "Captured again", "2")]),
    );
    harness.sync_and_project("capture").await;
    let history = harness.store.stored_work().unwrap();
    assert_eq!(history.len(), 2);
    assert!(history
        .iter()
        .any(|work| work.entry.id == first_id && work.entry.lifecycle == WorkLifecycle::Resolved));
    assert!(history.iter().any(|work| work.entry.id != first_id
        && work.entry.lifecycle == WorkLifecycle::Active
        && work.entry.progress == Some(WorkProgress::Todo)));
}

#[tokio::test]
async fn fixed_clock_controls_snooze_today_and_freshness_without_persisted_stale_state() {
    let harness = Harness::memory();
    harness.add_source(
        "capture",
        CAPTURE_SOURCE_TYPE,
        snapshot(vec![record("B", "Today", "1")]),
    );
    harness.sync_and_project("capture").await;
    let id = harness.store.stored_work().unwrap()[0].entry.id.clone();
    harness.commands.pin(&id).unwrap();
    let dashboard = harness.reads.dashboard().unwrap();
    assert_eq!(dashboard.today[0].freshness, Freshness::Fresh);
    assert_eq!(dashboard.inbox.len(), 1);

    harness.commands.snooze(&id, at(11, 0)).unwrap();
    assert!(harness.reads.dashboard().unwrap().today.is_empty());
    harness.clock.set(at(11, 1));
    assert_eq!(harness.reads.dashboard().unwrap().today.len(), 1);
    harness.clock.set(at(10, 3) + chrono::Duration::days(1));
    assert_eq!(
        harness.reads.dashboard().unwrap().today[0].freshness,
        Freshness::Stale
    );
}

#[test]
fn a_fork_source_registers_without_changing_core_vocabulary() {
    struct Adapter;
    #[async_trait]
    impl SourceAdapter for Adapter {
        async fn fetch(&self, _: &SourceConfig, _: Option<Value>) -> Result<SourceBatch> {
            unreachable!()
        }
    }
    struct Projector;
    impl WorkProjector for Projector {
        fn project(&self, _: &SourceEntity, _: &SourceChange) -> Result<WorkDraft> {
            unreachable!()
        }
    }

    let source_type = SourceTypeId("com.company.internal-ticket".into());
    let mut registry = ExtensionRegistry::new();
    registry
        .register(ProviderRegistration {
            provider_id: ProviderId("com.company".into()),
            display_name: "Company".into(),
            sources: vec![SourceRegistration {
                descriptor: SourceDescriptor {
                    source_type_id: source_type.clone(),
                    display_name: "Internal tickets".into(),
                    description: "Fork-owned source".into(),
                },
                adapter: Arc::new(Adapter),
                projector: Arc::new(Projector),
            }],
        })
        .unwrap();
    assert_eq!(
        registry
            .display_metadata(&source_type)
            .unwrap()
            .provider_id
            .0,
        "com.company"
    );
}

fn record(id: &str, title: &str, revision: &str) -> Value {
    json!({
        "identity": { "entity_type": "task", "external_id": id },
        "title": title,
        "revision": revision,
        "display": { "label": "Fake" },
        "metadata": { "kind": "action" },
        "navigation": { "web_url": format!("https://example.test/tasks/{id}") }
    })
}

fn snapshot(records: Vec<Value>) -> Value {
    json!({ "batch_kind": "full_snapshot", "records": records })
}

fn at(hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 11, hour, minute, 0)
        .single()
        .unwrap()
}
