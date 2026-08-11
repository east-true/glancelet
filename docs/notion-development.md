# Notion Data Source Tasks development setup

Glancelet Phase 2 treats the pages in one Notion Data Source as mirrored Actions. It reads mapped page properties only; it never retrieves page blocks, comments, files, or body content, and it does not write back to Notion.

The implementation uses Notion API version `2026-03-11` and the current Data Source endpoints:

- [`GET /v1/users/me`](https://developers.notion.com/guides/get-started/personal-access-tokens) validates the Personal Access Token and identifies its creator.
- [Search](https://developers.notion.com/reference/post-search) is a discovery convenience, not a complete workspace index.
- [Retrieve a data source](https://developers.notion.com/reference/retrieve-a-data-source) supplies the property schema.
- [Query a data source](https://developers.notion.com/reference/query-a-data-source) supplies a fully paginated snapshot of matching pages.

## Authentication scope

Phase 2 supports a local Notion Personal Access Token (PAT) only. A PAT acts with the permissions of its creator, so the people filter value `"me"` means that user. Internal Connection tokens are bot identities and are not supported by this flow. The token is validated in memory, then saved to the operating-system credential store through `SecretStore`; it is never returned to the frontend and is never stored in SQLite, source settings, logs, or browser storage.

A PAT is appropriate for a personal or developer-owned trusted local workflow. It is not the final authentication model for a broadly distributed multi-user product. A future official distribution should use Notion Public OAuth and may require an optional confidential-client broker. Phase 2 intentionally does not implement that flow or a generic authentication framework.

## Create a PAT

1. Open the Notion developer portal and create a Personal Access Token.
2. Enable the Notion API capability for the token. Workspace policy may require an owner to allow PAT creation.
3. Keep the token private. Do not put it in this repository, screenshots, shell history, issue reports, or example configuration.
4. Ensure the PAT creator can access the Data Source to be mirrored.
5. In Notion, open the database settings, choose **Manage data sources**, open the Data Source menu, and use **Copy data source ID** if search discovery does not show it immediately.

Notion Search is index-backed and can lag. The manual Data Source ID path is the reliable fallback.

## Manual end-to-end check

1. Run `npm run tauri dev`.
2. Open **Sources** and enter the PAT under **Notion**.
3. Choose **Connect Notion** and confirm the connected Notion user name.
4. Search accessible Data Sources, or paste a copied Data Source ID.
5. Load the schema.
6. Map the required title property and optional people, status, and date properties. Only compatible property types appear.
7. If people is mapped, leave **Only tasks assigned to me** enabled unless the whole Data Source should be mirrored.
8. If status is mapped, select the status options that represent active work. Completed options should normally remain unchecked.
9. Choose **Preview** and confirm that the returned task titles match the intended rows.
10. Choose **Add Source**, then **Sync now**.
11. Confirm that an active task appears in Inbox and can be planned into Today.
12. Change its title or due date in Notion, sync again, and confirm the projected fields change while Glancelet Planning, Snooze, and Pin remain unchanged.
13. Move the task to a status excluded from the active filter, sync, and confirm that the Work becomes resolved and disappears from active views.
14. Move it back to an active status, sync, and confirm reactivation resets Planning and Snooze/Dismiss while preserving Pin.
15. Click the Work card and confirm that the validated HTTPS target opens the original Notion page.
16. Disable/re-enable the source, then disconnect Notion. Confirm sync stops, the local PAT is removed, and existing Work history remains.

## Runtime behavior

Notion sync runs every five minutes by default. Every run retrieves the current schema, resolves saved property IDs against it, and queries all pages matching the configured assignee/status filters. Pagination cursors are query cursors only: Glancelet emits `FullSnapshot` only after every page succeeds. A later-page failure leaves all existing SourceEntities active. HTTP 429 responses honor `Retry-After`; other stored errors are sanitized and do not contain response bodies or credentials.

If a mapped property is removed or changes type, Sources shows a `Notion source needs configuration` error. Property renames continue working because source settings use property IDs as canonical identity and retain names only for display.
