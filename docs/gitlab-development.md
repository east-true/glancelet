# GitLab development

Phase 5 connects GitLab.com or a self-managed GitLab instance and projects the current user's pending To-Dos through `gitlab.todos`. Every successful sync is an authoritative `FullSnapshot`; GitLab remains the source of truth and Glancelet does not mark To-Dos done.

## GitLab.com setup

GitLab.com uses the OAuth 2 Device Authorization Grant. This is a secretless public-client flow that avoids an HTTPS callback broker for the desktop application. Create a development OAuth application that allows Device Authorization Grant, grant only `read_api`, and provide its public client ID when starting Glancelet:

```sh
GLANCELET_GITLAB_CLIENT_ID=your-public-client-id npm run tauri dev
```

Choose **Sources → Connect GitLab.com**. Glancelet opens the verification page in the system browser and shows the user code. Device and user codes live only in the expiring in-memory session. Polling follows the server-provided interval; `slow_down` adds five seconds, while denial or expiration ends the flow. Access and rotating refresh tokens are stored only in the operating-system credential store.

## Self-managed setup

Create a Personal Access Token on the target instance with `read_api`, then choose **Sources → Self-managed GitLab** and enter:

- the instance origin, such as `https://gitlab.company.com` (not `/api/v4`);
- the PAT.

Glancelet requires HTTPS and keeps normal TLS certificate verification enabled. Plain HTTP is accepted only for loopback development hosts. Private/self-signed certificate authorities are not configurable in Phase 5. The PAT is validated against both `GET /api/v4/user` and the To-Dos endpoint before it is placed in the OS credential store; it is never returned to the frontend.

GitLab.com and self-managed identity is `(normalized instance origin, user ID)`. Therefore equal numeric user IDs on different instances remain separate Connections. Reconnecting replaces the stored credential on the existing Connection and resumes its SourceConfig rather than creating a duplicate.

## To-Dos behavior

Glancelet calls `GET /api/v4/todos?state=pending&per_page=100` and follows the official `Link: rel="next"` pagination contract. Every page must succeed before Core receives a `FullSnapshot`. For self-managed instances, every next-page URL must remain on the configured scheme, host, and port and under `/api/v4/`; a cross-origin Link is rejected before a credential can be sent.

The GitLab To-Do ID is the stable SourceEntity identity. Target title, project path, action, target type, timestamps, and the validated target URL are normalized into minimal source data. Target descriptions, To-Do bodies, comments, diffs, repository files, and raw responses are neither requested separately nor persisted. Unknown future action or target-type strings remain generic Actions rather than failing the sync.

A To-Do removed from a successful pending snapshot resolves its Mirror Work. A later To-Do with a new GitLab To-Do ID creates new Work. Disable and remove preserve history; re-adding the same Connection and source type restores the existing SourceConfig and forces a new authoritative snapshot.

## Merge request review semantic spike

`GET /api/v4/merge_requests?scope=reviews_for_me&state=opened` represents reviewer assignment. GitLab's contract does not guarantee that a merge request disappears when the authenticated reviewer completes a review, and a reviewer assignment can remain afterward. Phase 5 therefore deliberately defers `gitlab.review_requests`: assignment alone is not a reliable pending-review lifecycle, and combining it with To-Dos could also create duplicate Work for one merge request.

## Manual E2E

1. Configure the GitLab.com client ID, or create a self-managed PAT with `read_api`.
2. Connect the account and confirm the displayed instance and username.
3. Add **GitLab To-Dos**.
4. Create or mark a GitLab To-Do, sync, and confirm an Inbox Action appears.
5. Click the Work and verify the original target opens on the configured instance.
6. Mark the To-Do done in GitLab, sync, and confirm the Work resolves.
7. Create a new To-Do and confirm it creates new Work.
8. Restart Glancelet and confirm the Connection, SourceConfig, and OS-stored credential recover.
9. Disable, re-enable, remove, and re-add the source; confirm history is preserved and no duplicate SourceConfig is created.
10. Revoke or expire the credential and confirm automatic polling suspends without resolving Work; reconnect and confirm the same Connection resumes.
11. For self-managed GitLab, replace the PAT and confirm the instance identity remains stable.
12. If two test instances expose the same numeric user ID, connect both and confirm they remain separate.

Rate limiting and transient network/server failures preserve existing entities. `429` honors `Retry-After` or `RateLimit-Reset`; DNS, VPN, TLS, timeout, and `5xx` failures are not treated as authentication failures.

## Official references

- [GitLab OAuth 2.0 and Device Authorization Grant](https://docs.gitlab.com/api/oauth2/)
- [Personal access token scopes](https://docs.gitlab.com/security/tokens/access_token_scopes/)
- [To-Do API](https://docs.gitlab.com/api/todos/)
- [REST pagination](https://docs.gitlab.com/api/rest/)
- [Merge request scopes](https://docs.gitlab.com/api/merge_requests/)
- [User and IP rate limits](https://docs.gitlab.com/administration/settings/user_and_ip_rate_limits/)
