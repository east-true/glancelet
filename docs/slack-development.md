# Slack Reaction Capture development setup

Glancelet Phase 1 uses a user token and the single `reactions:read` user scope. It does not create a bot, request bot scopes, subscribe to events, or write to Slack. The flow uses PKCE S256 and Slack's standard v2 authorization/token endpoints, so a client secret must not be configured or embedded.

The implementation follows Slack's current [PKCE guide](https://docs.slack.dev/authentication/using-pkce/) and [`oauth.v2.access` reference](https://docs.slack.dev/reference/methods/oauth.v2.access/). Authorization begins at `https://slack.com/oauth/v2/authorize`, requests `reactions:read` as a user scope, and exchanges the code at `https://slack.com/api/oauth.v2.access`. The fixed redirect URI is:

```text
http://localhost:42813/oauth/slack/callback
```

Slack requires an exact redirect match. Glancelet binds only `127.0.0.1:42813`; if that port is occupied, Connect reports a retryable callback-port error.

## Create a development Slack App

1. Create an app at the Slack developer site using `docs/slack-app-manifest.yml`, or enter the same fields manually.
2. Under OAuth & Permissions, confirm PKCE is enabled.
3. Confirm the only user scope is `reactions:read` and that there are no bot scopes.
4. Confirm the redirect URL exactly matches the value above.
5. Copy the app's Client ID. A Client ID is public client configuration; do not copy or configure the Client Secret.

The development manifest enables token rotation. Glancelet uses Slack's response fields (`expires_in`, access token, and rotating refresh token) rather than assuming a lifetime. A manifest without token rotation may issue a long-lived token for this localhost redirect; the same code accepts that response without inventing a refresh flow. Slack documents that PKCE refresh tokens expire after 30 days, so an unused connection can eventually require authorization again.

## Run

The environment variable must be visible to the GUI process:

```sh
export GLANCELET_SLACK_CLIENT_ID=123456789.123456789
npm run tauri dev
```

Open **Sources**, choose **Connect Slack**, approve the user scope in the system browser, and return to Glancelet. The default configured reaction is `todo`. Add `:todo:` to a Slack message, then use **Sync now** or wait for the two-minute scheduled interval. The message appears in Inbox and opens its Slack web permalink when clicked.

Removing the reaction deactivates the source marker but does not complete or delete the captured work. If the same reaction is added after deactivation, Glancelet creates a new capture activation and retains prior work history.

## Credential storage

Tokens are serialized as one replaceable credential bundle in the OS credential store through keyring-rs:

- macOS: Keychain
- Windows: Credential Manager
- Linux: Secret Service

There is no plaintext fallback. On Linux, a Secret Service implementation and an unlocked session collection must be available (for example, GNOME Keyring or KWallet with Secret Service support). Otherwise Connect returns an `OS secret store is unavailable` error. SQLite contains workspace/user IDs, the reaction name, sync runtime, and normalized source/work data—but no access token, refresh token, PKCE verifier, authorization code, or raw Slack payload.

Disconnect disables local sync and deletes the local credential. Existing Work history remains. Phase 1 does not call Slack's remote revoke API.
