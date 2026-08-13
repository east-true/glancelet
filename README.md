<div align="center">

<img src="apps/desktop/src-tauri/icons/128x128.png" alt="Glancelet" width="96" height="96">

# Glancelet

**A local-first, extensible desktop widget platform for the work that needs your attention.**

[![CI](https://github.com/east-true/glancelet/actions/workflows/ci.yml/badge.svg)](https://github.com/east-true/glancelet/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/tauri-2-24C8DB.svg)](https://tauri.app/)

</div>

<!-- Representative Desktop Surface screenshot goes here after manual visual validation. -->

Glancelet pulls work out of the tools it already lives in — Slack, Notion, Google
Calendar, GitHub, and GitLab — and puts it into a presentation-safe desktop view you can
keep on screen.

The original service always remains the source of truth. Glancelet does not try to
replace it or own your data: it helps you **discover** what needs attention,
**plan** it, **focus** on it, and **navigate back** to the tool where the real work
happens.

Everything is local. Work data lives in a SQLite database in your platform's
app-data directory, and provider credentials are stored **only** in your operating
system's credential store — never in the database, never on a Glancelet server.
There is no Glancelet server.

## Features

- **Desktop Work Surface** — configurable Today, Inbox, Upcoming, and Attention Widgets in one compact window.
- **Slack reaction capture** — react to a message to capture it as work.
- **Notion data source tasks** — mirror tasks from a Notion data source, with your own property mapping.
- **Google Calendar** — bring today's events into the same view.
- **GitHub work sources** — follow review requests, assigned issues, and repository workflow failures.
- **GitLab To-Dos** — follow pending personal attention from GitLab.com or a self-managed instance.
- **Plan, snooze, pin, dismiss** — lightweight local disposition that never writes back to the provider.
- **Presentation-safe boundary** — the HUD only ever receives a curated `WorkView`; credentials, raw provider payloads, and database layout never cross into the UI, and navigation targets are validated before the OS opens them.
- **Extensible by design** — sources register through a single extension boundary, so official and fork-specific sources use the same path.

## Status

Glancelet is **pre-1.0 (v0.1.0)** and under active development. There are no
prebuilt binaries yet — building from source is currently the only way to run it.
Interfaces and the on-disk schema may still change between versions.

## Getting started

### Prerequisites

- Stable Rust
- Node.js 22+ and npm
- The platform dependencies required by [Tauri 2](https://tauri.app/start/prerequisites/)

On Debian/Ubuntu, the Tauri system dependencies are:

```sh
sudo apt-get install -y --no-install-recommends \
  libwebkit2gtk-4.1-dev pkg-config build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

### Run

```sh
npm install
npm run tauri dev
```

To include the development-only fake sources, enable the `demo-sources` feature:

```sh
npm run tauri -- dev --features demo-sources
```

Connecting a real provider needs a little setup — see the per-source guides in
[Documentation](#documentation) below.

### Verify

```sh
cargo test --workspace
npm test
npm run build
```

## How it works

A source adapter fetches a batch from the provider; the batch becomes durable
`SourceChange` records; a projector turns those into the `WorkEntry` model that the
HUD renders. Core contains no provider-specific branches, and the SQLite adapter
contains no provider-specific logic — that boundary is what makes a new source a
contained addition rather than a change to the core.

```text
Slack / Notion / Google / GitHub / GitLab source → SourceBatch → SourceEntity → durable SourceChange
                             → WorkProjector → WorkEntry → WorkView → built-in Widgets
```

[`docs/architecture.md`](docs/architecture.md) covers the boundaries, the migration
history, and the omissions that were deliberate.

## Documentation

| Document                                                     | Contents                                                              |
| ------------------------------------------------------------ | --------------------------------------------------------------------- |
| [Architecture](docs/architecture.md)                         | Architectural boundaries, schema migrations, and deliberate omissions |
| [Slack setup](docs/slack-development.md)                     | Development Slack App and Secret Service setup                        |
| [Notion setup](docs/notion-development.md)                   | Notion PAT, task mapping, and manual E2E setup                        |
| [Google Calendar setup](docs/google-calendar-development.md) | Google Desktop OAuth, calendar selection, and manual E2E setup        |
| [GitHub setup](docs/github-development.md)                   | GitHub App Device Flow, permissions, source setup, and manual E2E     |
| [GitLab setup](docs/gitlab-development.md)                   | GitLab.com Device Flow, self-managed PAT, and To-Dos manual E2E       |

## Repository layout

- `crates/glancelet-core` — domain, application services, the extension boundary, the Slack/Notion/Google/GitHub/GitLab sources, and the SQLite adapter
- `apps/desktop` — the React HUD and its thin Tauri command layer
- `docs/` — architecture and per-source setup guides

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) first —
in particular, open an issue before starting substantial changes, and never include
credentials, tokens, or private workspace content in issues, tests, or pull requests.

To report a security vulnerability, follow [SECURITY.md](SECURITY.md) rather than
opening a public issue.

This project follows a [Code of Conduct](CODE_OF_CONDUCT.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
