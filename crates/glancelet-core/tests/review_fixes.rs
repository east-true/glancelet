use std::sync::Arc;

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
