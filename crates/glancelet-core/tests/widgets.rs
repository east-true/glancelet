use std::sync::Arc;

use chrono::{TimeZone, Utc};
use glancelet_core::{
    application::{
        Clock, FixedClock, SourceChangeProcessor, SyncCoordinator, TimeContext, WidgetInstance,
        WidgetLayoutService, WidgetSize, WidgetType, WorkCommandService, WorkReadService,
        WorkStore,
    },
    domain::{ProviderId, SourceTypeId, WorkPlanning},
    extension::{Connection, ExtensionRegistry, SourceConfig},
    sources::fake::{self, CAPTURE_SOURCE_TYPE, MIRROR_SOURCE_TYPE},
    storage::SqliteWorkStore,
};
use serde_json::{json, Value};
use tempfile::TempDir;

struct Harness {
    store: Arc<SqliteWorkStore>,
    clock: Arc<FixedClock>,
    sync: SyncCoordinator,
    changes: SourceChangeProcessor,
    reads: WorkReadService,
    commands: WorkCommandService,
}

impl Harness {
    fn new() -> Self {
        let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
        let mut registry = ExtensionRegistry::new();
        registry.register(fake::registration()).unwrap();
        let registry = Arc::new(registry);
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 8, 14, 3, 0, 0).single().unwrap(),
        ));
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
            reads: WorkReadService::new(
                Arc::clone(&store_port),
                registry,
                Arc::clone(&clock_port),
                TimeContext::named("Asia/Seoul").unwrap(),
            ),
            commands: WorkCommandService::new(store_port, clock_port),
            store,
            clock,
        }
    }

    async fn add(&self, id: &str, source_type: &str, records: Vec<Value>) {
        self.store
            .put_connection(&Connection {
                id: "connection".into(),
                provider_id: ProviderId("dev.glancelet.fake".into()),
                display_name: "Fake".into(),
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
                expected_sync_interval_seconds: 300,
                settings: json!({"records": records}),
            })
            .unwrap();
        self.sync.sync(id).await.unwrap();
        self.changes.process_pending(100).unwrap();
    }
}

fn record(id: &str, title: &str, metadata: Value) -> Value {
    json!({
        "identity": {"entity_type": "work", "external_id": id},
        "title": title,
        "revision": "1",
        "display": {},
        "metadata": metadata,
        "navigation": {"url": format!("https://example.com/{id}")}
    })
}

#[tokio::test]
async fn built_in_widget_queries_keep_today_inbox_upcoming_and_attention_distinct() {
    let harness = Harness::new();
    harness
        .add(
            "mirror",
            MIRROR_SOURCE_TYPE,
            vec![
                record(
                    "today-event",
                    "Today event",
                    json!({
                        "kind": "event",
                        "start": {"type":"date","date":"2026-08-14"},
                        "end": {"type":"date","date":"2026-08-15"}
                    }),
                ),
                record(
                    "multi-event",
                    "Multi-day event",
                    json!({
                        "kind": "event",
                        "start": {"type":"date","date":"2026-08-14"},
                        "end": {"type":"date","date":"2026-08-17"}
                    }),
                ),
                record("attention", "Build failing", json!({"kind": "attention"})),
                record(
                    "due",
                    "Due Monday",
                    json!({"due": {"type":"date","date":"2026-08-17"}}),
                ),
            ],
        )
        .await;
    harness
        .add(
            "capture",
            CAPTURE_SOURCE_TYPE,
            vec![record("inbox", "Unplanned task", json!({}))],
        )
        .await;

    let stored = harness.store.stored_work().unwrap();
    let inbox = stored
        .iter()
        .find(|work| work.entry.title == "Unplanned task")
        .unwrap();
    harness.commands.move_to_inbox(&inbox.entry.id).unwrap();
    let due = stored
        .iter()
        .find(|work| work.entry.title == "Due Monday")
        .unwrap();
    harness
        .commands
        .plan(
            &due.entry.id,
            chrono::NaiveDate::from_ymd_opt(2026, 8, 17).unwrap(),
        )
        .unwrap();

    let widgets = harness.reads.widgets(7).unwrap();
    assert_eq!(
        widgets
            .today
            .iter()
            .map(|work| work.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Build failing", "Multi-day event", "Today event"]
    );
    assert_eq!(widgets.inbox.len(), 1);
    assert_eq!(widgets.attention.len(), 1);
    assert!(widgets.upcoming.iter().any(|item| {
        item.work.title == "Multi-day event"
            && item.date == chrono::NaiveDate::from_ymd_opt(2026, 8, 15).unwrap()
    }));
    assert!(widgets.upcoming.iter().any(|item| {
        item.work.title == "Due Monday"
            && item.date == chrono::NaiveDate::from_ymd_opt(2026, 8, 17).unwrap()
            && item.basis == glancelet_core::application::UpcomingBasis::Planned
    }));
    assert!(widgets.upcoming.iter().any(|item| {
        item.work.title == "Due Monday"
            && item.date == chrono::NaiveDate::from_ymd_opt(2026, 8, 17).unwrap()
            && item.basis == glancelet_core::application::UpcomingBasis::Due
    }));
}

#[tokio::test]
async fn snooze_dismiss_pin_and_planning_drive_widget_membership() {
    let harness = Harness::new();
    harness
        .add(
            "capture",
            CAPTURE_SOURCE_TYPE,
            vec![record("task", "Triage", json!({}))],
        )
        .await;
    let work = harness.store.stored_work().unwrap().remove(0).entry;
    harness.commands.move_to_inbox(&work.id).unwrap();
    assert_eq!(harness.reads.widgets(7).unwrap().inbox.len(), 1);

    harness
        .commands
        .plan(
            &work.id,
            chrono::NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
        )
        .unwrap();
    assert_eq!(harness.reads.widgets(7).unwrap().today.len(), 1);
    harness.commands.pin(&work.id).unwrap();
    assert!(harness.reads.widgets(7).unwrap().today[0].pinned);
    harness
        .commands
        .snooze(
            &work.id,
            Utc.with_ymd_and_hms(2026, 8, 15, 3, 0, 0).single().unwrap(),
        )
        .unwrap();
    assert!(harness.reads.widgets(7).unwrap().today.is_empty());
    harness
        .clock
        .set(Utc.with_ymd_and_hms(2026, 8, 15, 4, 0, 0).single().unwrap());
    assert_eq!(
        harness
            .store
            .stored_work_by_id(&work.id)
            .unwrap()
            .entry
            .planning,
        Some(WorkPlanning::Planned(
            chrono::NaiveDate::from_ymd_opt(2026, 8, 14).unwrap()
        ))
    );
    harness.commands.dismiss(&work.id).unwrap();
    assert!(harness.reads.widgets(7).unwrap().today.is_empty());
}

#[test]
fn widget_layout_seeds_persists_reorders_and_recovers_from_corruption() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("widgets.db");
    {
        let store = Arc::new(SqliteWorkStore::open(&path).unwrap());
        let store_port: Arc<dyn WorkStore> = store;
        let layouts = WidgetLayoutService::new(store_port);
        let defaults = layouts.layout().unwrap();
        assert_eq!(defaults.len(), 3);
        assert_eq!(defaults[0].widget_type, WidgetType::Today);
        layouts
            .save(&[
                WidgetInstance {
                    widget_type: WidgetType::Attention,
                    position: 99,
                    size: WidgetSize::Tall,
                    settings: json!({}),
                },
                WidgetInstance {
                    widget_type: WidgetType::Upcoming,
                    position: 42,
                    size: WidgetSize::Wide,
                    settings: json!({"days": 7}),
                },
            ])
            .unwrap();
    }
    {
        let store = Arc::new(SqliteWorkStore::open(&path).unwrap());
        let store_port: Arc<dyn WorkStore> = store;
        let restored = WidgetLayoutService::new(store_port).layout().unwrap();
        assert_eq!(restored[0].widget_type, WidgetType::Attention);
        assert_eq!(restored[0].position, 0);
        assert_eq!(restored[1].widget_type, WidgetType::Upcoming);
    }
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE widget_instances SET size='\"impossible\"' WHERE position=0",
                [],
            )
            .unwrap();
    }
    let store = Arc::new(SqliteWorkStore::open(&path).unwrap());
    let store_port: Arc<dyn WorkStore> = store;
    let recovered = WidgetLayoutService::new(store_port).layout().unwrap();
    assert_eq!(
        recovered,
        glancelet_core::application::default_widget_layout()
    );
}

#[test]
fn widget_layout_rejects_duplicate_builtin_widgets_and_persists_preferences() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    let store_port: Arc<dyn WorkStore> = store;
    let layouts = WidgetLayoutService::new(store_port);
    let duplicate = WidgetInstance {
        widget_type: WidgetType::Today,
        position: 0,
        size: WidgetSize::Compact,
        settings: json!({}),
    };
    assert!(layouts.save(&[duplicate.clone(), duplicate]).is_err());
    let defaults = layouts.preferences().unwrap();
    assert!(!defaults.always_on_top);
    assert!(defaults.global_shortcut_enabled);
    assert!(!defaults.privacy_mode);
    assert!(layouts.set_always_on_top(true).unwrap().always_on_top);
    assert!(layouts.set_privacy_mode(true).unwrap().privacy_mode);
    assert!(
        layouts
            .set_global_shortcut_enabled(false)
            .unwrap()
            .privacy_mode
    );
    let saved = layouts.preferences().unwrap();
    assert!(saved.always_on_top);
    assert!(!saved.global_shortcut_enabled);
    assert!(saved.privacy_mode);
}
