# Contributing to Glancelet

Thanks for your interest in contributing to Glancelet.

## Before you start

- Search existing issues before opening a new one.
- For substantial changes, open an issue first so scope and architecture can be discussed.
- Do not include credentials, OAuth tokens, private workspace data, or provider content in issues, tests, fixtures, or pull requests.

## Development

Glancelet uses Rust, Node.js, React, and Tauri 2. See `README.md` and the documents under `docs/` for setup and source-specific development guidance.

Before opening a pull request, run the checks relevant to your change. For broad changes, use:

```sh
npm install
cargo test --workspace
npm test
npm run build
```

## Pull requests

Keep pull requests focused. Explain what changed, why it changed, and how it was validated. Preserve Glancelet's local-first and presentation-safe boundaries; existing services remain the source of truth.

By submitting a contribution, you agree that your contribution is licensed under the Apache License 2.0.
