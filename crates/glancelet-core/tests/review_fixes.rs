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
            title: entity.title.clone(),
            summary: None,
            priority: None,
            progress: Some(WorkProgress::Todo),
            start: None,
            end: None,
            due: None,
            dimensions: json!({}),
            facets: json!({}),
            binding_mode: WorkBindingMode::Mirror,
            progress_authority: ProgressAuthority::External,
        })
    }
}

fn versioned_registration(fail_old: Arc<AtomicBool>, version: i32) -> ProviderRegistration {
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
            projector: Arc::new(VersionedProjector { fail_old, version }),
        }],
    }
}

#[test]
fn deferred_projection_preserves_order_within_the_same_entity() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    put_source(&store, "versioned", "versioned.source", json!({}));
    let fail_old = Arc::new(AtomicBool::new(true));
    let mut registry = ExtensionRegistry::new();
    registry
        .register(versioned_registration(Arc::clone(&fail_old), 1))
        .unwrap();
    let registry = Arc::new(registry);
    let clock = Arc::new(FixedClock::new(now()));
    let state = store.source_sync_state("source").unwrap();
    for (title, revision) in [("Old", "1"), ("New", "2")] {
        store
            .apply_source_batch(
                &state.0,
                state.1.config_revision,
                &SourceBatch {
                    kind: SourceBatchKind::Delta,
                    mutations: vec![SourceMutation::Upsert(SourceRecord {
                        identity: SourceIdentity {
                            entity_type: "versioned".into(),
                            external_id: "same".into(),
                        },
                        title: title.into(),
                        revision: revision.into(),
                        display: json!({}),
                        metadata: json!({}),
                        navigation: json!({}),
                    })],
                    next_checkpoint: None,
                },
                clock.now(),
            )
            .unwrap();
    }
    let processor = SourceChangeProcessor::new(store.clone(), registry, clock.clone());
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
    assert_eq!(store.stored_work().unwrap()[0].entry.title, "New");
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
    put_source(&store, "blocking", "blocking.source", json!({"value":1}));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let registration = ProviderRegistration {
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
    };
    let mut registry = ExtensionRegistry::new();
    registry.register(registration).unwrap();
    let store_port: Arc<dyn WorkStore> = store.clone();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let sync = Arc::new(SyncCoordinator::new(store_port, Arc::new(registry), clock));
    let running = {
        let sync = Arc::clone(&sync);
        tokio::spawn(async move { sync.sync("source").await })
    };
    started.notified().await;
    let mut config = store.source_config("source").unwrap();
    config.settings = json!({"value":2});
    store.put_source_config(&config).unwrap();
    release.notify_one();
    assert!(matches!(
        running.await.unwrap(),
        Err(GlanceletError::InvalidOperation(_))
    ));
    assert!(store.stored_work().unwrap().is_empty());
    assert!(store.pending_source_changes(10).unwrap().is_empty());
}

#[tokio::test]
async fn reconnect_invalidates_an_in_flight_sync_without_an_auth_failure() {
    let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
    put_source(&store, "blocking", "blocking.source", json!({"value":1}));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let registration = ProviderRegistration {
        provider_id: ProviderId("blocking".into()),
        display_name: "Blocking".into(),
        sources: vec![SourceRegistration {
            descriptor: SourceDescriptor {
                source_type_id: SourceTypeId("blocking.source".into()),
                display_name: "Blocking".into(),
                description: "Credential generation regression".into(),
            },
            adapter: Arc::new(BlockingAdapter {
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            }),
            projector: Arc::new(ReviewProjector),
        }],
    };
    let mut registry = ExtensionRegistry::new();
    registry.register(registration).unwrap();
    let store_port: Arc<dyn WorkStore> = store.clone();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let sync = Arc::new(SyncCoordinator::new(store_port, Arc::new(registry), clock));
    let previous_revision = store.source_runtime("source").unwrap().config_revision;
    let running = {
        let sync = Arc::clone(&sync);
        tokio::spawn(async move { sync.sync("source").await })
    };
    started.notified().await;
    let connection = store.connections().unwrap().remove(0);
    store.connect_connection(&connection, &[]).unwrap();
    assert!(store.source_runtime("source").unwrap().config_revision > previous_revision);
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
    for _ in 0..32 {
        let store = Arc::new(SqliteWorkStore::in_memory().unwrap());
        put_source(
            &store,
            "dev.glancelet.fake",
            CAPTURE_SOURCE_TYPE,
            json!({
                "records": [{
                    "identity": { "entity_type": "capture", "external_id": "race" },
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
        let start_store = Arc::clone(&store_port);
        let start_clock = Arc::clone(&clock);
        let start_id = id.clone();
        let start_barrier = Arc::clone(&barrier);
        let start = std::thread::spawn(move || {
            let commands = WorkCommandService::new(start_store, start_clock);
            start_barrier.wait();
            commands.start_work(&start_id)
        });
        let complete_store = Arc::clone(&store_port);
        let complete_clock = Arc::clone(&clock);
        let complete_id = id.clone();
        let complete_barrier = Arc::clone(&barrier);
        let complete = std::thread::spawn(move || {
            let commands = WorkCommandService::new(complete_store, complete_clock);
            complete_barrier.wait();
            commands.complete(&complete_id)
        });
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
    let registry = Arc::new(registry);
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(now()));
    let state = store.source_sync_state("source").unwrap();
    let mutations = (0..501)
        .map(|index| {
            SourceMutation::Upsert(SourceRecord {
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
        })
        .collect();
    store
        .apply_source_batch(
            &state.0,
            state.1.config_revision,
            &SourceBatch {
                kind: SourceBatchKind::FullSnapshot,
                mutations,
                next_checkpoint: None,
            },
            clock.now(),
        )
        .unwrap();
    let processor = SourceChangeProcessor::new(store.clone(), registry, clock);
    let report = processor.drain_pending(500, 1_000).unwrap();
    assert_eq!(report.processed, 501);
    assert!(report.failures.is_empty());
    assert_eq!(store.stored_work().unwrap().len(), 501);
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
