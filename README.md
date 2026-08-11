# Glancelet

> Glancelet is a local-first, extensible desktop widget platform for the work that needs your attention.

Glancelet brings work from existing systems into presentation-safe desktop views. The original service remains the source of truth: Glancelet helps you discover, plan, focus, and navigate back to it.

Phase 0 established the local-first core with two fake sources. Phase 1 added Slack Reaction Capture, Phase 2 added Notion Data Source Tasks, and Phase 3 adds Google Calendar:

```text
Fake / Slack / Notion / Google source → SourceBatch → SourceEntity → durable SourceChange
                    → WorkProjector → WorkEntry → WorkView → Today / Inbox HUD
```

The fake mirror source models externally owned tasks; the fake capture source models explicit captures such as a tagged chat message. They use the same source-code extension registry intended for future official and fork-specific sources.

## Repository

- `crates/glancelet-core`: domain, application services, extension boundary, fake/Slack/Notion/Google sources, and SQLite adapter
- `apps/desktop`: React HUD and its thin Tauri command layer
- `docs/architecture.md`: architectural boundaries and deliberate omissions
- `docs/slack-development.md`: development Slack App and Secret Service setup
- `docs/notion-development.md`: Notion PAT, task mapping, and manual E2E setup
- `docs/google-calendar-development.md`: Google Desktop OAuth, Calendar selection, and manual E2E setup

## Development

Requirements: stable Rust, Node.js 22+, npm, and the platform dependencies required by Tauri 2.

```sh
npm install
cargo test --workspace
npm test
npm run build
npm run tauri dev
```

The desktop app stores work data in the platform app-data directory as `glancelet.db`. Provider credentials are stored only in the operating system credential store, never in SQLite.
