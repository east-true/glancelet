# Glancelet architecture

Glancelet treats providers as open registration metadata, not domain vocabulary. A source extension registers an adapter, projector, and descriptor under string identifiers. Adapters fetch and normalize external state; projectors assign work meaning without network or storage access.

The SQLite adapter atomically applies a complete `SourceBatch`, persists derived `SourceChange` records, and advances the checkpoint. Projection is a separate transaction and is idempotent per durable change, so projector failure never rolls back a successful source checkpoint.

The application exposes only `WorkView` to presentation. The React HUD does not know source metadata or database layout. Navigation targets are selected and validated in the application/Tauri boundary before the operating system opens them.

Only three stable ports exist: the source extension contracts, a coarse-grained `WorkStore`, and the minimal `SecretStore` credential boundary. `Clock` is also injectable because time changes observable product behavior. `TimeContext` converts instants to an OS-local date using IANA timezone rules. Application coordinators and command/query services remain concrete.

The repository deliberately has two Rust crates rather than mapping every architectural name to a crate. SQLite lives in `glancelet-core::storage` because its transaction logic is tightly coupled to the initial model; it remains behind `WorkStore`. Schema changes use a small ordered `schema_migrations` table, with the seven Phase 0 tables recorded as `001_initial`.

Slack is registered through the same source-code extension registry as fake sources. Its typed HTTP DTOs, PKCE session, token rotation, adapter, and projector stay under `sources::slack`; Slack DTOs do not enter the domain, and its capture projector reuses the existing activation-aware binding semantics. OAuth credentials go through `SecretStore`, while SQLite contains only connection identity and non-secret source configuration.

Runtime plugins, rules, AI, relations, generic OAuth infrastructure, widget/theme SDKs, and speculative database tables remain absent.
