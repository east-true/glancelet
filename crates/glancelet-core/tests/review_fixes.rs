use std::sync::{atomic::AtomicBool, Arc};

use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use glancelet_core::{
    application::{
        Clock, ConnectionCommandService, FixedClock, InMemorySecretStore, SecretStore,
        SourceChangeProcessor, SyncCoordinator, WorkCommandService, WorkStore,
    },
    domain::{
        ProgressAuthority, ProviderId, SourceBatch, SourceBatchKind, SourceChange, SourceEntity,
        SourceIdentity, SourceMutation, SourceRecord, SourceTypeId, WorkBindingMode, WorkDraft,
        WorkKind, WorkLifecycle, WorkProgress,
    },
    extension::{
        Connection, ExtensionRegistry, ProviderRegistration, SourceAdapter, SourceConfig,
        SourceDescriptor, SourceRegistration, WorkProjector,
    },
    sources::{
        fake::{self, CAPTURE_SOURCE_TYPE},
        notion::{
            self, NotionDataSource, NotionPropertyMapping, NotionPropertySchema,
            NotionSourceSettings, NotionTaskProperties,
        },
    },
    storage::SqliteWorkStore,
    GlanceletError, Result,
};
use serde_json::{json, Value};

const REVIEW_PROVIDER: &str = "test.review";
const REVIEW_SOURCE: &str = "test.review.source";

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 12, 6, 0, 0).single().unwrap()
}

struct ReviewAdapter;

#[async_trait]
impl SourceAdapter for ReviewAdapter {
    async fn fetch(
        &self,
        _config: &SourceConfig,
        _checkpoint: Option<Value>,
    ) -> Result<SourceBatch> {
        let record = |external_id: &str, title: &str| {
            SourceMutation::Upsert(SourceRecord {
                identity: SourceIdentity {
                    entity_type: "review".into(),
                    external_id: external_id.into(),
                },
                title: title.into(),
                revision: "1".into(),
                display: json!({}),
                metadata: json!({}),
                navigation: json!({}),
            })
        };
        Ok(SourceBatch {
            kind: SourceBatchKind::FullSnapshot,
            mutations: vec![record("poison", "Poison"), record("valid", "Valid")],
            next_checkpoint: None,
        })
    }
}

struct ReviewProjector;

impl WorkProjector for ReviewProjector {
    fn project(&self, entity: &SourceEntity, _change: &SourceChange) -> Result<WorkDraft> {
        if entity.title == "Poison" {
            return Err(GlanceletError::Source("poison projection".into()));
        }
        Ok(WorkDraft {
            kind: WorkKind::Action,
            title: entity.title.clone(),
            summary: None,
            priority: None,
            progress: Some(WorkProgress::Todo),
            start: None,
            end: None,
            due: None,
            dimensions: json!({}),
            facets: json!({}),
            binding_mode: WorkBindingMode::Capture,
            progress_authority: ProgressAuthority::Local,
        })
    }
}

fn review_registration() -> ProviderRegistration {
    ProviderRegistration {
        provider_id: ProviderId(REVIEW_PROVIDER.into()),
        display_name: "Review".into(),
        sources: vec![SourceRegistration {
            descriptor: SourceDescriptor {
                source_type_id: SourceTypeId(REVIEW_SOURCE.into()),
                display_name: "Review source".into(),
                description: "Exercises projection isolation".into(),
            },
            adapter: Arc::new(ReviewAdapter),
            projector: Arc::new(ReviewProjector),
        }],
    }
}

fn put_source(store: &SqliteWorkStore, provider: &str, source_type: &str, settings: Value) {
    store
        .put_connection(&Connection {
            id: "connection".into(),
            provider_id: ProviderId(provider.into()),
            display_name: "Account".into(),
            config: json!({ "status": "connected" }),
        })
        .unwrap();
    store
        .put_source_config(&SourceConfig {
            id: "source".into(),
            connection_id: "connection".into(),
            source_type_id: SourceTypeId(source_type.into()),
            display_name: "Source".into(),
            enabled: true,
            removed_at: None,
            expected_sync_interval_seconds: 60,
            settings,
        })
        .unwrap();
}

#[tokio::test]
async fn projection_failure_is_deferred_while_later_changes_are_processed() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    put_source(&store, REVIEW_PROVIDER, REVIEW_SOURCE, json!({}));
    let mut registry = ExtensionRegistry::new();
    registry.register(review_registration()).unwrap();
    let registry = Arc::new(registry);
    let clock = Arc::new(FixedClock::new(now()));
    let store_port: Arc<dyn WorkStore> = store.clone();
    let clock_port: Arc<dyn Clock> = clock.clone();
    let sync = SyncCoordinator::new(
        Arc::clone(&store_port),
        Arc::clone(&registry),
        Arc::clone(&clock_port),
    );
    let changes = SourceChangeProcessor::new(store_port, registry, clock_port);

    assert_eq!(sync.sync("source").await.unwrap(), 2);
    let error = changes.process_pending(10).unwrap_err();
    assert!(error.to_string().contains("poison projection"));
    let stored = store.stored_work().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].entry.title, "Valid");
    assert!(store
        .pending_source_changes_at(10, clock.now())
        .unwrap()
        .is_empty());

    clock.set(clock.now() + Duration::minutes(5) + Duration::seconds(1));
    let retry = store.pending_source_changes_at(10, clock.now()).unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].source_entity.title, "Poison");
}

#[tokio::test]
async fn core_rejects_stale_local_progress_transitions() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    put_source(
        &store,
        "dev.glancelet.fake",
        CAPTURE_SOURCE_TYPE,
        json!({
            "records": [{
                "identity": { "entity_type": "capture", "external_id": "one" },
                "title": "Captured",
                "revision": "1",
                "display": {},
                "metadata": { "kind": "action" },
                "navigation": {}
            }]
        }),
    );
    let mut registry = ExtensionRegistry::new();
    registry.register(fake::registration()).unwrap();
    let registry = Arc::new(registry);
    let clock = Arc::new(FixedClock::new(now()));
    let store_port: Arc<dyn WorkStore> = store.clone();
    let clock_port: Arc<dyn Clock> = clock.clone();
    let sync = SyncCoordinator::new(
        Arc::clone(&store_port),
        Arc::clone(&registry),
        Arc::clone(&clock_port),
    );
    let changes =
        SourceChangeProcessor::new(Arc::clone(&store_port), registry, Arc::clone(&clock_port));
    let commands = WorkCommandService::new(store_port, clock_port);

    sync.sync("source").await.unwrap();
    changes.process_pending(10).unwrap();
    let id = store.stored_work().unwrap()[0].entry.id.clone();
    commands.start_work(&id).unwrap();
    assert!(commands.start_work(&id).is_err());
    commands.complete(&id).unwrap();
    assert!(commands.start_work(&id).is_err());
    let work = store.stored_work_by_id(&id).unwrap();
    assert_eq!(work.entry.progress, Some(WorkProgress::Done));
    assert_eq!(work.entry.lifecycle, WorkLifecycle::Resolved);
}

#[test]
fn notion_assigned_to_me_requires_an_assignee_mapping() {
    let schema = NotionDataSource {
        id: "data-source".into(),
        title: "Tasks".into(),
        properties: vec![NotionPropertySchema {
            id: "title".into(),
            name: "Task".into(),
            kind: "title".into(),
            status: None,
        }],
    };
    let settings = NotionSourceSettings {
        data_source_id: schema.id.clone(),
        data_source_name: schema.title.clone(),
        properties: NotionTaskProperties {
            title: NotionPropertyMapping {
                id: "title".into(),
                name: "Task".into(),
            },
            assignee: None,
            status: None,
            due: None,
        },
        only_assigned_to_me: true,
        active_status_ids: vec![],
    };
    assert!(notion::validate_settings(&schema, &settings).is_err());
}

struct FailingDeleteSecretStore;

impl SecretStore for FailingDeleteSecretStore {
    fn get(&self, _key: &str) -> Result<Option<String>> {
        Ok(Some("still-present".into()))
    }

    fn set(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }

    fn delete(&self, _key: &str) -> Result<()> {
        Err(GlanceletError::SecretStoreUnavailable(
            "injected delete failure".into(),
        ))
    }
}

#[test]
fn disconnect_is_atomic_in_sqlite_and_safe_when_secret_cleanup_fails() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    put_source(&store, "provider", "source.type", json!({}));
    let store_port: Arc<dyn WorkStore> = store.clone();
    let failing: Arc<dyn SecretStore> = Arc::new(FailingDeleteSecretStore);
    let service = ConnectionCommandService::new(Arc::clone(&store_port), failing);

    let error = service
        .disconnect(
            "connection",
            &ProviderId("provider".into()),
            "provider:connection",
        )
        .unwrap_err();
    assert!(error.to_string().contains("injected delete failure"));
    assert_eq!(
        store.connections().unwrap()[0].config["status"],
        "disconnected"
    );
    assert!(!store.source_config("source").unwrap().enabled);

    let secrets = Arc::new(InMemorySecretStore::new());
    secrets.set("provider:other", "secret").unwrap();
    let secrets_port: Arc<dyn SecretStore> = secrets.clone();
    let service = ConnectionCommandService::new(store_port, secrets_port);
    assert!(service
        .disconnect(
            "connection",
            &ProviderId("other-provider".into()),
            "provider:other",
        )
        .is_err());
    assert_eq!(
        secrets.get("provider:other").unwrap().as_deref(),
        Some("secret")
    );
}

struct VersionedProjector {
    fail_old: Arc<AtomicBool>,
    version: i32,
    binding_mode: WorkBindingMode,
    authority: ProgressAuthority,
}

impl WorkProjector for VersionedProjector {
    fn version(&self) -> i32 {
        self.version
    }

    fn project(&self, entity: &SourceEntity, _change: &SourceChange) -> Result<WorkDraft> {
        if self.fail_old.load(std::sync::atomic::Ordering::SeqCst) && entity.title == "Old" {
            return Err(GlanceletError::Source("old projection failed".into()));
        }
        Ok(WorkDraft {
            kind: WorkKind::Action,
            title: format!("{} v{}", entity.title, self.version),
            summary: None,
            priority: None,
            progress: Some(WorkProgress::Todo),
            start: None,
            end: None,
            due: None,
            dimensions: json!({}),
            facets: json!({}),
            binding_mode: self.binding_mode,
            progress_authority: self.authority,
        })
    }
}

fn versioned_registration(
    fail_old: Arc<AtomicBool>,
    version: i32,
    binding_mode: WorkBindingMode,
    authority: ProgressAuthority,
) -> ProviderRegistration {
    ProviderRegistration {
        provider_id: ProviderId("versioned".into()),
        display_name: "Versioned".into(),
        sources: vec![SourceRegistration {
            descriptor: SourceDescriptor {
                source_type_id: SourceTypeId("versioned.source".into()),
                display_name: "Versioned source".into(),
                description: "Causal projection regression".into(),
            },
            adapter: Arc::new(ReviewAdapter),
            projector: Arc::new(VersionedProjector {
                fail_old,
                version,
                binding_mode,
                authority,
            }),
        }],
    }
}

fn apply_records(store: &SqliteWorkStore, records: Vec<SourceRecord>, now: chrono::DateTime<Utc>) {
    let (config, runtime) = store.source_sync_state("source").unwrap();
    store
        .apply_source_batch(
            &config,
            runtime.config_revision,
            &SourceBatch {
                kind: SourceBatchKind::Delta,
                mutations: records.into_iter().map(SourceMutation::Upsert).collect(),
                next_checkpoint: None,
            },
            now,
        )
        .unwrap();
}

fn versioned_record(title: &str, revision: &str) -> SourceRecord {
    SourceRecord {
        identity: SourceIdentity {
            entity_type: "versioned".into(),
            external_id: "same".into(),
        },
        title: title.into(),
        revision: revision.into(),
        display: json!({}),
        metadata: json!({}),
        navigation: json!({}),
    }
}

#[test]
fn deferred_projection_preserves_order_within_the_same_entity() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    put_source(&store, "versioned", "versioned.source", json!({}));
    let fail_old = Arc::new(AtomicBool::new(true));
    let mut registry = ExtensionRegistry::new();
    registry
        .register(versioned_registration(
            Arc::clone(&fail_old),
            1,
            WorkBindingMode::Mirror,
            ProgressAuthority::External,
        ))
        .unwrap();
    let registry = Arc::new(registry);
    let clock = Arc::new(FixedClock::new(now()));

    apply_records(&store, vec![versioned_record("Old", "1")], clock.now());
    apply_records(&store, vec![versioned_record("New", "2")], clock.now());
    let store_port: Arc<dyn WorkStore> = store.clone();
    let clock_port: Arc<dyn Clock> = clock.clone();
    let processor = SourceChangeProcessor::new(store_port, registry, clock_port);

    assert!(processor.process_pending(10).is_err());
    assert!(store.stored_work().unwrap().is_empty());
    assert!(store
        .pending_source_changes_at(10, clock.now())
        .unwrap()
        .is_empty());

    fail_old.store(false, std::sync::atomic::Ordering::SeqCst);
    clock.set(clock.now() + Duration::minutes(5) + Duration::seconds(1));
    let report = processor.drain_pending(10, 100).unwrap();
    assert_eq!(report.processed, 2);
    assert!(report.failures.is_empty());
    assert_eq!(store.stored_work().unwrap()[0].entry.title, "New v1");
}

#[test]
fn projector_version_change_reprojects_existing_work_and_updates_binding_metadata() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    put_source(&store, "versioned", "versioned.source", json!({}));
    let never_fail = Arc::new(AtomicBool::new(false));
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    apply_records(&store, vec![versioned_record("Task", "1")], clock.now());

    let mut v1 = ExtensionRegistry::new();
    v1.register(versioned_registration(
        Arc::clone(&never_fail),
        1,
        WorkBindingMode::Mirror,
        ProgressAuthority::External,
    ))
    .unwrap();
    let store_port: Arc<dyn WorkStore> = store.clone();
    SourceChangeProcessor::new(Arc::clone(&store_port), Arc::new(v1), Arc::clone(&clock))
        .process_pending(10)
        .unwrap();
    assert_eq!(store.stored_work().unwrap()[0].binding.projector_version, 1);

    let mut v2 = ExtensionRegistry::new();
    v2.register(versioned_registration(
        never_fail,
        2,
        WorkBindingMode::Capture,
        ProgressAuthority::Local,
    ))
    .unwrap();
    SourceChangeProcessor::new(store_port, Arc::new(v2), clock)
        .process_pending(10)
        .unwrap();
    let projected = store.stored_work().unwrap().into_iter().next().unwrap();
    assert_eq!(projected.entry.title, "Task v2");
    assert_eq!(projected.binding.projector_version, 2);
    assert_eq!(projected.binding.mode, WorkBindingMode::Capture);
    assert_eq!(
        projected.binding.progress_authority,
        ProgressAuthority::Local
    );
}

struct BlockingAdapter {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl SourceAdapter for BlockingAdapter {
    async fn fetch(
        &self,
        _config: &SourceConfig,
        _checkpoint: Option<Value>,
    ) -> Result<SourceBatch> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(SourceBatch {
            kind: SourceBatchKind::FullSnapshot,
            mutations: vec![SourceMutation::Upsert(SourceRecord {
                identity: SourceIdentity {
                    entity_type: "blocking".into(),
                    external_id: "one".into(),
                },
                title: "Stale result".into(),
                revision: "1".into(),
                display: json!({}),
                metadata: json!({}),
                navigation: json!({}),
            })],
            next_checkpoint: None,
        })
    }
}

#[tokio::test]
async fn reconfiguration_invalidates_an_in_flight_sync_batch() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    put_source(&store, "blocking", "blocking.source", json!({"value": 1}));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut registry = ExtensionRegistry::new();
    registry
        .register(ProviderRegistration {
            provider_id: ProviderId("blocking".into()),
            display_name: "Blocking".into(),
            sources: vec![SourceRegistration {
                descriptor: SourceDescriptor {
                    source_type_id: SourceTypeId("blocking.source".into()),
                    display_name: "Blocking".into(),
                    description: "Stale sync regression".into(),
                },
                adapter: Arc::new(BlockingAdapter {
                    started: Arc::clone(&started),
                    release: Arc::clone(&release),
                }),
                projector: Arc::new(ReviewProjector),
            }],
        })
        .unwrap();
    let store_port: Arc<dyn WorkStore> = store.clone();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let sync = Arc::new(SyncCoordinator::new(store_port, Arc::new(registry), clock));
    let running = {
        let sync = Arc::clone(&sync);
        tokio::spawn(async move { sync.sync("source").await })
    };
    started.notified().await;
    let mut config = store.source_config("source").unwrap();
    config.settings = json!({"value": 2});
    store.put_source_config(&config).unwrap();
    release.notify_one();

    assert!(matches!(
        running.await.unwrap(),
        Err(GlanceletError::InvalidOperation(_))
    ));
    assert!(store.stored_work().unwrap().is_empty());
    assert!(store.pending_source_changes(10).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_progress_transition_is_atomic_under_start_complete_races() {
    for iteration in 0..16 {
        let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
        put_source(
            &store,
            "dev.glancelet.fake",
            CAPTURE_SOURCE_TYPE,
            json!({
                "records": [{
                    "identity": {
                        "entity_type": "capture",
                        "external_id": format!("race-{iteration}")
                    },
                    "title": "Race",
                    "revision": "1",
                    "display": {},
                    "metadata": { "kind": "action" },
                    "navigation": {}
                }]
            }),
        );
        let mut registry = ExtensionRegistry::new();
        registry.register(fake::registration()).unwrap();
        let registry = Arc::new(registry);
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
        let store_port: Arc<dyn WorkStore> = store.clone();
        let sync = SyncCoordinator::new(
            Arc::clone(&store_port),
            Arc::clone(&registry),
            Arc::clone(&clock),
        );
        sync.sync("source").await.unwrap();
        SourceChangeProcessor::new(Arc::clone(&store_port), registry, Arc::clone(&clock))
            .process_pending(10)
            .unwrap();
        let id = store.stored_work().unwrap()[0].entry.id.clone();
        let barrier = Arc::new(std::sync::Barrier::new(3));

        let start = {
            let store = Arc::clone(&store_port);
            let clock = Arc::clone(&clock);
            let id = id.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let commands = WorkCommandService::new(store, clock);
                barrier.wait();
                commands.start_work(&id)
            })
        };
        let complete = {
            let store = Arc::clone(&store_port);
            let clock = Arc::clone(&clock);
            let id = id.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let commands = WorkCommandService::new(store, clock);
                barrier.wait();
                commands.complete(&id)
            })
        };
        barrier.wait();
        let _ = start.join().unwrap();
        let _ = complete.join().unwrap();

        let work = store.stored_work_by_id(&id).unwrap();
        assert_eq!(work.entry.progress, Some(WorkProgress::Done));
        assert_eq!(work.entry.lifecycle, WorkLifecycle::Resolved);
    }
}

#[test]
fn projection_drain_processes_more_than_one_batch() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    put_source(&store, REVIEW_PROVIDER, REVIEW_SOURCE, json!({}));
    let mut registry = ExtensionRegistry::new();
    registry.register(review_registration()).unwrap();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let records = (0..501)
        .map(|index| SourceRecord {
            identity: SourceIdentity {
                entity_type: "bulk".into(),
                external_id: index.to_string(),
            },
            title: format!("Valid {index}"),
            revision: "1".into(),
            display: json!({}),
            metadata: json!({}),
            navigation: json!({}),
        })
        .collect();
    apply_records(&store, records, clock.now());
    let store_port: Arc<dyn WorkStore> = store.clone();
    let report = SourceChangeProcessor::new(store_port, Arc::new(registry), clock)
        .drain_pending(500, 1_000)
        .unwrap();
    assert_eq!(report.processed, 501);
    assert!(!report.reached_limit);
    assert!(report.failures.is_empty());
    assert_eq!(store.stored_work().unwrap().len(), 501);
}

#[test]
fn connection_and_source_batch_writes_roll_back_as_a_unit() {
    let store = SqliteWorkStore::in_memory().unwrap();
    let connection = Connection {
        id: "new-connection".into(),
        provider_id: ProviderId("provider".into()),
        display_name: "Account".into(),
        config: json!({"status": "connected"}),
    };
    let valid = SourceConfig {
        id: "first".into(),
        connection_id: connection.id.clone(),
        source_type_id: SourceTypeId("source.type".into()),
        display_name: "First".into(),
        enabled: true,
        removed_at: None,
        expected_sync_interval_seconds: 60,
        settings: json!({}),
    };
    let mut mismatched = valid.clone();
    mismatched.id = "second".into();
    mismatched.connection_id = "different-connection".into();
    assert!(store
        .connect_connection(&connection, &[valid.clone(), mismatched])
        .is_err());
    assert!(store.connections().unwrap().is_empty());
    assert!(store.source_config(&valid.id).is_err());

    store.put_connection(&connection).unwrap();
    let mut invalid_fk = valid.clone();
    invalid_fk.id = "third".into();
    invalid_fk.connection_id = "missing".into();
    assert!(store
        .put_source_configs(&[valid.clone(), invalid_fk])
        .is_err());
    assert!(store.source_config(&valid.id).is_err());
}

#[tokio::test]
async fn dashboard_query_excludes_resolved_dismissed_and_future_snoozed_history() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    put_source(
        &store,
        "dev.glancelet.fake",
        CAPTURE_SOURCE_TYPE,
        json!({
            "records": [
                {
                    "identity": {"entity_type": "capture", "external_id": "snooze"},
                    "title": "Snooze",
                    "revision": "1",
                    "display": {},
                    "metadata": {"kind": "action"},
                    "navigation": {}
                },
                {
                    "identity": {"entity_type": "capture", "external_id": "complete"},
                    "title": "Complete",
                    "revision": "1",
                    "display": {},
                    "metadata": {"kind": "action"},
                    "navigation": {}
                },
                {
                    "identity": {"entity_type": "capture", "external_id": "dismiss"},
                    "title": "Dismiss",
                    "revision": "1",
                    "display": {},
                    "metadata": {"kind": "action"},
                    "navigation": {}
                }
            ]
        }),
    );
    let mut registry = ExtensionRegistry::new();
    registry.register(fake::registration()).unwrap();
    let registry = Arc::new(registry);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let store_port: Arc<dyn WorkStore> = store.clone();
    SyncCoordinator::new(
        Arc::clone(&store_port),
        Arc::clone(&registry),
        Arc::clone(&clock),
    )
    .sync("source")
    .await
    .unwrap();
    SourceChangeProcessor::new(Arc::clone(&store_port), registry, Arc::clone(&clock))
        .process_pending(10)
        .unwrap();
    let by_title = store
        .stored_work()
        .unwrap()
        .into_iter()
        .map(|work| (work.entry.title.clone(), work.entry.id))
        .collect::<std::collections::HashMap<_, _>>();
    let commands = WorkCommandService::new(store_port, Arc::clone(&clock));
    commands
        .snooze(&by_title["Snooze"], clock.now() + Duration::hours(1))
        .unwrap();
    commands.complete(&by_title["Complete"]).unwrap();
    commands.dismiss(&by_title["Dismiss"]).unwrap();

    assert_eq!(store.stored_work().unwrap().len(), 3);
    assert!(store.dashboard_work(clock.now()).unwrap().is_empty());
}

#[test]
fn provider_registration_is_atomic() {
    let mut registry = ExtensionRegistry::new();
    registry.register(review_registration()).unwrap();
    let duplicate = ProviderRegistration {
        provider_id: ProviderId("duplicate".into()),
        display_name: "Duplicate".into(),
        sources: vec![
            SourceRegistration {
                descriptor: SourceDescriptor {
                    source_type_id: SourceTypeId("would.have.been.inserted".into()),
                    display_name: "First".into(),
                    description: "First".into(),
                },
                adapter: Arc::new(ReviewAdapter),
                projector: Arc::new(ReviewProjector),
            },
            SourceRegistration {
                descriptor: SourceDescriptor {
                    source_type_id: SourceTypeId(REVIEW_SOURCE.into()),
                    display_name: "Duplicate".into(),
                    description: "Duplicate".into(),
                },
                adapter: Arc::new(ReviewAdapter),
                projector: Arc::new(ReviewProjector),
            },
        ],
    };
    assert!(registry.register(duplicate).is_err());
    assert!(registry
        .adapter(&SourceTypeId("would.have.been.inserted".into()))
        .is_err());
}
