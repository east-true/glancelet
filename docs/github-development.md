# GitHub development

Phase 4 connects a GitHub App through Device Flow and creates three independent source types under one GitHub account:

- `github.review_requests` — open pull requests directly requesting the authenticated user's review
- `github.assigned_issues` — open assigned issues, excluding pull-request-shaped Issue API results
- `github.workflow_failures` — currently failing workflows on one selected repository's default branch

All three sources perform authoritative FullSnapshot syncs. User access and installation discovery are transient; SQLite stores only connection identity, source settings, normalized work, and runtime state. User access and refresh tokens are stored only in the OS credential store.

## GitHub App setup

1. In GitHub developer settings, create a GitHub App for Glancelet development.
2. Enable **Device Flow** in the App settings.
3. Grant only these read-only repository permissions:
   - Metadata: Read
   - Issues: Read
   - Pull requests: Read
   - Actions: Read
4. Do not grant write or webhook permissions for Phase 4.
5. Install the App on the user or organization repositories that Glancelet should see. Repository discovery is limited to the intersection of the authenticated user's access, the App installation's repository selection, and the App permissions.
6. Copy the App client ID and provide it when starting Glancelet:

   ```sh
   GLANCELET_GITHUB_CLIENT_ID=your-github-app-client-id npm run tauri dev
   ```

Do not configure or embed a client secret for the desktop Device Flow. GitHub's device authorization and device-flow refresh requests use the client ID without a client secret. If expiring user access tokens are enabled, Glancelet stores and rotates the returned refresh token bundle.

An account may authenticate successfully while exposing no repositories. This is a valid Connection state: install the GitHub App on the intended repositories, or have an organization owner approve/configure its installation, then refresh repositories in Glancelet. Organization policies and SAML SSO authorization may further limit access.

## Device Flow behavior

Choose **Sources → Connect GitHub**. Glancelet requests a device code, opens GitHub's verification page in the system browser, and displays the user code. The code is kept only for the connection UI; the device code remains in an expiring in-memory session.

Polling observes GitHub's returned interval. `authorization_pending` waits for the next interval, `slow_down` increases that interval, and denial or expiration ends the session. After authorization Glancelet validates the immutable numeric user ID through `GET /user`; the login is display-only. Reconnecting the same user replaces the OS-stored credential and resumes the existing Connection and SourceConfig identities.

## Source behavior

Review Requests searches for `is:open is:pr user-review-requested:@me`. Every page must succeed, `incomplete_results` must be false, and the result must fit GitHub Search's 1,000-result authoritative limit. Otherwise no snapshot is returned and existing Work remains active.

Assigned Issues uses the authenticated user's assigned, open Issues endpoint and skips responses containing the `pull_request` field. Both global sources use the stable GraphQL node ID as SourceEntity identity and project Mirror Actions with no local progress command.

Workflow Failures is one SourceConfig per repository. It discovers active workflows and requests the latest **completed** run for each workflow on the repository's default branch. A newer in-progress run therefore does not erase the last completed failure. `failure`, `timed_out`, `startup_failure`, and `action_required` are active failure conclusions; success and other conclusions are absent from the snapshot. Workflow ID is the stable occurrence identity, so a later failed run updates/reactivates the same Attention instead of creating history noise.

The adapter does not request or persist PR/Issue bodies, comments, diffs, repository source, workflow/job logs, artifacts, or raw responses.

## Manual E2E

1. Create the development GitHub App, enable Device Flow, configure the read permissions above, and install it on two test repositories if available.
2. Start Glancelet with `GLANCELET_GITHUB_CLIENT_ID` set.
3. Open **Sources**, choose **Connect GitHub**, enter the displayed code on GitHub, and confirm the connected login.
4. Add **Review Requests**. Request the connected user's review on an open PR, sync, and confirm an Inbox Action appears. Remove/complete the review request, sync, and confirm it resolves; request review again and confirm the same Work reactivates.
5. Add **Assigned Issues**. Assign an open Issue to the connected user and confirm an Inbox Action. Unassign or close it and confirm it resolves; reassign/reopen it and confirm reactivation.
6. Refresh repositories and add **Workflow Failures** for a repository. Produce a failing default-branch workflow and confirm an Attention linking to the failed run. Complete a later successful run and confirm it resolves.
7. While a newer run is in progress after a failure, sync and confirm the previous completed failure remains active.
8. Add a second repository's Workflow Failures source and verify sync/error state is independent from the first repository and global sources.
9. Disable, re-enable, remove, and re-add a source. Confirm history is preserved and the removed SourceConfig is restored without a duplicate.
10. Click each Work item and verify the validated HTTPS target opens the original PR, Issue, or workflow run.
11. Restart Glancelet and verify the Connection, source configuration, and OS-stored credential recover without a duplicate account.

CI uses mock HTTP fixtures and requires no GitHub credentials. A live GitHub App remains a manual product integration check.

## Official references

- [Generating a user access token with Device Flow](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app)
- [Refreshing user access tokens](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/refreshing-user-access-tokens)
- [GitHub App installations](https://docs.github.com/en/rest/apps/installations?apiVersion=2026-03-10)
- [Search issues and pull requests](https://docs.github.com/en/rest/search/search?apiVersion=2026-03-10#search-issues-and-pull-requests)
- [Issues assigned to the authenticated user](https://docs.github.com/en/rest/issues/issues?apiVersion=2026-03-10#list-issues-assigned-to-the-authenticated-user)
- [Actions workflows](https://docs.github.com/en/rest/actions/workflows?apiVersion=2026-03-10)
- [Workflow runs](https://docs.github.com/en/rest/actions/workflow-runs?apiVersion=2026-03-10)
- [REST API versions](https://docs.github.com/en/rest/about-the-rest-api/api-versions?apiVersion=2026-03-10)
- [REST API rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)
