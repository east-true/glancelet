# Glancelet architecture

Glancelet treats providers as open registration metadata, not domain vocabulary. A source extension registers an adapter, projector, and descriptor under string identifiers. Adapters fetch and normalize external state; projectors assign work meaning without network or storage access.

The SQLite adapter atomically applies a complete `SourceBatch`, persists derived `SourceChange` records, and advances the checkpoint. Every fetch captures the SourceConfig generation it started with; the commit transaction rejects the result if configuration, lifecycle, connection state, or credentials changed while the provider request was in flight. Projection is a separate transaction and is idempotent per durable change, so projector failure never rolls back a successful source checkpoint. Failed changes defer only their own entity: independent entities continue, while later changes for the same SourceEntity retain causal order.

Absence is meaningful only after a successful authoritative `FullSnapshot`, or through an explicit `Delta` deactivation. Fetch, authentication, pagination, rate-limit, and provider failures never deactivate SourceEntities or resolve Work.

The application exposes only `WorkView` to presentation. The React HUD does not know source metadata or database layout. Navigation targets are selected and validated in the application/Tauri boundary before the operating system opens them.

Only three stable ports exist: the source extension contracts, a coarse-grained `WorkStore`, and the minimal `SecretStore` credential boundary. `Clock` is also injectable because time changes observable product behavior. `TimeContext` converts instants to an OS-local date using IANA timezone rules; its production form re-reads the system timezone so a Calendar checkpoint can reconcile after an OS timezone change, while named test contexts remain fixed. Application coordinators and command/query services remain concrete.

The repository deliberately has two Rust crates rather than mapping every architectural name to a crate. SQLite lives in `glancelet-core::storage` because its transaction logic is tightly coupled to the initial model; it remains behind `WorkStore`. Schema changes use a small ordered `schema_migrations` table, with the seven Phase 0 tables recorded as `001_initial`. Phase 3 adds the provider-neutral `002_source_config_lifecycle`, moving history-preserving removal out of provider settings into `source_configs.removed_at`; reliability migration `003_projection_failure_retry` adds provider-neutral projection retry metadata to durable SourceChanges. Consistency migration `004_core_consistency` adds SourceConfig generations, structured sync failure kinds, causal projection indexes, and dashboard-read indexes without provider-specific schema.

`SourceConfig` lifecycle has two independent axes. `enabled=false, removed_at=NULL` is a reversible pause; a non-null `removed_at` is history-preserving removal and always excludes the source from scheduled/manual sync and active configuration lists. Re-adding the same provider source restores its existing SourceConfig identity, updates its current settings, and clears the old checkpoint so the next sync is an authoritative reconciliation. Migration `002` promotes and then removes the legacy Notion `_removed` settings marker.

Slack is registered through the same source-code extension registry as fake sources. Its typed HTTP DTOs, PKCE session, token rotation, adapter, and projector stay under `sources::slack`; Slack DTOs do not enter the domain, and its capture projector reuses the existing activation-aware binding semantics. OAuth credentials go through `SecretStore`, while SQLite contains only connection identity and non-secret source configuration.

Notion is registered through that same boundary without Core or schema branches. Its PAT identity validation, Data Source schema/query DTOs, property-ID mappings, full-snapshot adapter, and mirror projector stay under `sources::notion`. Full synchronization still requires an authoritative complete snapshot, while preview queries stop after the requested rows and never paginate the entire Data Source. Notion uses external progress authority and the existing Mirror lifecycle. PATs share only the minimal `SecretStore` boundary with Slack; the static PAT flow and Slack's rotating PKCE credentials deliberately remain separate.

Google Calendar is the first real delta source. Its bounded daily reconciliation and `syncToken` pagination both stay inside `sources::google`, returning only the existing `FullSnapshot` or `Delta` batches. A 410 response triggers a complete bounded fetch before any replacement checkpoint can commit. Recurrence is expanded by Google and normalized as occurrence-level entities using `recurringEventId + originalStartTime`; no RRULE model enters Core. One Google Connection can own multiple independent Calendar SourceConfigs and checkpoints, and multi-calendar configuration is committed as one SQLite transaction. Calendar work is an existing Mirror Event with no progress authority. An attendee transition to declined is an ordinary Delta deactivation, and accepting again is an ordinary upsert/reactivation. `endTimeUnspecified` maps to the existing optional Event end rather than a provider-specific temporal type. Calendar window boundaries use the first representable local instant when a timezone transition removes local midnight or an entire local date.

Core contains no provider-ID behavior branches, and SQLite contains no provider-specific tables or columns. Provider configuration and minimal normalized metadata remain in the existing generic JSON fields.

Event ranges use provider-neutral half-open `[start, end)` semantics. This represents Google all-day exclusive ends and timed/multi-day events without provider-specific domain fields, while preserving Notion date-only values and Google IANA timezone metadata in `TemporalValue`.

The concrete scheduler runs a bounded number of independent SourceConfigs concurrently and delegates same-source single-flight to `SyncCoordinator`; weak lock entries disappear after inactive sources and credentials are no longer in use. Shutdown stops new scheduling; external fetches occur outside write transactions and every SQLite write is transactional, so runtime cancellation cannot commit a partial batch. Authentication-required, rate-limited, and other failures are stored as structured state rather than inferred from display text. Authentication-required failures preserve Work and checkpoints, suspend automatic provider calls, and make manual sync request reconnection. Reconnecting the same Connection atomically advances the generation of its SourceConfigs and clears only authentication failure state; transient failures keep their ordinary retry scheduling.

Local progress transitions are conditional SQLite updates, so concurrent Start and Complete commands cannot resurrect resolved Work. Manual synchronization drains projection work in bounded batches and reports a safety-limit partial result instead of declaring completion after the first 500 changes. Projector version increases enqueue provider-neutral reprojection changes and update WorkBinding metadata after successful projection. Dashboard reads filter resolved, dismissed, and future-snoozed history in SQLite before JSON deserialization.

## Core Architecture v0 validation

Validated real sources:

- Slack reaction Capture — FullSnapshot, local progress
- Notion data source tasks — Mirror FullSnapshot, external progress
- Google Calendar — Mirror FullSnapshot + Delta, occurrence Events, no progress

Phase 3.5 integration tests cover source lifecycle restoration, multi-provider failure isolation and single-flight, restart persistence, credential separation, Google declined transitions, and unspecified Event ends. All current Core boundaries are stable for these providers.

**Glancelet Core Architecture v0 — Status: VALIDATED.** This freezes the internal direction, not a public SDK or semantic-version compatibility promise. New Providers use these contracts by default; Core changes require evidence from a real Provider that the current model cannot represent correctly.

## Desktop Integration Gate

**Local Desktop Integration Gate — Status: PASSED.** The current tree passes native `cargo check -p glancelet-desktop --all-features` and desktop clippy on Linux with GTK/WebKitGTK prerequisites.

**Current-tree GitHub Actions — Status: PASSED.** CI installs the current Tauri 2 Debian/Ubuntu prerequisites and separately runs native desktop check and clippy. The Core v0 freeze tree passed both native and web jobs; this operational gate remains independent of Core Architecture validation.

Runtime plugins, rules, AI, relations, generic OAuth infrastructure, widget/theme SDKs, and speculative database tables remain absent.
