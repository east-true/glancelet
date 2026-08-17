use std::sync::Arc;

use chrono::{NaiveDate, TimeZone, Utc};
use glancelet_core::{
    application::{
        Clock, FixedClock, SourceChangeProcessor, SourceFailureKind, SyncCoordinator, TimeContext,
        WidgetLayoutService, WorkAction, WorkCommandService, WorkReadService, WorkStore,
    },
    domain::{ProviderId, SourceTypeId, WorkLifecycle, WorkPlanning, WorkProgress},
    extension::{Connection, ExtensionRegistry, SourceConfig},
    sources::{
        fake::{self, MIRROR_SOURCE_TYPE},
        local::{self, SOURCE_CONFIG_ID},
    },
    storage::SqliteWorkStore,
};
use serde_json::json;
use tempfile::TempDir;

struct Harness {
    store: Arc<SqliteWorkStore>,
    clock: Arc<FixedClock>,
    changes: SourceChangeProcessor,
    reads: WorkReadService,
    commands: WorkCommandService,
    sync: SyncCoordinator,
}

impl Harness {
    fn memory() -> Self {
        Self::with_store(Arc::new(SqliteWorkStore::in_memory().unwrap()))
    }

    fn with_store(store: Arc<SqliteWorkStore>) -> Self {
        let mut registry = ExtensionRegistry::new();
        registry.register(fake::registration()).unwrap();
        registry.register(local::registration()).unwrap();
        let registry = Arc::new(registry);
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 8, 15, 1, 0, 0).single().unwrap(),
        ));
        let store_port: Arc<dyn WorkStore> = store.clone();
        let clock_port: Arc<dyn Clock> = clock.clone();
        Self {
            changes: SourceChangeProcessor::new(
                Arc::clone(&store_port),
                Arc::clone(&registry),
                Arc::clone(&clock_port),
            ),
            reads: WorkReadService::new(
                Arc::clone(&store_port),
                Arc::clone(&registry),
                Arc::clone(&clock_port),
                TimeContext::named("Asia/Seoul").unwrap(),
            ),
            commands: WorkCommandService::new(Arc::clone(&store_port), Arc::clone(&clock_port)),
            sync: SyncCoordinator::new(store_port, registry, clock_port),
            store,
            clock,
        }
    }

    fn capture(&self, request_id: &str, title: &str) -> String {
        let identity =
            local::ingest(self.store.as_ref(), request_id, title, self.clock.now()).unwrap();
        self.changes.process_pending(100).unwrap();
        self.store
            .work_id_for_source_identity(SOURCE_CONFIG_ID, &identity)
            .unwrap()
            .unwrap()
    }
}

#[test]
fn manual_capture_uses_existing_capture_semantics_and_is_idempotent() {
    let harness = Harness::memory();
    let request_id = "64b2f871-16da-4aca-84f3-a9272fa26fc5";
    let work_id = harness.capture(request_id, "  Review deployment issue  ");
    let duplicate_id = harness.capture(request_id, "Review deployment issue");
    assert_eq!(work_id, duplicate_id);

    let work = harness.store.stored_work_by_id(&work_id).unwrap();
    assert_eq!(work.entry.title, "Review deployment issue");
    assert_eq!(work.entry.progress, Some(WorkProgress::Todo));
    assert_eq!(work.entry.planning, Some(WorkPlanning::Inbox));
    assert_eq!(
        work.binding.mode,
        glancelet_core::domain::WorkBindingMode::Capture
    );
    assert_eq!(
        work.binding.progress_authority,
        glancelet_core::domain::ProgressAuthority::Local
    );
    let view = harness.reads.widgets(7).unwrap().inbox.remove(0);
    assert!(!view.can_navigate);
    assert!(!view.available_actions.contains(&WorkAction::OpenSource));
    assert!(view.available_actions.contains(&WorkAction::Complete));
    assert_eq!(harness.store.stored_work().unwrap().len(), 1);
}

#[test]
fn manual_capture_supports_existing_planning_and_progress_lifecycle() {
    let harness = Harness::memory();
    let today = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
    let tomorrow = NaiveDate::from_ymd_opt(2026, 8, 16).unwrap();

    let today_id = harness.capture("43b045b3-b5de-4579-81f4-b0da7348669f", "Today");
    harness.commands.plan(&today_id, today).unwrap();
    assert_eq!(harness.reads.widgets(7).unwrap().today.len(), 1);

    let tomorrow_id = harness.capture("bcbf1d62-63cf-4817-9012-273fe4ac91bf", "Tomorrow");
    harness.commands.plan(&tomorrow_id, tomorrow).unwrap();
    assert!(harness
        .reads
        .widgets(7)
        .unwrap()
        .upcoming
        .iter()
        .any(|item| item.work.id == tomorrow_id));

    let backlog_id = harness.capture("3b23593c-b0af-4328-a9fa-e3d5da7cc878", "Backlog");
    harness.commands.move_to_backlog(&backlog_id).unwrap();
    assert_eq!(
        harness
            .store
            .stored_work_by_id(&backlog_id)
            .unwrap()
            .entry
            .planning,
        Some(WorkPlanning::Backlog)
    );

    harness.commands.start_work(&today_id).unwrap();
    harness.commands.complete(&today_id).unwrap();
    let completed = harness.store.stored_work_by_id(&today_id).unwrap().entry;
    assert_eq!(completed.progress, Some(WorkProgress::Done));
    assert_eq!(completed.lifecycle, WorkLifecycle::Resolved);
}

#[test]
fn manual_capture_rejects_invalid_input_without_persisting_content() {
    let harness = Harness::memory();
    assert!(local::ingest(
        harness.store.as_ref(),
        "not-a-uuid",
        "title",
        harness.clock.now()
    )
    .is_err());
    assert!(local::ingest(
        harness.store.as_ref(),
        "66903b66-9e2f-47e2-992f-c6f9be06ab21",
        "   ",
        harness.clock.now()
    )
    .is_err());
    assert!(local::ingest(
        harness.store.as_ref(),
        "c61f62bf-2633-4833-88c9-cb479cdb9e64",
        &"x".repeat(local::MAX_TITLE_LENGTH + 1),
        harness.clock.now()
    )
    .is_err());
    assert!(harness.store.source_configs().unwrap().is_empty());
}

#[tokio::test]
async fn privacy_redacts_sensitive_fields_before_serialization() {
    let harness = Harness::memory();
    harness
        .store
        .put_connection(&Connection {
            id: "sensitive-connection".into(),
            provider_id: ProviderId("dev.glancelet.fake".into()),
            display_name: "Sensitive account".into(),
            config: json!({}),
        })
        .unwrap();
    harness
        .store
        .put_source_config(&SourceConfig {
            id: "sensitive-source".into(),
            connection_id: "sensitive-connection".into(),
            source_type_id: SourceTypeId(MIRROR_SOURCE_TYPE.into()),
            display_name: "Secret calendar".into(),
            enabled: true,
            removed_at: None,
            expected_sync_interval_seconds: 300,
            settings: json!({
                "records": [{
                    "identity": {"entity_type":"event","external_id":"sensitive"},
                    "title": "Acquisition meeting",
                    "revision": "1",
                    "display": {},
                    "metadata": {
                        "kind": "event",
                        "summary": "Project Falcon",
                        "start": {"type":"date","date":"2026-08-15"},
                        "dimensions": {"customer":"Acme"},
                        "facets": {"label":"Board only"}
                    },
                    "navigation": {"web_url":"https://example.com/private-meeting"}
                }]
            }),
        })
        .unwrap();
    harness.sync.sync("sensitive-source").await.unwrap();
    harness.changes.process_pending(100).unwrap();
    let manual_id = harness.capture(
        "79116ed2-362c-4a8f-b0d5-455dfc03e8c2",
        "Sensitive local task",
    );
    harness
        .commands
        .plan(&manual_id, NaiveDate::from_ymd_opt(2026, 8, 15).unwrap())
        .unwrap();
    let runtime = harness.store.source_runtime("sensitive-source").unwrap();
    harness
        .store
        .record_sync_failure(
            "sensitive-source",
            runtime.config_revision,
            harness.clock.now(),
            None,
            SourceFailureKind::AuthenticationRequired,
            "sensitive provider failure",
        )
        .unwrap();

    let normal = serde_json::to_string(&harness.reads.widgets(7).unwrap()).unwrap();
    assert!(normal.contains("Acquisition meeting"));
    assert!(normal.contains("Project Falcon"));
    assert!(normal.contains("Acme"));
    assert!(normal.contains("Board only"));
    assert!(normal.contains("Secret calendar"));
    assert!(normal.contains("Sensitive local task"));

    let private = harness.reads.widgets_with_privacy(7, true).unwrap();
    let serialized = serde_json::to_string(&private).unwrap();
    for sensitive in [
        "Acquisition meeting",
        "Project Falcon",
        "Acme",
        "Board only",
        "Secret calendar",
        "sensitive-source",
        "Sensitive local task",
    ] {
        assert!(!serialized.contains(sensitive));
    }
    let event = private
        .today
        .iter()
        .find(|work| work.kind == glancelet_core::domain::WorkKind::Event)
        .unwrap();
    assert_eq!(event.title, "Private event");
    assert!(event.start.is_some());
    assert!(event.can_navigate);
    assert!(event.available_actions.contains(&WorkAction::OpenSource));
    let action = private
        .today
        .iter()
        .find(|work| work.kind == glancelet_core::domain::WorkKind::Action)
        .unwrap();
    assert_eq!(
        action.planning,
        Some(WorkPlanning::Planned(
            NaiveDate::from_ymd_opt(2026, 8, 15).unwrap()
        ))
    );
    assert!(action.available_actions.contains(&WorkAction::Complete));
}

#[test]
fn captures_and_daily_use_preferences_survive_restart() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("daily-use.db");
    {
        let harness = Harness::with_store(Arc::new(SqliteWorkStore::open(&path).unwrap()));
        harness.capture("d7d95823-57db-425a-8218-e8b70fe67151", "Persist me");
        let layouts = WidgetLayoutService::new(harness.store.clone());
        layouts.set_privacy_mode(true).unwrap();
        layouts.set_global_shortcut_enabled(false).unwrap();
    }
    let harness = Harness::with_store(Arc::new(SqliteWorkStore::open(&path).unwrap()));
    harness.capture("a20f3804-2820-4f36-82dc-08f17d8ec5a5", "Persist another");
    let preferences = WidgetLayoutService::new(harness.store.clone())
        .preferences()
        .unwrap();
    assert!(preferences.privacy_mode);
    assert!(!preferences.global_shortcut_enabled);
    assert_eq!(harness.reads.widgets(7).unwrap().inbox.len(), 2);
    assert!(harness
        .reads
        .widgets_with_privacy(7, preferences.privacy_mode)
        .unwrap()
        .inbox
        .iter()
        .all(|work| work.title == "Private work item"));
    assert_eq!(
        harness
            .store
            .source_configs()
            .unwrap()
            .iter()
            .filter(|source| source.id == SOURCE_CONFIG_ID)
            .count(),
        1
    );
    assert_eq!(
        harness
            .store
            .connections()
            .unwrap()
            .iter()
            .filter(|connection| connection.id == local::CONNECTION_ID)
            .count(),
        1
    );
    assert_eq!(harness.store.schema_version().unwrap(), 8);
}
