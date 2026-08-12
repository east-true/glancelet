import { useState } from "react";
import {
  glanceletApi,
  syncReportMessage,
  type GoogleCalendar,
  type GoogleConnection,
} from "./api";

export function GoogleSettings({
  busy,
  connections,
  refresh,
  refreshWork,
  setError,
}: {
  busy: boolean;
  connections: GoogleConnection[];
  refresh: () => Promise<void>;
  refreshWork: () => Promise<void>;
  setError: (value: string | null) => void;
}) {
  async function connect() {
    try {
      setError(null);
      await glanceletApi.connectGoogle();
      await refresh();
    } catch (reason) {
      setError(String(reason));
    }
  }

  return (
    <section className="source-settings" aria-label="Google Calendar sources">
      <div className="source-heading">
        <div>
          <h2>Google Calendar</h2>
          <p>Mirror event occurrences from selected calendars.</p>
        </div>
        <button disabled={busy} onClick={() => void connect()}>
          Connect Google
        </button>
      </div>
      {connections.length === 0 ? (
        <div className="empty-source">No Google account connected.</div>
      ) : (
        connections.map((connection) => (
          <GoogleConnectionCard
            key={connection.connectionId}
            connection={connection}
            refresh={refresh}
            refreshWork={refreshWork}
            setError={setError}
          />
        ))
      )}
    </section>
  );
}

function GoogleConnectionCard({
  connection,
  refresh,
  refreshWork,
  setError,
}: {
  connection: GoogleConnection;
  refresh: () => Promise<void>;
  refreshWork: () => Promise<void>;
  setError: (value: string | null) => void;
}) {
  const [calendars, setCalendars] = useState<GoogleCalendar[]>([]);
  const [selected, setSelected] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);

  async function act(operation: () => Promise<unknown>, work = false) {
    setLoading(true);
    try {
      setError(null);
      await operation();
      await refresh();
      if (work) await refreshWork();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }

  async function discover() {
    setLoading(true);
    try {
      const values = await glanceletApi.googleCalendars(
        connection.connectionId,
      );
      setCalendars(values);
      const existing = new Set(
        connection.sources.map((source) => source.calendarId),
      );
      setSelected(
        values
          .filter((calendar) => calendar.selected && !existing.has(calendar.id))
          .map((calendar) => calendar.id),
      );
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }

  async function syncSource(sourceId: string) {
    setLoading(true);
    try {
      setError(null);
      const report = await glanceletApi.syncSource(sourceId);
      await Promise.all([refresh(), refreshWork()]);
      setError(syncReportMessage(report));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }

  return (
    <article className="connection-card">
      <div className="connection-title">
        <div>
          <strong>{connection.email}</strong>
          <span>{connection.status.replaceAll("_", " ")}</span>
        </div>
        <button disabled={loading} onClick={() => void discover()}>
          Refresh calendars
        </button>
      </div>

      {connection.sources.map((source) => (
        <div className="source-row" key={source.sourceId}>
          <div>
            <strong>{source.name}</strong>
            <span>
              {source.lastError ?? (source.enabled ? "Enabled" : "Disabled")}
            </span>
          </div>
          <div className="source-actions">
            <button
              disabled={loading || !source.enabled}
              onClick={() => void syncSource(source.sourceId)}
            >
              Sync now
            </button>
            <button
              disabled={loading}
              onClick={() =>
                void act(() =>
                  glanceletApi.updateGoogleSource(
                    source.sourceId,
                    !source.enabled,
                  ),
                )
              }
            >
              {source.enabled ? "Disable" : "Enable"}
            </button>
            <button
              disabled={loading}
              onClick={() =>
                void act(() => glanceletApi.removeGoogleSource(source.sourceId))
              }
            >
              Remove
            </button>
          </div>
        </div>
      ))}

      {calendars.length > 0 && (
        <fieldset className="calendar-picker">
          <legend>Select calendars</legend>
          {calendars.map((calendar) => {
            const alreadyAdded = connection.sources.some(
              (source) => source.calendarId === calendar.id,
            );
            return (
              <label key={calendar.id}>
                <input
                  type="checkbox"
                  disabled={alreadyAdded}
                  checked={alreadyAdded || selected.includes(calendar.id)}
                  onChange={(event) =>
                    setSelected((current) =>
                      event.target.checked
                        ? [...current, calendar.id]
                        : current.filter((id) => id !== calendar.id),
                    )
                  }
                />
                {calendar.summaryOverride ?? calendar.summary}
              </label>
            );
          })}
          <button
            disabled={loading || selected.length === 0}
            onClick={() =>
              void act(
                () =>
                  glanceletApi.saveGoogleCalendars(
                    connection.connectionId,
                    selected,
                  ),
                true,
              ).then(() => setSelected([]))
            }
          >
            Add selected calendars
          </button>
        </fieldset>
      )}

      <button
        className="disconnect-button"
        disabled={loading}
        onClick={() =>
          void act(
            () => glanceletApi.disconnectGoogle(connection.connectionId),
            true,
          )
        }
      >
        Disconnect Google
      </button>
    </article>
  );
}
