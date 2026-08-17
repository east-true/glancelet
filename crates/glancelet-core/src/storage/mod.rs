use std::{collections::HashSet, path::Path, sync::Mutex};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection as SqliteConnection, OptionalExtension, Row, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use uuid::Uuid;

mod secrets;
pub use secrets::*;

use crate::{
    application::{
        default_widget_layout, DesktopPreferences, ProjectionFailureState, SourceFailureKind,
        SourceRuntime, StoredWork, WidgetInstance, WorkMutation, WorkStore,
    },
    domain::{
        LocalDisposition, ProgressAuthority, ProviderId, SourceBatch, SourceBatchKind,
        SourceChange, SourceChangeKind, SourceEntity, SourceIdentity, SourceMutation, WorkBinding,
        WorkBindingMode, WorkDraft, WorkEntry, WorkKind, WorkLifecycle, WorkPlanning, WorkProgress,
    },
    extension::{Connection, SourceConfig},
    GlanceletError, Result,
};

pub struct SqliteWorkStore {
    connection: Mutex<SqliteConnection>,
}

impl SqliteWorkStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = SqliteConnection::open(path).map_err(storage_error)?;
        Self::from_connection(connection)
    }

    pub fn in_memory() -> Result<Self> {
        Self::from_connection(SqliteConnection::open_in_memory().map_err(storage_error)?)
    }

    fn from_connection(mut connection: SqliteConnection) -> Result<Self> {
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
            .map_err(storage_error)?;
        let transaction = connection.transaction().map_err(storage_error)?;
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                   version INTEGER PRIMARY KEY,
                   name TEXT NOT NULL,
                   applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                 );",
            )
            .map_err(storage_error)?;
        let initial_applied = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=1)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_error)?;
        if !initial_applied {
            transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS connections (
                   id TEXT PRIMARY KEY,
                   provider_id TEXT NOT NULL,
                   display_name TEXT NOT NULL,
                   config_json TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS source_configs (
                   id TEXT PRIMARY KEY,
                   connection_id TEXT NOT NULL REFERENCES connections(id),
                   source_type_id TEXT NOT NULL,
                   display_name TEXT NOT NULL,
                   enabled INTEGER NOT NULL,
                   expected_sync_interval_seconds INTEGER NOT NULL,
                   settings_json TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS source_runtime (
                   source_config_id TEXT PRIMARY KEY REFERENCES source_configs(id) ON DELETE CASCADE,
                   checkpoint_json TEXT,
                   last_attempt_at TEXT,
                   last_success_at TEXT,
                   next_sync_at TEXT,
                   failure_count INTEGER NOT NULL DEFAULT 0,
                   last_error TEXT,
                   config_revision INTEGER NOT NULL DEFAULT 1,
                   failure_kind TEXT
                 );
                 CREATE TABLE IF NOT EXISTS source_entities (
                   id TEXT PRIMARY KEY,
                   source_config_id TEXT NOT NULL REFERENCES source_configs(id) ON DELETE CASCADE,
                   entity_type TEXT NOT NULL,
                   external_id TEXT NOT NULL,
                   title TEXT NOT NULL,
                   revision TEXT NOT NULL,
                   active INTEGER NOT NULL,
                   activation_seq INTEGER NOT NULL,
                   display_json TEXT NOT NULL,
                   metadata_json TEXT NOT NULL,
                   navigation_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   UNIQUE(source_config_id, entity_type, external_id)
                 );
                 CREATE TABLE IF NOT EXISTS source_changes (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   source_entity_id TEXT NOT NULL REFERENCES source_entities(id) ON DELETE CASCADE,
                   kind TEXT NOT NULL,
                   activation_seq INTEGER NOT NULL,
                   entity_snapshot_json TEXT NOT NULL,
                   occurred_at TEXT NOT NULL,
                   processed_at TEXT,
                   projection_failure_count INTEGER NOT NULL DEFAULT 0,
                   projection_last_error TEXT,
                   projection_next_retry_at TEXT,
                   projection_quarantined_at TEXT,
                   projection_projector_version INTEGER,
                   projection_superseded_at TEXT
                 );
                 CREATE INDEX IF NOT EXISTS source_changes_pending
                   ON source_changes(processed_at, id);
                 CREATE TABLE IF NOT EXISTS work_entries (
                   id TEXT PRIMARY KEY,
                   kind TEXT NOT NULL,
                   title TEXT NOT NULL,
                   summary TEXT,
                   priority INTEGER,
                   lifecycle TEXT NOT NULL,
                   progress TEXT,
                   planning_json TEXT,
                   disposition TEXT NOT NULL,
                   pinned INTEGER NOT NULL,
                   snoozed_until TEXT,
                   start_json TEXT,
                   end_json TEXT,
                   due_json TEXT,
                   dimensions_json TEXT NOT NULL,
                   facets_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS work_bindings (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   source_entity_id TEXT NOT NULL REFERENCES source_entities(id) ON DELETE CASCADE,
                   work_entry_id TEXT NOT NULL REFERENCES work_entries(id) ON DELETE CASCADE,
                   mode TEXT NOT NULL,
                   progress_authority TEXT NOT NULL,
                   source_activation_seq INTEGER NOT NULL,
                   projector_version INTEGER NOT NULL,
                   UNIQUE(source_entity_id, source_activation_seq)
                 );",
            )
            .map_err(storage_error)?;
            transaction
                .execute(
                    "INSERT INTO schema_migrations(version, name) VALUES (1, '001_initial')",
                    [],
                )
                .map_err(storage_error)?;
        }
        let lifecycle_applied = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=2)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_error)?;
        if !lifecycle_applied {
            let removed_column_exists = {
                let mut statement = transaction
                    .prepare("PRAGMA table_info(source_configs)")
                    .map_err(storage_error)?;
                let columns = statement
                    .query_map([], |row| row.get::<_, String>(1))
                    .map_err(storage_error)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(storage_error)?;
                columns.iter().any(|column| column == "removed_at")
            };
            if !removed_column_exists {
                transaction
                    .execute_batch("ALTER TABLE source_configs ADD COLUMN removed_at TEXT;")
                    .map_err(storage_error)?;
            }
            transaction
                .execute(
                    "UPDATE source_configs
                     SET enabled=0, removed_at=strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE json_extract(settings_json, '$._removed')=1 AND removed_at IS NULL",
                    [],
                )
                .map_err(storage_error)?;
            transaction
                .execute(
                    "UPDATE source_configs
                     SET settings_json=json_remove(settings_json, '$._removed')
                     WHERE json_type(settings_json, '$._removed') IS NOT NULL",
                    [],
                )
                .map_err(storage_error)?;
            transaction
                .execute(
                    "INSERT INTO schema_migrations(version, name)
                     VALUES (2, '002_source_config_lifecycle')",
                    [],
                )
                .map_err(storage_error)?;
        }
        let projection_failures_applied = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=3)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_error)?;
        if !projection_failures_applied {
            let columns = {
                let mut statement = transaction
                    .prepare("PRAGMA table_info(source_changes)")
                    .map_err(storage_error)?;
                let columns = statement
                    .query_map([], |row| row.get::<_, String>(1))
                    .map_err(storage_error)?
                    .collect::<std::result::Result<HashSet<_>, _>>()
                    .map_err(storage_error)?;
                columns
            };
            if !columns.contains("projection_failure_count") {
                transaction
                    .execute_batch(
                        "ALTER TABLE source_changes
                         ADD COLUMN projection_failure_count INTEGER NOT NULL DEFAULT 0;",
                    )
                    .map_err(storage_error)?;
            }
            if !columns.contains("projection_last_error") {
                transaction
                    .execute_batch(
                        "ALTER TABLE source_changes ADD COLUMN projection_last_error TEXT;",
                    )
                    .map_err(storage_error)?;
            }
            if !columns.contains("projection_next_retry_at") {
                transaction
                    .execute_batch(
                        "ALTER TABLE source_changes ADD COLUMN projection_next_retry_at TEXT;",
                    )
                    .map_err(storage_error)?;
            }
            transaction
                .execute_batch(
                    "CREATE INDEX IF NOT EXISTS source_changes_projection_due
                     ON source_changes(processed_at, projection_next_retry_at, id);",
                )
                .map_err(storage_error)?;
            transaction
                .execute(
                    "INSERT INTO schema_migrations(version, name)
                     VALUES (3, '003_projection_failure_retry')",
                    [],
                )
                .map_err(storage_error)?;
        }
        let consistency_applied = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=4)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_error)?;
        if !consistency_applied {
            let runtime_columns = {
                let mut statement = transaction
                    .prepare("PRAGMA table_info(source_runtime)")
                    .map_err(storage_error)?;
                let columns = statement
                    .query_map([], |row| row.get::<_, String>(1))
                    .map_err(storage_error)?
                    .collect::<std::result::Result<HashSet<_>, _>>()
                    .map_err(storage_error)?;
                columns
            };
            if !runtime_columns.contains("config_revision") {
                transaction
                    .execute_batch(
                        "ALTER TABLE source_runtime
                         ADD COLUMN config_revision INTEGER NOT NULL DEFAULT 1;",
                    )
                    .map_err(storage_error)?;
            }
            if !runtime_columns.contains("failure_kind") {
                transaction
                    .execute_batch("ALTER TABLE source_runtime ADD COLUMN failure_kind TEXT;")
                    .map_err(storage_error)?;
            }
            transaction
                .execute_batch(
                    r#"UPDATE source_runtime
                     SET failure_kind=CASE
                       WHEN last_error LIKE 'authentication is required:%'
                         THEN '"authentication_required"'
                       WHEN last_error LIKE '%rate limited%'
                         THEN '"rate_limited"'
                       WHEN last_error IS NOT NULL THEN '"other"'
                       ELSE NULL
                     END
                     WHERE failure_kind IS NULL;
                     CREATE INDEX IF NOT EXISTS source_changes_entity_pending
                       ON source_changes(source_entity_id, processed_at, id);
                     CREATE INDEX IF NOT EXISTS work_entries_dashboard
                       ON work_entries(lifecycle, disposition, snoozed_until);"#,
                )
                .map_err(storage_error)?;
            transaction
                .execute(
                    "INSERT INTO schema_migrations(version, name)
                     VALUES (4, '004_core_consistency')",
                    [],
                )
                .map_err(storage_error)?;
        }
        let retry_quarantine_applied = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=5)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_error)?;
        if !retry_quarantine_applied {
            let columns = {
                let mut statement = transaction
                    .prepare("PRAGMA table_info(source_changes)")
                    .map_err(storage_error)?;
                let columns = statement
                    .query_map([], |row| row.get::<_, String>(1))
                    .map_err(storage_error)?
                    .collect::<std::result::Result<HashSet<_>, _>>()
                    .map_err(storage_error)?;
                columns
            };
            if !columns.contains("projection_quarantined_at") {
                transaction
                    .execute_batch(
                        "ALTER TABLE source_changes ADD COLUMN projection_quarantined_at TEXT;",
                    )
                    .map_err(storage_error)?;
            }
            if !columns.contains("projection_projector_version") {
                transaction
                    .execute_batch(
                        "ALTER TABLE source_changes ADD COLUMN projection_projector_version INTEGER;",
                    )
                    .map_err(storage_error)?;
            }
            if !columns.contains("projection_superseded_at") {
                transaction
                    .execute_batch(
                        "ALTER TABLE source_changes ADD COLUMN projection_superseded_at TEXT;",
                    )
                    .map_err(storage_error)?;
            }
            transaction
                .execute_batch(
                    "CREATE INDEX IF NOT EXISTS source_changes_projection_ready
                       ON source_changes(processed_at, projection_quarantined_at,
                                         projection_superseded_at, projection_next_retry_at, id);",
                )
                .map_err(storage_error)?;
            transaction
                .execute(
                    "INSERT INTO schema_migrations(version, name)
                     VALUES (5, '005_retry_backoff_projection_quarantine')",
                    [],
                )
                .map_err(storage_error)?;
        }
        let unused_indexes_applied = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=6)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_error)?;
        if !unused_indexes_applied {
            // `source_changes_pending` and `source_changes_entity_pending` already serve
            // every pending-projection query; the planner never chose these two, so they
            // only cost writes.
            transaction
                .execute_batch(
                    "DROP INDEX IF EXISTS source_changes_projection_due;
                     DROP INDEX IF EXISTS source_changes_projection_ready;",
                )
                .map_err(storage_error)?;
            transaction
                .execute(
                    "INSERT INTO schema_migrations(version, name)
                     VALUES (6, '006_drop_unused_projection_indexes')",
                    [],
                )
                .map_err(storage_error)?;
        }
        let widget_layout_applied = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=7)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_error)?;
        if !widget_layout_applied {
            transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS widget_instances (
                       widget_type TEXT PRIMARY KEY,
                       position INTEGER NOT NULL UNIQUE,
                       size TEXT NOT NULL,
                       settings_json TEXT NOT NULL
                     );
                     CREATE TABLE IF NOT EXISTS desktop_preferences (
                       id INTEGER PRIMARY KEY CHECK (id=1),
                       always_on_top INTEGER NOT NULL
                     );
                     INSERT OR IGNORE INTO desktop_preferences(id, always_on_top) VALUES (1, 0);",
                )
                .map_err(storage_error)?;
            for widget in default_widget_layout() {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO widget_instances(
                           widget_type, position, size, settings_json
                         ) VALUES (?1, ?2, ?3, ?4)",
                        params![
                            json(&widget.widget_type)?,
                            widget.position,
                            json(&widget.size)?,
                            json(&widget.settings)?
                        ],
                    )
                    .map_err(storage_error)?;
            }
            transaction
                .execute(
                    "INSERT INTO schema_migrations(version, name)
                     VALUES (7, '007_widget_layout')",
                    [],
                )
                .map_err(storage_error)?;
        }
        let daily_use_preferences_applied = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=8)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .map_err(storage_error)?;
        if !daily_use_preferences_applied {
            if !table_has_column(
                &transaction,
                "desktop_preferences",
                "global_shortcut_enabled",
            )? {
                transaction
                    .execute(
                        "ALTER TABLE desktop_preferences
                           ADD COLUMN global_shortcut_enabled INTEGER NOT NULL DEFAULT 1",
                        [],
                    )
                    .map_err(storage_error)?;
            }
            if !table_has_column(&transaction, "desktop_preferences", "privacy_mode")? {
                transaction
                    .execute(
                        "ALTER TABLE desktop_preferences
                           ADD COLUMN privacy_mode INTEGER NOT NULL DEFAULT 0",
                        [],
                    )
                    .map_err(storage_error)?;
            }
            transaction
                .execute(
                    "INSERT INTO schema_migrations(version, name)
                     VALUES (8, '008_daily_use_preferences')",
                    [],
                )
                .map_err(storage_error)?;
        }
        transaction.commit().map_err(storage_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.connection
            .lock()
            .expect("sqlite connection poisoned")
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)
    }
}

fn table_has_column(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
    column: &str,
) -> Result<bool> {
    let mut statement = transaction
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(storage_error)?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(storage_error)?;
    for existing in columns {
        if existing.map_err(storage_error)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

impl WorkStore for SqliteWorkStore {
    fn put_connection(&self, connection: &Connection) -> Result<()> {
        let mut database = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = database.transaction().map_err(storage_error)?;
        put_connection_tx(&transaction, connection)?;
        transaction.commit().map_err(storage_error)
    }

    fn connect_connection(
        &self,
        connection: &Connection,
        source_configs: &[SourceConfig],
    ) -> Result<()> {
        let mut database = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = database.transaction().map_err(storage_error)?;
        put_connection_tx(&transaction, connection)?;
        for config in source_configs {
            if config.connection_id != connection.id {
                return Err(GlanceletError::InvalidOperation(
                    "connected source does not belong to the connection".into(),
                ));
            }
            put_source_config_tx(&transaction, config)?;
        }
        resume_connection_tx(&transaction, &connection.id)?;
        transaction.commit().map_err(storage_error)
    }

    fn connections(&self) -> Result<Vec<Connection>> {
        let connection = self.connection.lock().expect("sqlite connection poisoned");
        let mut statement = connection
            .prepare(
                "SELECT id, provider_id, display_name, config_json FROM connections ORDER BY id",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok(Connection {
                    id: row.get(0)?,
                    provider_id: crate::domain::ProviderId(row.get(1)?),
                    display_name: row.get(2)?,
                    config: parse_column(row, 3)?,
                })
            })
            .map_err(storage_error)?;
        rows.map(|row| row.map_err(storage_error)).collect()
    }

    fn disconnect_connection(&self, connection_id: &str, provider_id: &ProviderId) -> Result<()> {
        let mut connection = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = connection.transaction().map_err(storage_error)?;
        let mut stored = transaction
            .query_row(
                "SELECT id, provider_id, display_name, config_json
                 FROM connections WHERE id=?1",
                [connection_id],
                |row| {
                    Ok(Connection {
                        id: row.get(0)?,
                        provider_id: ProviderId(row.get(1)?),
                        display_name: row.get(2)?,
                        config: parse_column(row, 3)?,
                    })
                },
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| GlanceletError::NotFound(format!("connection {connection_id}")))?;
        if &stored.provider_id != provider_id {
            return Err(GlanceletError::InvalidOperation(
                "connection does not belong to the requested provider".into(),
            ));
        }
        let config = stored.config.as_object_mut().ok_or_else(|| {
            GlanceletError::Storage("connection configuration is not an object".into())
        })?;
        config.insert(
            "status".into(),
            serde_json::Value::String("disconnected".into()),
        );
        transaction
            .execute(
                "UPDATE connections SET config_json=?2 WHERE id=?1",
                params![connection_id, json(&stored.config)?],
            )
            .map_err(storage_error)?;
        transaction
            .execute(
                "UPDATE source_configs SET enabled=0 WHERE connection_id=?1",
                [connection_id],
            )
            .map_err(storage_error)?;
        transaction
            .execute(
                "UPDATE source_runtime
                 SET config_revision=config_revision+1
                 WHERE source_config_id IN (
                   SELECT id FROM source_configs WHERE connection_id=?1
                 )",
                [connection_id],
            )
            .map_err(storage_error)?;
        transaction.commit().map_err(storage_error)
    }

    fn resume_connection(&self, connection_id: &str) -> Result<()> {
        let mut database = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = database.transaction().map_err(storage_error)?;
        resume_connection_tx(&transaction, connection_id)?;
        transaction.commit().map_err(storage_error)
    }

    fn put_source_config(&self, config: &SourceConfig) -> Result<()> {
        let mut database = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = database.transaction().map_err(storage_error)?;
        put_source_config_tx(&transaction, config)?;
        transaction.commit().map_err(storage_error)
    }

    fn put_source_configs(&self, configs: &[SourceConfig]) -> Result<()> {
        let mut database = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = database.transaction().map_err(storage_error)?;
        let mut ids = HashSet::new();
        for config in configs {
            if !ids.insert(config.id.as_str()) {
                return Err(GlanceletError::InvalidOperation(format!(
                    "source config '{}' was supplied more than once",
                    config.id
                )));
            }
            put_source_config_tx(&transaction, config)?;
        }
        transaction.commit().map_err(storage_error)
    }

    fn source_configs(&self) -> Result<Vec<SourceConfig>> {
        let connection = self.connection.lock().expect("sqlite connection poisoned");
        let mut statement = connection
            .prepare(
                "SELECT id, connection_id, source_type_id, display_name, enabled,
                        expected_sync_interval_seconds, settings_json, removed_at
                 FROM source_configs ORDER BY id",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], source_config_from_row)
            .map_err(storage_error)?;
        rows.map(|row| row.map_err(storage_error)).collect()
    }

    fn source_config(&self, id: &str) -> Result<SourceConfig> {
        self.connection
            .lock()
            .expect("sqlite connection poisoned")
            .query_row(
                "SELECT id, connection_id, source_type_id, display_name, enabled,
                        expected_sync_interval_seconds, settings_json, removed_at
                 FROM source_configs WHERE id=?1",
                [id],
                source_config_from_row,
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| GlanceletError::NotFound(format!("source config {id}")))
    }

    fn source_runtime(&self, id: &str) -> Result<SourceRuntime> {
        self.connection
            .lock()
            .expect("sqlite connection poisoned")
            .query_row(
                "SELECT checkpoint_json, last_attempt_at, last_success_at, next_sync_at,
                        failure_count, last_error, config_revision, failure_kind
                 FROM source_runtime WHERE source_config_id=?1",
                [id],
                runtime_from_row,
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| GlanceletError::NotFound(format!("source runtime {id}")))
    }

    fn source_sync_state(&self, id: &str) -> Result<(SourceConfig, SourceRuntime)> {
        self.connection
            .lock()
            .expect("sqlite connection poisoned")
            .query_row(
                "SELECT sc.id, sc.connection_id, sc.source_type_id, sc.display_name,
                        sc.enabled, sc.expected_sync_interval_seconds, sc.settings_json,
                        sc.removed_at, sr.checkpoint_json, sr.last_attempt_at,
                        sr.last_success_at, sr.next_sync_at, sr.failure_count,
                        sr.last_error, sr.config_revision, sr.failure_kind
                 FROM source_configs sc
                 JOIN source_runtime sr ON sr.source_config_id=sc.id
                 WHERE sc.id=?1",
                [id],
                source_sync_state_from_row,
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| GlanceletError::NotFound(format!("source config {id}")))
    }

    fn record_sync_attempt(
        &self,
        id: &str,
        expected_config_revision: i64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let updated = self
            .connection
            .lock()
            .expect("sqlite connection poisoned")
            .execute(
                "UPDATE source_runtime SET last_attempt_at=?3
                 WHERE source_config_id=?1 AND config_revision=?2",
                params![id, expected_config_revision, timestamp(now)],
            )
            .map_err(storage_error)?;
        ensure_current_sync(updated)
    }

    fn record_sync_failure(
        &self,
        id: &str,
        expected_config_revision: i64,
        now: DateTime<Utc>,
        next_retry_at: Option<DateTime<Utc>>,
        kind: SourceFailureKind,
        error: &str,
    ) -> Result<()> {
        let updated = self
            .connection
            .lock()
            .expect("sqlite connection poisoned")
            .execute(
                "UPDATE source_runtime
                 SET last_attempt_at=?3, next_sync_at=?4,
                     failure_count=failure_count+1, last_error=?5, failure_kind=?6
                 WHERE source_config_id=?1 AND config_revision=?2",
                params![
                    id,
                    expected_config_revision,
                    timestamp(now),
                    next_retry_at.map(timestamp),
                    error,
                    json(&kind)?
                ],
            )
            .map_err(storage_error)?;
        ensure_current_sync(updated)
    }

    fn apply_source_batch(
        &self,
        config: &SourceConfig,
        expected_config_revision: i64,
        batch: &SourceBatch,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let mut database = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = database.transaction().map_err(storage_error)?;
        let current = transaction
            .query_row(
                "SELECT sc.enabled, sc.removed_at, sr.config_revision
                 FROM source_configs sc
                 JOIN source_runtime sr ON sr.source_config_id=sc.id
                 WHERE sc.id=?1",
                [&config.id],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| GlanceletError::NotFound(format!("source config {}", config.id)))?;
        if !current.0 || current.1.is_some() || current.2 != expected_config_revision {
            return Err(stale_sync_error());
        }

        let mut changes = 0;
        let mut snapshot_identities = HashSet::new();
        for mutation in &batch.mutations {
            match mutation {
                SourceMutation::Upsert(record) => {
                    snapshot_identities.insert((
                        record.identity.entity_type.clone(),
                        record.identity.external_id.clone(),
                    ));
                    changes += upsert_source(&transaction, config, record, now)?;
                }
                SourceMutation::Deactivate(identity) => {
                    changes += deactivate_source(&transaction, &config.id, identity, now)?;
                }
            }
        }

        if batch.kind == SourceBatchKind::FullSnapshot {
            let mut statement = transaction
                .prepare(
                    "SELECT entity_type, external_id FROM source_entities
                     WHERE source_config_id=?1 AND active=1",
                )
                .map_err(storage_error)?;
            let active = statement
                .query_map([&config.id], |row| {
                    Ok(SourceIdentity {
                        entity_type: row.get(0)?,
                        external_id: row.get(1)?,
                    })
                })
                .map_err(storage_error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage_error)?;
            drop(statement);
            for identity in active {
                if !snapshot_identities
                    .contains(&(identity.entity_type.clone(), identity.external_id.clone()))
                {
                    changes += deactivate_source(&transaction, &config.id, &identity, now)?;
                }
            }
        }

        let next_sync = now + chrono::Duration::seconds(config.expected_sync_interval_seconds);
        let updated = transaction
            .execute(
                "UPDATE source_runtime SET checkpoint_json=?3, last_success_at=?4,
                   next_sync_at=?5, failure_count=0, last_error=NULL, failure_kind=NULL
                 WHERE source_config_id=?1 AND config_revision=?2",
                params![
                    config.id,
                    expected_config_revision,
                    optional_json(batch.next_checkpoint.as_ref())?,
                    timestamp(now),
                    timestamp(next_sync)
                ],
            )
            .map_err(storage_error)?;
        ensure_current_sync(updated)?;
        transaction.commit().map_err(storage_error)?;
        Ok(changes)
    }

    fn pending_source_changes_at(
        &self,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<Vec<SourceChange>> {
        let connection = self.connection.lock().expect("sqlite connection poisoned");
        let mut statement = connection
            .prepare(
                "SELECT id, kind, entity_snapshot_json, occurred_at, processed_at
                 FROM source_changes
                 WHERE processed_at IS NULL
                   AND projection_quarantined_at IS NULL
                   AND projection_superseded_at IS NULL
                   AND (projection_next_retry_at IS NULL OR projection_next_retry_at<=?1)
                   AND NOT EXISTS (
                     SELECT 1 FROM source_changes earlier
                     WHERE earlier.source_entity_id=source_changes.source_entity_id
                       AND earlier.processed_at IS NULL
                       AND earlier.projection_quarantined_at IS NULL
                       AND earlier.projection_superseded_at IS NULL
                       AND earlier.id<source_changes.id
                   )
                 ORDER BY id LIMIT ?2",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map(params![timestamp(now), limit as i64], |row| {
                let snapshot: String = row.get(2)?;
                let kind: String = row.get(1)?;
                Ok(SourceChange {
                    id: row.get(0)?,
                    kind: parse_json(&kind).map_err(to_sql_error)?,
                    source_entity: parse_json(&snapshot).map_err(to_sql_error)?,
                    occurred_at: parse_timestamp(&row.get::<_, String>(3)?)
                        .map_err(to_sql_error)?,
                    processed_at: row
                        .get::<_, Option<String>>(4)?
                        .map(|value| parse_timestamp(&value))
                        .transpose()
                        .map_err(to_sql_error)?,
                })
            })
            .map_err(storage_error)?;
        rows.map(|row| row.map_err(storage_error)).collect()
    }

    fn record_projection_failure(
        &self,
        change_id: i64,
        projector_version: i32,
        failed_at: DateTime<Utc>,
        retry_base_seconds: i64,
        retry_max_seconds: i64,
        max_attempts: i64,
        error: &str,
    ) -> Result<ProjectionFailureState> {
        if retry_base_seconds <= 0 || retry_max_seconds < retry_base_seconds || max_attempts <= 0 {
            return Err(GlanceletError::InvalidOperation(
                "invalid projection retry policy".into(),
            ));
        }
        let mut database = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = database.transaction().map_err(storage_error)?;
        let current = transaction
            .query_row(
                "SELECT projection_failure_count, processed_at, projection_quarantined_at
                 FROM source_changes WHERE id=?1",
                [change_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| GlanceletError::NotFound(format!("source change {change_id}")))?;
        if current.1.is_some() || current.2.is_some() {
            return Err(GlanceletError::InvalidOperation(
                "projection failure can only be recorded for an active pending change".into(),
            ));
        }
        let failure_count = current.0.saturating_add(1);
        let quarantined_at = (failure_count >= max_attempts).then_some(failed_at);
        let next_retry_at = quarantined_at.is_none().then(|| {
            failed_at
                + chrono::Duration::seconds(projection_retry_delay_seconds(
                    change_id,
                    failure_count,
                    retry_base_seconds,
                    retry_max_seconds,
                ))
        });
        let updated = transaction
            .execute(
                "UPDATE source_changes
                 SET projection_failure_count=?2,
                     projection_last_error=?3,
                     projection_next_retry_at=?4,
                     projection_quarantined_at=?5,
                     projection_projector_version=?6
                 WHERE id=?1 AND processed_at IS NULL
                   AND projection_quarantined_at IS NULL",
                params![
                    change_id,
                    failure_count,
                    error,
                    next_retry_at.map(timestamp),
                    quarantined_at.map(timestamp),
                    projector_version
                ],
            )
            .map_err(storage_error)?;
        if updated == 0 {
            return Err(GlanceletError::InvalidOperation(
                "projection failure state changed concurrently".into(),
            ));
        }
        transaction.commit().map_err(storage_error)?;
        Ok(ProjectionFailureState {
            failure_count,
            next_retry_at,
            quarantined_at,
        })
    }

    fn enqueue_reprojections(
        &self,
        source_config_id: &str,
        projector_version: i32,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let mut database = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = database.transaction().map_err(storage_error)?;
        let entities = {
            let mut statement = transaction
                .prepare(
                    "SELECT se.id, se.source_config_id, se.entity_type, se.external_id,
                            se.title, se.revision, se.active, se.activation_seq,
                            se.display_json, se.metadata_json, se.navigation_json,
                            se.created_at, se.updated_at
                     FROM source_entities se
                     LEFT JOIN work_bindings wb ON wb.id=(
                       SELECT latest.id FROM work_bindings latest
                       WHERE latest.source_entity_id=se.id
                       ORDER BY latest.source_activation_seq DESC, latest.id DESC LIMIT 1
                     )
                     WHERE se.source_config_id=?1 AND se.active=1
                       AND (
                         (wb.id IS NOT NULL AND wb.projector_version<?2)
                         OR EXISTS (
                           SELECT 1 FROM source_changes quarantined
                           WHERE quarantined.source_entity_id=se.id
                             AND quarantined.processed_at IS NULL
                             AND quarantined.projection_quarantined_at IS NOT NULL
                             AND quarantined.projection_superseded_at IS NULL
                             AND COALESCE(quarantined.projection_projector_version, 0)<?2
                         )
                       )
                       AND NOT EXISTS (
                         SELECT 1 FROM source_changes pending
                         WHERE pending.source_entity_id=se.id
                           AND pending.processed_at IS NULL
                           AND pending.projection_quarantined_at IS NULL
                           AND pending.projection_superseded_at IS NULL
                       )",
                )
                .map_err(storage_error)?;
            let entities = statement
                .query_map(
                    params![source_config_id, projector_version],
                    source_entity_from_row,
                )
                .map_err(storage_error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage_error)?;
            entities
        };
        for entity in &entities {
            transaction
                .execute(
                    "UPDATE source_changes
                     SET projection_superseded_at=?3
                     WHERE source_entity_id=?1
                       AND processed_at IS NULL
                       AND projection_quarantined_at IS NOT NULL
                       AND projection_superseded_at IS NULL
                       AND COALESCE(projection_projector_version, 0)<?2",
                    params![entity.id, projector_version, timestamp(now)],
                )
                .map_err(storage_error)?;
            insert_change(&transaction, entity, SourceChangeKind::Updated, now)?;
        }
        transaction.commit().map_err(storage_error)?;
        Ok(entities.len())
    }

    fn apply_projection(
        &self,
        change: &SourceChange,
        draft: &WorkDraft,
        projector_version: i32,
        now: DateTime<Utc>,
    ) -> Result<()> {
        if draft.progress_authority == ProgressAuthority::None && draft.progress.is_some() {
            return Err(GlanceletError::InvalidOperation(
                "a work draft with no progress authority must not have progress".into(),
            ));
        }
        let mut connection = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = connection.transaction().map_err(storage_error)?;
        let processed: Option<String> = transaction
            .query_row(
                "SELECT processed_at FROM source_changes WHERE id=?1",
                [change.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)?
            .flatten();
        if processed.is_some() {
            transaction.commit().map_err(storage_error)?;
            return Ok(());
        }

        let existing = latest_binding(&transaction, &change.source_entity.id)?;
        match draft.binding_mode {
            WorkBindingMode::Mirror => apply_mirror_projection(
                &transaction,
                change,
                draft,
                existing,
                projector_version,
                now,
            )?,
            WorkBindingMode::Capture => apply_capture_projection(
                &transaction,
                change,
                draft,
                existing,
                projector_version,
                now,
            )?,
        }
        transaction
            .execute(
                "UPDATE source_changes
                 SET processed_at=?2, projection_last_error=NULL,
                     projection_next_retry_at=NULL, projection_quarantined_at=NULL
                 WHERE id=?1 AND processed_at IS NULL",
                params![change.id, timestamp(now)],
            )
            .map_err(storage_error)?;
        transaction.commit().map_err(storage_error)
    }

    fn stored_work(&self) -> Result<Vec<StoredWork>> {
        let connection = self.connection.lock().expect("sqlite connection poisoned");
        let mut statement = connection
            .prepare(&stored_work_query(
                "ORDER BY work_entries.created_at, work_entries.id",
            ))
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], stored_work_from_row)
            .map_err(storage_error)?;
        rows.map(|row| row.map_err(storage_error)).collect()
    }

    fn dashboard_work(&self, now: DateTime<Utc>) -> Result<Vec<StoredWork>> {
        let connection = self.connection.lock().expect("sqlite connection poisoned");
        let mut statement = connection
            .prepare(&stored_work_query(
                "WHERE work_entries.lifecycle=?1
                   AND work_entries.disposition!=?2
                   AND (
                     work_entries.disposition!=?3
                     OR (work_entries.snoozed_until IS NOT NULL
                         AND work_entries.snoozed_until<=?4)
                   )
                 ORDER BY work_entries.created_at, work_entries.id",
            ))
            .map_err(storage_error)?;
        let rows = statement
            .query_map(
                params![
                    json(&WorkLifecycle::Active)?,
                    json(&LocalDisposition::Dismissed)?,
                    json(&LocalDisposition::Snoozed)?,
                    timestamp(now)
                ],
                stored_work_from_row,
            )
            .map_err(storage_error)?;
        rows.map(|row| row.map_err(storage_error)).collect()
    }

    fn widget_layout(&self) -> Result<Vec<WidgetInstance>> {
        let connection = self.connection.lock().expect("sqlite connection poisoned");
        let mut statement = connection
            .prepare(
                "SELECT widget_type, position, size, settings_json
                 FROM widget_instances ORDER BY position",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(storage_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage_error)?;
        if rows.is_empty() {
            return Ok(default_widget_layout());
        }
        let mut widgets = Vec::with_capacity(rows.len());
        for (widget_type, position, size, settings) in rows {
            let parsed = (|| {
                let settings = parse_json::<serde_json::Value>(&settings)?;
                if position < 0 || !settings.is_object() {
                    return Err(GlanceletError::Storage("invalid widget layout row".into()));
                }
                Ok(WidgetInstance {
                    widget_type: parse_json(&widget_type)?,
                    position,
                    size: parse_json(&size)?,
                    settings,
                })
            })();
            match parsed {
                Ok(widget) => widgets.push(widget),
                Err(_) => return Ok(default_widget_layout()),
            }
        }
        Ok(widgets)
    }

    fn save_widget_layout(&self, widgets: &[WidgetInstance]) -> Result<()> {
        let mut connection = self.connection.lock().expect("sqlite connection poisoned");
        let transaction = connection.transaction().map_err(storage_error)?;
        transaction
            .execute("DELETE FROM widget_instances", [])
            .map_err(storage_error)?;
        for widget in widgets {
            transaction
                .execute(
                    "INSERT INTO widget_instances(widget_type, position, size, settings_json)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        json(&widget.widget_type)?,
                        widget.position,
                        json(&widget.size)?,
                        json(&widget.settings)?
                    ],
                )
                .map_err(storage_error)?;
        }
        transaction.commit().map_err(storage_error)
    }

    fn desktop_preferences(&self) -> Result<DesktopPreferences> {
        self.connection
            .lock()
            .expect("sqlite connection poisoned")
            .query_row(
                "SELECT always_on_top, global_shortcut_enabled, privacy_mode
                 FROM desktop_preferences WHERE id=1",
                [],
                |row| {
                    Ok(DesktopPreferences {
                        always_on_top: row.get(0)?,
                        global_shortcut_enabled: row.get(1)?,
                        privacy_mode: row.get(2)?,
                    })
                },
            )
            .map_err(storage_error)
    }

    fn save_desktop_preferences(&self, preferences: &DesktopPreferences) -> Result<()> {
        self.connection
            .lock()
            .expect("sqlite connection poisoned")
            .execute(
                "UPDATE desktop_preferences
                 SET always_on_top=?1, global_shortcut_enabled=?2, privacy_mode=?3
                 WHERE id=1",
                params![
                    preferences.always_on_top,
                    preferences.global_shortcut_enabled,
                    preferences.privacy_mode
                ],
            )
            .map(|_| ())
            .map_err(storage_error)
    }

    fn stored_work_by_id(&self, id: &str) -> Result<StoredWork> {
        self.connection
            .lock()
            .expect("sqlite connection poisoned")
            .query_row(
                &stored_work_query("WHERE work_entries.id=?1"),
                [id],
                stored_work_from_row,
            )
            .optional()
            .map_err(storage_error)?
            .ok_or_else(|| GlanceletError::NotFound(format!("work entry {id}")))
    }

    fn work_id_for_source_identity(
        &self,
        source_config_id: &str,
        identity: &SourceIdentity,
    ) -> Result<Option<String>> {
        self.connection
            .lock()
            .expect("sqlite connection poisoned")
            .query_row(
                "SELECT wb.work_entry_id
                 FROM source_entities se
                 JOIN work_bindings wb ON wb.source_entity_id=se.id
                 WHERE se.source_config_id=?1 AND se.entity_type=?2 AND se.external_id=?3
                 ORDER BY wb.source_activation_seq DESC LIMIT 1",
                params![source_config_id, identity.entity_type, identity.external_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_error)
    }

    fn mutate_work(&self, id: &str, mutation: WorkMutation, now: DateTime<Utc>) -> Result<()> {
        let connection = self.connection.lock().expect("sqlite connection poisoned");
        let updated = match mutation {
            WorkMutation::SetPlanning(planning) => connection.execute(
                "UPDATE work_entries SET planning_json=?2, updated_at=?3 WHERE id=?1",
                params![id, json(&planning)?, timestamp(now)],
            ),
            WorkMutation::Snooze(until) => connection.execute(
                "UPDATE work_entries SET disposition=?2, snoozed_until=?3, updated_at=?4
                 WHERE id=?1",
                params![
                    id,
                    json(&LocalDisposition::Snoozed)?,
                    timestamp(until),
                    timestamp(now)
                ],
            ),
            WorkMutation::Dismiss => connection.execute(
                "UPDATE work_entries SET disposition=?2, updated_at=?3 WHERE id=?1",
                params![id, json(&LocalDisposition::Dismissed)?, timestamp(now)],
            ),
            WorkMutation::SetPinned(pinned) => connection.execute(
                "UPDATE work_entries SET pinned=?2, updated_at=?3 WHERE id=?1",
                params![id, pinned, timestamp(now)],
            ),
        }
        .map_err(storage_error)?;
        if updated == 0 {
            return Err(GlanceletError::NotFound(format!("work entry {id}")));
        }
        Ok(())
    }

    fn transition_local_progress(
        &self,
        id: &str,
        allowed_from: &[WorkProgress],
        to: WorkProgress,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let connection = self.connection.lock().expect("sqlite connection poisoned");
        let next_progress = json(&to)?;
        let next_lifecycle = json(&if to == WorkProgress::Done {
            WorkLifecycle::Resolved
        } else {
            WorkLifecycle::Active
        })?;
        let active = json(&WorkLifecycle::Active)?;
        let local = json(&ProgressAuthority::Local)?;
        let allowed = allowed_from.iter().map(json).collect::<Result<Vec<_>>>()?;
        let updated = match allowed.as_slice() {
            [one] => connection.execute(
                "UPDATE work_entries
                 SET progress=?2, lifecycle=?3, updated_at=?4
                 WHERE id=?1 AND lifecycle=?5 AND progress=?6
                   AND EXISTS (
                     SELECT 1 FROM work_bindings
                     WHERE work_entry_id=?1 AND progress_authority=?7
                   )",
                params![
                    id,
                    next_progress,
                    next_lifecycle,
                    timestamp(now),
                    active,
                    one,
                    local
                ],
            ),
            [one, two] => connection.execute(
                "UPDATE work_entries
                 SET progress=?2, lifecycle=?3, updated_at=?4
                 WHERE id=?1 AND lifecycle=?5 AND progress IN (?6, ?7)
                   AND EXISTS (
                     SELECT 1 FROM work_bindings
                     WHERE work_entry_id=?1 AND progress_authority=?8
                   )",
                params![
                    id,
                    next_progress,
                    next_lifecycle,
                    timestamp(now),
                    active,
                    one,
                    two,
                    local
                ],
            ),
            _ => {
                return Err(GlanceletError::InvalidOperation(
                    "unsupported local progress transition".into(),
                ))
            }
        }
        .map_err(storage_error)?;
        if updated == 1 {
            return Ok(());
        }
        let local_authority = connection
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM work_bindings
                   WHERE work_entry_id=?1 AND progress_authority=?2
                 ) FROM work_entries WHERE id=?1",
                params![id, json(&ProgressAuthority::Local)?],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map_err(storage_error)?;
        match local_authority {
            None => Err(GlanceletError::NotFound(format!("work entry {id}"))),
            Some(false) => Err(GlanceletError::InvalidOperation(
                "progress is not locally controlled".into(),
            )),
            Some(true) => Err(GlanceletError::InvalidOperation(
                "work progress transition is stale or invalid".into(),
            )),
        }
    }
}

fn put_connection_tx(transaction: &Transaction<'_>, connection: &Connection) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO connections (id, provider_id, display_name, config_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET provider_id=?2, display_name=?3, config_json=?4",
            params![
                connection.id,
                connection.provider_id.0,
                connection.display_name,
                json(&connection.config)?
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn put_source_config_tx(transaction: &Transaction<'_>, config: &SourceConfig) -> Result<()> {
    let existing_removed = transaction
        .query_row(
            "SELECT removed_at IS NOT NULL FROM source_configs WHERE id=?1",
            [&config.id],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map_err(storage_error)?;
    let restoring = existing_removed == Some(true) && config.removed_at.is_none();
    transaction
        .execute(
            "INSERT INTO source_configs
               (id, connection_id, source_type_id, display_name, enabled,
                expected_sync_interval_seconds, settings_json, removed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET connection_id=?2, source_type_id=?3,
               display_name=?4, enabled=?5, expected_sync_interval_seconds=?6,
               settings_json=?7, removed_at=?8",
            params![
                config.id,
                config.connection_id,
                config.source_type_id.0,
                config.display_name,
                config.enabled,
                config.expected_sync_interval_seconds,
                json(&config.settings)?,
                config.removed_at.map(timestamp)
            ],
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO source_runtime (source_config_id) VALUES (?1)",
            [&config.id],
        )
        .map_err(storage_error)?;
    if existing_removed.is_some() {
        let configuration_required = json(&SourceFailureKind::ConfigurationRequired)?;
        transaction
            .execute(
                "UPDATE source_runtime
                 SET config_revision=config_revision+1,
                     next_sync_at=CASE WHEN failure_kind=?2 THEN NULL ELSE next_sync_at END,
                     failure_count=CASE WHEN failure_kind=?2 THEN 0 ELSE failure_count END,
                     last_error=CASE WHEN failure_kind=?2 THEN NULL ELSE last_error END,
                     failure_kind=CASE WHEN failure_kind=?2 THEN NULL ELSE failure_kind END
                 WHERE source_config_id=?1",
                params![config.id, configuration_required],
            )
            .map_err(storage_error)?;
    }
    if restoring {
        transaction
            .execute(
                "UPDATE source_runtime
                 SET checkpoint_json=NULL, next_sync_at=NULL,
                     failure_count=0, last_error=NULL, failure_kind=NULL
                 WHERE source_config_id=?1",
                [&config.id],
            )
            .map_err(storage_error)?;
    }
    Ok(())
}

fn resume_connection_tx(transaction: &Transaction<'_>, connection_id: &str) -> Result<()> {
    let authentication_required = json(&SourceFailureKind::AuthenticationRequired)?;
    transaction
        .execute(
            "UPDATE source_runtime
             SET config_revision=config_revision+1,
                 next_sync_at=CASE WHEN failure_kind=?2 THEN NULL ELSE next_sync_at END,
                 failure_count=CASE WHEN failure_kind=?2 THEN 0 ELSE failure_count END,
                 last_error=CASE WHEN failure_kind=?2 THEN NULL ELSE last_error END,
                 failure_kind=CASE WHEN failure_kind=?2 THEN NULL ELSE failure_kind END
             WHERE source_config_id IN (
               SELECT id FROM source_configs WHERE connection_id=?1
             )",
            params![connection_id, authentication_required],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn projection_retry_delay_seconds(
    change_id: i64,
    failure_count: i64,
    base_seconds: i64,
    max_seconds: i64,
) -> i64 {
    let exponent = (failure_count.saturating_sub(1)).clamp(0, 12) as u32;
    let multiplier = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
    let without_jitter = base_seconds.saturating_mul(multiplier).min(max_seconds);
    let jitter_window = (without_jitter / 10).max(1);
    let seed = change_id.unsigned_abs() ^ failure_count as u64;
    without_jitter
        .saturating_add((seed % (jitter_window as u64 + 1)) as i64)
        .min(max_seconds)
}

fn ensure_current_sync(updated: usize) -> Result<()> {
    if updated == 1 {
        Ok(())
    } else {
        Err(stale_sync_error())
    }
}

fn stale_sync_error() -> GlanceletError {
    GlanceletError::InvalidOperation(
        "source configuration changed while synchronization was running".into(),
    )
}

fn upsert_source(
    transaction: &Transaction<'_>,
    config: &SourceConfig,
    record: &crate::domain::SourceRecord,
    now: DateTime<Utc>,
) -> Result<usize> {
    let existing = transaction
        .query_row(
            "SELECT id, source_config_id, entity_type, external_id, title, revision, active,
                    activation_seq, display_json, metadata_json, navigation_json,
                    created_at, updated_at
             FROM source_entities
             WHERE source_config_id=?1 AND entity_type=?2 AND external_id=?3",
            params![
                config.id,
                record.identity.entity_type,
                record.identity.external_id
            ],
            source_entity_from_row,
        )
        .optional()
        .map_err(storage_error)?;

    let (entity, change_kind) = match existing {
        None => {
            let entity = SourceEntity {
                id: Uuid::new_v4().to_string(),
                source_config_id: config.id.clone(),
                identity: record.identity.clone(),
                title: record.title.clone(),
                revision: record.revision.clone(),
                active: true,
                activation_seq: 1,
                display: record.display.clone(),
                metadata: record.metadata.clone(),
                navigation: record.navigation.clone(),
                created_at: now,
                updated_at: now,
            };
            insert_source_entity(transaction, &entity)?;
            (entity, Some(SourceChangeKind::Created))
        }
        Some(mut entity) if !entity.active => {
            entity.title = record.title.clone();
            entity.revision = record.revision.clone();
            entity.active = true;
            entity.activation_seq += 1;
            entity.display = record.display.clone();
            entity.metadata = record.metadata.clone();
            entity.navigation = record.navigation.clone();
            entity.updated_at = now;
            update_source_entity(transaction, &entity)?;
            (entity, Some(SourceChangeKind::Reactivated))
        }
        Some(mut entity) => {
            let changed = entity.revision != record.revision
                || entity.title != record.title
                || entity.display != record.display
                || entity.metadata != record.metadata
                || entity.navigation != record.navigation;
            if changed {
                entity.title = record.title.clone();
                entity.revision = record.revision.clone();
                entity.display = record.display.clone();
                entity.metadata = record.metadata.clone();
                entity.navigation = record.navigation.clone();
                entity.updated_at = now;
                update_source_entity(transaction, &entity)?;
                (entity, Some(SourceChangeKind::Updated))
            } else {
                (entity, None)
            }
        }
    };
    if let Some(kind) = change_kind {
        insert_change(transaction, &entity, kind, now)?;
        Ok(1)
    } else {
        Ok(0)
    }
}

fn deactivate_source(
    transaction: &Transaction<'_>,
    source_config_id: &str,
    identity: &SourceIdentity,
    now: DateTime<Utc>,
) -> Result<usize> {
    let entity = transaction
        .query_row(
            "SELECT id, source_config_id, entity_type, external_id, title, revision, active,
                    activation_seq, display_json, metadata_json, navigation_json,
                    created_at, updated_at
             FROM source_entities
             WHERE source_config_id=?1 AND entity_type=?2 AND external_id=?3 AND active=1",
            params![source_config_id, identity.entity_type, identity.external_id],
            source_entity_from_row,
        )
        .optional()
        .map_err(storage_error)?;
    let Some(mut entity) = entity else {
        return Ok(0);
    };
    entity.active = false;
    entity.updated_at = now;
    transaction
        .execute(
            "UPDATE source_entities SET active=0, updated_at=?2 WHERE id=?1",
            params![entity.id, timestamp(now)],
        )
        .map_err(storage_error)?;
    insert_change(transaction, &entity, SourceChangeKind::Deactivated, now)?;
    Ok(1)
}

fn insert_source_entity(transaction: &Transaction<'_>, entity: &SourceEntity) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO source_entities
               (id, source_config_id, entity_type, external_id, title, revision, active,
                activation_seq, display_json, metadata_json, navigation_json, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                entity.id,
                entity.source_config_id,
                entity.identity.entity_type,
                entity.identity.external_id,
                entity.title,
                entity.revision,
                entity.active,
                entity.activation_seq,
                json(&entity.display)?,
                json(&entity.metadata)?,
                json(&entity.navigation)?,
                timestamp(entity.created_at),
                timestamp(entity.updated_at)
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn update_source_entity(transaction: &Transaction<'_>, entity: &SourceEntity) -> Result<()> {
    transaction
        .execute(
            "UPDATE source_entities SET title=?2, revision=?3, active=?4, activation_seq=?5,
               display_json=?6, metadata_json=?7, navigation_json=?8, updated_at=?9 WHERE id=?1",
            params![
                entity.id,
                entity.title,
                entity.revision,
                entity.active,
                entity.activation_seq,
                json(&entity.display)?,
                json(&entity.metadata)?,
                json(&entity.navigation)?,
                timestamp(entity.updated_at)
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn insert_change(
    transaction: &Transaction<'_>,
    entity: &SourceEntity,
    kind: SourceChangeKind,
    now: DateTime<Utc>,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO source_changes
               (source_entity_id, kind, activation_seq, entity_snapshot_json, occurred_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entity.id,
                json(&kind)?,
                entity.activation_seq,
                json(entity)?,
                timestamp(now)
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn latest_binding(
    transaction: &Transaction<'_>,
    source_entity_id: &str,
) -> Result<Option<WorkBinding>> {
    transaction
        .query_row(
            "SELECT source_entity_id, work_entry_id, mode, progress_authority,
                    source_activation_seq, projector_version
             FROM work_bindings WHERE source_entity_id=?1
             ORDER BY source_activation_seq DESC LIMIT 1",
            [source_entity_id],
            work_binding_from_row,
        )
        .optional()
        .map_err(storage_error)
}

fn apply_mirror_projection(
    transaction: &Transaction<'_>,
    change: &SourceChange,
    draft: &WorkDraft,
    existing: Option<WorkBinding>,
    projector_version: i32,
    now: DateTime<Utc>,
) -> Result<()> {
    match (change.kind, existing) {
        (SourceChangeKind::Deactivated, Some(binding)) => {
            transaction
                .execute(
                    "UPDATE work_entries SET lifecycle=?2, updated_at=?3 WHERE id=?1",
                    params![
                        binding.work_entry_id,
                        json(&WorkLifecycle::Resolved)?,
                        timestamp(now)
                    ],
                )
                .map_err(storage_error)?;
        }
        (SourceChangeKind::Deactivated, None) => {}
        (SourceChangeKind::Reactivated, Some(binding)) => {
            update_projected_fields(transaction, &binding, draft, projector_version, now)?;
            let planning = if draft.kind == WorkKind::Action {
                Some(json(&WorkPlanning::Inbox)?)
            } else {
                None
            };
            transaction
                .execute(
                    "UPDATE work_entries SET lifecycle=?2, planning_json=?3, disposition=?4,
                       snoozed_until=NULL, updated_at=?5 WHERE id=?1",
                    params![
                        binding.work_entry_id,
                        json(&WorkLifecycle::Active)?,
                        planning,
                        json(&LocalDisposition::Normal)?,
                        timestamp(now)
                    ],
                )
                .map_err(storage_error)?;
            transaction
                .execute(
                    "UPDATE work_bindings SET source_activation_seq=?2, mode=?3,
                       progress_authority=?4, projector_version=?5 WHERE work_entry_id=?1",
                    params![
                        binding.work_entry_id,
                        change.source_entity.activation_seq,
                        json(&draft.binding_mode)?,
                        json(&draft.progress_authority)?,
                        projector_version
                    ],
                )
                .map_err(storage_error)?;
        }
        (_, Some(binding)) => {
            update_projected_fields(transaction, &binding, draft, projector_version, now)?
        }
        (_, None) => insert_work_and_binding(transaction, change, draft, projector_version, now)?,
    }
    Ok(())
}

fn apply_capture_projection(
    transaction: &Transaction<'_>,
    change: &SourceChange,
    draft: &WorkDraft,
    existing: Option<WorkBinding>,
    projector_version: i32,
    now: DateTime<Utc>,
) -> Result<()> {
    match change.kind {
        SourceChangeKind::Created | SourceChangeKind::Reactivated => {
            let same_activation = existing.as_ref().is_some_and(|binding| {
                binding.source_activation_seq == change.source_entity.activation_seq
            });
            if !same_activation {
                insert_work_and_binding(transaction, change, draft, projector_version, now)?;
            }
        }
        SourceChangeKind::Updated => {
            if let Some(binding) = existing {
                update_projected_fields(transaction, &binding, draft, projector_version, now)?;
            } else {
                insert_work_and_binding(transaction, change, draft, projector_version, now)?;
            }
        }
        SourceChangeKind::Deactivated => {
            // Capture lifetime is local; removal of the source marker is not completion.
        }
    }
    Ok(())
}

fn insert_work_and_binding(
    transaction: &Transaction<'_>,
    change: &SourceChange,
    draft: &WorkDraft,
    projector_version: i32,
    now: DateTime<Utc>,
) -> Result<()> {
    let work_id = Uuid::new_v4().to_string();
    let planning = if draft.kind == WorkKind::Action {
        Some(json(&WorkPlanning::Inbox)?)
    } else {
        None
    };
    transaction
        .execute(
            "INSERT INTO work_entries
               (id, kind, title, summary, priority, lifecycle, progress, planning_json,
                disposition, pinned, snoozed_until, start_json, end_json, due_json,
                dimensions_json, facets_json, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,NULL,?10,?11,?12,?13,?14,?15,?16)",
            params![
                work_id,
                json(&draft.kind)?,
                draft.title,
                draft.summary,
                draft.priority,
                json(&WorkLifecycle::Active)?,
                optional_json(draft.progress.as_ref())?,
                planning,
                json(&LocalDisposition::Normal)?,
                optional_json(draft.start.as_ref())?,
                optional_json(draft.end.as_ref())?,
                optional_json(draft.due.as_ref())?,
                json(&draft.dimensions)?,
                json(&draft.facets)?,
                timestamp(now),
                timestamp(now)
            ],
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO work_bindings
               (source_entity_id, work_entry_id, mode, progress_authority,
                source_activation_seq, projector_version)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                change.source_entity.id,
                work_id,
                json(&draft.binding_mode)?,
                json(&draft.progress_authority)?,
                change.source_entity.activation_seq,
                projector_version
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn update_projected_fields(
    transaction: &Transaction<'_>,
    binding: &WorkBinding,
    draft: &WorkDraft,
    projector_version: i32,
    now: DateTime<Utc>,
) -> Result<()> {
    let progress = if draft.progress_authority == ProgressAuthority::Local {
        transaction
            .query_row(
                "SELECT progress FROM work_entries WHERE id=?1",
                [&binding.work_entry_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(storage_error)?
    } else {
        optional_json(draft.progress.as_ref())?
    };
    transaction
        .execute(
            "UPDATE work_entries SET kind=?2, title=?3, summary=?4, priority=?5,
               progress=?6, start_json=?7, end_json=?8, due_json=?9,
               dimensions_json=?10, facets_json=?11, updated_at=?12 WHERE id=?1",
            params![
                binding.work_entry_id,
                json(&draft.kind)?,
                draft.title,
                draft.summary,
                draft.priority,
                progress,
                optional_json(draft.start.as_ref())?,
                optional_json(draft.end.as_ref())?,
                optional_json(draft.due.as_ref())?,
                json(&draft.dimensions)?,
                json(&draft.facets)?,
                timestamp(now)
            ],
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "UPDATE work_bindings
             SET mode=?2, progress_authority=?3, projector_version=?4
             WHERE source_entity_id=?1 AND work_entry_id=?5",
            params![
                binding.source_entity_id,
                json(&draft.binding_mode)?,
                json(&draft.progress_authority)?,
                projector_version,
                binding.work_entry_id
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn stored_work_query(suffix: &str) -> String {
    format!(
        "SELECT
           work_entries.id, work_entries.kind, work_entries.title, work_entries.summary,
           work_entries.priority, work_entries.lifecycle, work_entries.progress,
           work_entries.planning_json, work_entries.disposition, work_entries.pinned,
           work_entries.snoozed_until, work_entries.start_json, work_entries.end_json,
           work_entries.due_json, work_entries.dimensions_json, work_entries.facets_json,
           work_entries.created_at, work_entries.updated_at,
           work_bindings.source_entity_id, work_bindings.work_entry_id, work_bindings.mode,
           work_bindings.progress_authority, work_bindings.source_activation_seq,
           work_bindings.projector_version,
           source_configs.id, source_configs.connection_id, source_configs.source_type_id,
           source_configs.display_name, source_configs.enabled,
           source_configs.expected_sync_interval_seconds, source_configs.settings_json,
           connections.id, connections.provider_id, connections.display_name,
           connections.config_json,
           source_runtime.checkpoint_json, source_runtime.last_attempt_at,
           source_runtime.last_success_at, source_runtime.next_sync_at,
           source_runtime.failure_count, source_runtime.last_error,
           source_runtime.config_revision, source_runtime.failure_kind,
           source_entities.display_json, source_entities.navigation_json,
           source_configs.removed_at
         FROM work_entries
         JOIN work_bindings ON work_bindings.work_entry_id=work_entries.id
         JOIN source_entities ON source_entities.id=work_bindings.source_entity_id
         JOIN source_configs ON source_configs.id=source_entities.source_config_id
         JOIN connections ON connections.id=source_configs.connection_id
         JOIN source_runtime ON source_runtime.source_config_id=source_configs.id
         {suffix}"
    )
}

fn stored_work_from_row(row: &Row<'_>) -> rusqlite::Result<StoredWork> {
    let entry = WorkEntry {
        id: row.get(0)?,
        kind: parse_column(row, 1)?,
        title: row.get(2)?,
        summary: row.get(3)?,
        priority: row.get(4)?,
        lifecycle: parse_column(row, 5)?,
        progress: parse_optional_column(row, 6)?,
        planning: parse_optional_column(row, 7)?,
        disposition: parse_column(row, 8)?,
        pinned: row.get(9)?,
        snoozed_until: parse_optional_timestamp_column(row, 10)?,
        start: parse_optional_column(row, 11)?,
        end: parse_optional_column(row, 12)?,
        due: parse_optional_column(row, 13)?,
        dimensions: parse_column(row, 14)?,
        facets: parse_column(row, 15)?,
        created_at: parse_timestamp_column(row, 16)?,
        updated_at: parse_timestamp_column(row, 17)?,
    };
    Ok(StoredWork {
        binding: WorkBinding {
            source_entity_id: row.get(18)?,
            work_entry_id: row.get(19)?,
            mode: parse_column(row, 20)?,
            progress_authority: parse_column(row, 21)?,
            source_activation_seq: row.get(22)?,
            projector_version: row.get(23)?,
        },
        source_config: SourceConfig {
            id: row.get(24)?,
            connection_id: row.get(25)?,
            source_type_id: crate::domain::SourceTypeId(row.get(26)?),
            display_name: row.get(27)?,
            enabled: row.get(28)?,
            expected_sync_interval_seconds: row.get(29)?,
            settings: parse_column(row, 30)?,
            removed_at: parse_optional_timestamp_column(row, 45)?,
        },
        connection: Connection {
            id: row.get(31)?,
            provider_id: crate::domain::ProviderId(row.get(32)?),
            display_name: row.get(33)?,
            config: parse_column(row, 34)?,
        },
        runtime: SourceRuntime {
            checkpoint: parse_optional_column(row, 35)?,
            last_attempt_at: parse_optional_timestamp_column(row, 36)?,
            last_success_at: parse_optional_timestamp_column(row, 37)?,
            next_sync_at: parse_optional_timestamp_column(row, 38)?,
            failure_count: row.get(39)?,
            last_error: row.get(40)?,
            config_revision: row.get(41)?,
            failure_kind: parse_optional_column(row, 42)?,
        },
        source_display: parse_column(row, 43)?,
        navigation: parse_column(row, 44)?,
        entry,
    })
}

fn source_config_from_row(row: &Row<'_>) -> rusqlite::Result<SourceConfig> {
    Ok(SourceConfig {
        id: row.get(0)?,
        connection_id: row.get(1)?,
        source_type_id: crate::domain::SourceTypeId(row.get(2)?),
        display_name: row.get(3)?,
        enabled: row.get(4)?,
        expected_sync_interval_seconds: row.get(5)?,
        settings: parse_column(row, 6)?,
        removed_at: parse_optional_timestamp_column(row, 7)?,
    })
}

fn runtime_from_row(row: &Row<'_>) -> rusqlite::Result<SourceRuntime> {
    Ok(SourceRuntime {
        checkpoint: parse_optional_column(row, 0)?,
        last_attempt_at: parse_optional_timestamp_column(row, 1)?,
        last_success_at: parse_optional_timestamp_column(row, 2)?,
        next_sync_at: parse_optional_timestamp_column(row, 3)?,
        failure_count: row.get(4)?,
        last_error: row.get(5)?,
        config_revision: row.get(6)?,
        failure_kind: parse_optional_column(row, 7)?,
    })
}

fn source_sync_state_from_row(row: &Row<'_>) -> rusqlite::Result<(SourceConfig, SourceRuntime)> {
    Ok((
        SourceConfig {
            id: row.get(0)?,
            connection_id: row.get(1)?,
            source_type_id: crate::domain::SourceTypeId(row.get(2)?),
            display_name: row.get(3)?,
            enabled: row.get(4)?,
            expected_sync_interval_seconds: row.get(5)?,
            settings: parse_column(row, 6)?,
            removed_at: parse_optional_timestamp_column(row, 7)?,
        },
        SourceRuntime {
            checkpoint: parse_optional_column(row, 8)?,
            last_attempt_at: parse_optional_timestamp_column(row, 9)?,
            last_success_at: parse_optional_timestamp_column(row, 10)?,
            next_sync_at: parse_optional_timestamp_column(row, 11)?,
            failure_count: row.get(12)?,
            last_error: row.get(13)?,
            config_revision: row.get(14)?,
            failure_kind: parse_optional_column(row, 15)?,
        },
    ))
}

fn source_entity_from_row(row: &Row<'_>) -> rusqlite::Result<SourceEntity> {
    Ok(SourceEntity {
        id: row.get(0)?,
        source_config_id: row.get(1)?,
        identity: SourceIdentity {
            entity_type: row.get(2)?,
            external_id: row.get(3)?,
        },
        title: row.get(4)?,
        revision: row.get(5)?,
        active: row.get(6)?,
        activation_seq: row.get(7)?,
        display: parse_column(row, 8)?,
        metadata: parse_column(row, 9)?,
        navigation: parse_column(row, 10)?,
        created_at: parse_timestamp_column(row, 11)?,
        updated_at: parse_timestamp_column(row, 12)?,
    })
}

fn work_binding_from_row(row: &Row<'_>) -> rusqlite::Result<WorkBinding> {
    Ok(WorkBinding {
        source_entity_id: row.get(0)?,
        work_entry_id: row.get(1)?,
        mode: parse_column(row, 2)?,
        progress_authority: parse_column(row, 3)?,
        source_activation_seq: row.get(4)?,
        projector_version: row.get(5)?,
    })
}

fn json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|error| GlanceletError::Storage(error.to_string()))
}

fn optional_json<T: Serialize>(value: Option<&T>) -> Result<Option<String>> {
    value.map(json).transpose()
}

fn parse_json<T: DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|error| GlanceletError::Storage(error.to_string()))
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| GlanceletError::Storage(error.to_string()))
}

fn parse_column<T: DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    parse_json(&row.get::<_, String>(index)?).map_err(to_sql_error)
}

fn parse_optional_column<T: DeserializeOwned>(
    row: &Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<T>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| parse_json(&value))
        .transpose()
        .map_err(to_sql_error)
}

fn parse_timestamp_column(row: &Row<'_>, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    parse_timestamp(&row.get::<_, String>(index)?).map_err(to_sql_error)
}

fn parse_optional_timestamp_column(
    row: &Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| parse_timestamp(&value))
        .transpose()
        .map_err(to_sql_error)
}

fn storage_error(error: rusqlite::Error) -> GlanceletError {
    GlanceletError::Storage(error.to_string())
}

fn to_sql_error(error: GlanceletError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}
