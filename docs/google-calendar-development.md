# Google Calendar development

Phase 3 uses Google OAuth for Desktop Apps with Authorization Code + PKCE S256 and a random `127.0.0.1` loopback port. It requests only `openid`, `email`, and `https://www.googleapis.com/auth/calendar.readonly`. The OIDC `sub` claim is the stable Connection identity; email is display-only. Tokens are stored in the OS credential store and are never returned to the frontend or written to SQLite.

## Google Cloud setup

1. Create or select a Google Cloud project.
2. Enable the Google Calendar API.
3. Configure the OAuth consent screen and its test users while the app is in testing mode.
4. Create an OAuth client with application type **Desktop app**.
5. Set its client ID before launching Glancelet:

   ```sh
   GLANCELET_GOOGLE_CLIENT_ID=your-desktop-client-id npm run tauri dev
   ```

Do not add a client secret. An installed desktop binary is a public client and cannot keep one confidential. The app opens the system browser, binds only `127.0.0.1` on an OS-assigned port, validates a random state, and consumes the callback once. The production OAuth consent screen may require Google verification for the Calendar scope; that external verification workflow is not part of Phase 3.

DPoP is not implemented. It remains a future hardening candidate because correct support also requires a device-bound private-key lifecycle and suitable protected key storage.

## Sync behavior

Each selected Calendar becomes an independent `google.calendar` SourceConfig under the Google Connection. Initial sync fetches a rolling window from seven days in the past through ninety days in the future with `singleEvents=true`, `showDeleted=true`, and the current IANA timezone. Every page must succeed before a FullSnapshot and its final `nextSyncToken` can commit.

Subsequent syncs use that token and return a Delta. `timeMin` and `timeMax` are not sent with `syncToken`. A 410 response performs a new bounded full fetch in memory; a failed recovery leaves the prior entities and checkpoint intact. A local date or query-timezone change also starts a fresh bounded reconciliation so the rolling horizon advances.

Recurring series are requested as instances. Each occurrence uses `recurringEventId + originalStartTime`, so moving one occurrence updates the same Work and cancelling one resolves only that occurrence. The adapter persists title, start/end, event type, navigation URL, and minimal identity metadata only. It does not persist description, attendees, conference data, attachments, or raw responses.

The adapter uses only the transient attendee with `self=true` to detect a declined invitation. A Delta transition to declined emits `Deactivate`; accepting again emits `Upsert` and uses normal Mirror reactivation. If `endTimeUnspecified=true`, Google's compatibility `end` is ignored and Work stores no end; Today then uses only the Event start date.

## Manual E2E

1. Start Glancelet with `GLANCELET_GOOGLE_CLIENT_ID` set.
2. Open **Sources**, choose **Connect Google**, and finish consent in the system browser.
3. Confirm the displayed account email, choose **Refresh calendars**, select Work and optionally a second Calendar, then add them.
4. Choose **Sync now** for the Work Calendar. Create a normal event in the rolling window and confirm it appears in Today on its local date.
5. Change its title and time, sync again, and confirm the existing Work updates rather than duplicating.
6. Create a one-day all-day event and verify it appears only on its start date. Create a multi-day all-day event and verify the exclusive end date is not included.
7. Create a recurring event. Move one occurrence and confirm the same occurrence Work updates. Cancel another occurrence and confirm only that Work resolves.
8. Add a second Calendar Source and verify each source reports its own last sync/error and continues independently if the other fails.
9. Click an Event Work and confirm the validated HTTPS target opens the original Google Calendar event.
10. Restart Glancelet and verify the credential and per-Calendar checkpoint recover without consent or a duplicate initial history.
11. Change an invitation from accepted to declined and verify its Work resolves; accept it again and verify the same Work reactivates.
12. Disable and re-enable a Calendar, then remove it; existing Work history must remain. Re-add the same Calendar and verify the same SourceConfig is restored with a bounded full reconciliation rather than its old sync token. Disconnect Google and verify syncing stops and the OS-stored credential is deleted while history remains.

Live OAuth is intentionally manual and CI requires no Google credential. Google Tasks, write-back/RSVP, service accounts, watch channels, push notifications, and DPoP are outside Phase 3.

## Official references

- [OAuth 2.0 for Desktop Apps](https://developers.google.com/identity/protocols/oauth2/native-app)
- [Google OpenID Connect UserInfo](https://developers.google.com/identity/openid-connect/reference)
- [CalendarList.list](https://developers.google.com/workspace/calendar/api/v3/reference/calendarList/list)
- [Events.list](https://developers.google.com/workspace/calendar/api/v3/reference/events/list)
- [Incremental synchronization](https://developers.google.com/workspace/calendar/api/guides/sync)
- [Event resource and recurring identity](https://developers.google.com/workspace/calendar/api/v3/reference/events)
- [Calendar API errors and quotas](https://developers.google.com/workspace/calendar/api/guides/errors)
