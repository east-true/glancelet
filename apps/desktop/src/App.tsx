import { useCallback, useEffect, useState } from "react";
import {
  glanceletApi,
  type SlackConnection,
  type WorkAction,
  type WorkCommand,
  type WorkDashboard,
  type WorkView,
} from "./api";
import { localDateString } from "./local-time";
import "./styles.css";

const emptyDashboard: WorkDashboard = { today: [], inbox: [] };
type Tab = keyof WorkDashboard | "settings";

export default function App() {
  const [dashboard, setDashboard] = useState(emptyDashboard);
  const [tab, setTab] = useState<Tab>("today");
  const [slackConnections, setSlackConnections] = useState<SlackConnection[]>(
    [],
  );
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setError(null);
      setDashboard(await glanceletApi.dashboard());
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function sync() {
    setBusy(true);
    try {
      await glanceletApi.sync();
      await refresh();
    } catch (reason) {
      setError(String(reason));
      setBusy(false);
    }
  }

  const refreshSlack = useCallback(async () => {
    try {
      setError(null);
      setSlackConnections(await glanceletApi.slackConnections());
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  async function selectTab(next: Tab) {
    setTab(next);
    if (next === "settings") await refreshSlack();
  }

  async function connectSlack() {
    setBusy(true);
    try {
      await glanceletApi.connectSlack();
      await refreshSlack();
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  async function run(workId: string, command: WorkCommand) {
    try {
      await glanceletApi.command(workId, command);
      await refresh();
    } catch (reason) {
      setError(String(reason));
    }
  }

  return (
    <main className="hud-shell">
      <header className="masthead">
        <div>
          <p className="eyebrow">Your work, at a glance</p>
          <h1>Glancelet</h1>
        </div>
        <button
          className="sync-button"
          disabled={busy}
          onClick={() => void sync()}
        >
          {busy ? "Syncing…" : "Sync"}
        </button>
      </header>

      <nav className="tabs" aria-label="Work views">
        {(["today", "inbox", "settings"] as const).map((name) => (
          <button
            key={name}
            className={tab === name ? "active" : ""}
            onClick={() => void selectTab(name)}
          >
            {name === "today"
              ? "Today"
              : name === "inbox"
                ? "Inbox"
                : "Sources"}
            {name !== "settings" && <span>{dashboard[name].length}</span>}
          </button>
        ))}
      </nav>

      {error && <p className="error-banner">{error}</p>}
      {tab === "settings" ? (
        <SlackSettings
          busy={busy}
          connections={slackConnections}
          connect={connectSlack}
          refresh={refreshSlack}
          setError={setError}
        />
      ) : (
        <section className="work-list" aria-live="polite">
          {!busy && dashboard[tab].length === 0 ? (
            <div className="empty-state">
              <span>All clear</span>
              <p>
                {tab === "today"
                  ? "Nothing needs your attention today."
                  : "Your inbox is empty."}
              </p>
            </div>
          ) : (
            dashboard[tab].map((work) => (
              <WorkCard key={work.id} work={work} run={run} />
            ))
          )}
        </section>
      )}
    </main>
  );
}

function WorkCard({
  work,
  run,
}: {
  work: WorkView;
  run: (id: string, command: WorkCommand) => Promise<void>;
}) {
  const supports = (action: WorkAction) =>
    work.availableActions.includes(action);
  const open = () =>
    supports("open_source") && void glanceletApi.openSource(work.id);
  const today = localDateString(new Date());
  const oneHourFromNow = new Date(Date.now() + 60 * 60 * 1000).toISOString();

  return (
    <article className={`work-card kind-${work.kind}`}>
      <button className="work-main" disabled={!work.canNavigate} onClick={open}>
        <span className="kind-mark" aria-hidden="true" />
        <span className="work-copy">
          <span className="work-meta">
            {work.kind} · {work.source.configName}
            <i
              className={`freshness ${work.freshness}`}
              title={work.freshness}
            />
          </span>
          <strong>{work.title}</strong>
          {work.summary && <small>{work.summary}</small>}
        </span>
        {work.canNavigate && <span className="open-arrow">↗</span>}
      </button>
      <div className="work-actions">
        {supports("start_work") && (
          <button onClick={() => void run(work.id, { type: "start_work" })}>
            Start
          </button>
        )}
        {supports("complete") && (
          <button onClick={() => void run(work.id, { type: "complete" })}>
            Complete
          </button>
        )}
        {supports("move_to_backlog") && (
          <button
            onClick={() => void run(work.id, { type: "move_to_backlog" })}
          >
            Backlog
          </button>
        )}
        {supports("plan") && (
          <button
            onClick={() => void run(work.id, { type: "plan", date: today })}
          >
            Today
          </button>
        )}
        {supports("move_to_inbox") && work.planning?.type !== "inbox" && (
          <button onClick={() => void run(work.id, { type: "move_to_inbox" })}>
            Inbox
          </button>
        )}
        {supports("snooze") && (
          <button
            onClick={() =>
              void run(work.id, { type: "snooze", until: oneHourFromNow })
            }
          >
            Snooze
          </button>
        )}
        {supports("dismiss") && (
          <button onClick={() => void run(work.id, { type: "dismiss" })}>
            Dismiss
          </button>
        )}
        <button
          aria-label={work.pinned ? "Unpin" : "Pin"}
          onClick={() =>
            void run(work.id, { type: work.pinned ? "unpin" : "pin" })
          }
        >
          {work.pinned ? "Pinned" : "Pin"}
        </button>
      </div>
    </article>
  );
}

function SlackSettings({
  busy,
  connections,
  connect,
  refresh,
  setError,
}: {
  busy: boolean;
  connections: SlackConnection[];
  connect: () => Promise<void>;
  refresh: () => Promise<void>;
  setError: (error: string | null) => void;
}) {
  return (
    <section className="source-settings" aria-label="Sources">
      <div className="settings-heading">
        <div>
          <h2>Slack</h2>
          <p>Capture messages you react to with a configured emoji.</p>
        </div>
        <button disabled={busy} onClick={() => void connect()}>
          Connect Slack
        </button>
      </div>
      {connections.length === 0 ? (
        <div className="empty-source">No Slack workspace connected.</div>
      ) : (
        connections.map((connection) => (
          <SlackConnectionCard
            key={connection.connectionId}
            connection={connection}
            refresh={refresh}
            setError={setError}
          />
        ))
      )}
    </section>
  );
}

function SlackConnectionCard({
  connection,
  refresh,
  setError,
}: {
  connection: SlackConnection;
  refresh: () => Promise<void>;
  setError: (error: string | null) => void;
}) {
  const [reaction, setReaction] = useState(connection.reactionName);
  const [working, setWorking] = useState(false);

  async function action(task: () => Promise<void>) {
    setWorking(true);
    try {
      setError(null);
      await task();
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  }

  return (
    <article className="source-card">
      <div className="source-identity">
        <strong>{connection.workspace}</strong>
        <span>
          {connection.user} · {connection.status.replace("_", " ")}
        </span>
      </div>
      {connection.sourceId && (
        <label>
          Reaction name
          <input
            aria-label="Reaction name"
            value={reaction}
            onChange={(event) => setReaction(event.target.value)}
          />
        </label>
      )}
      <p className="source-runtime">
        Last sync: {connection.lastSync ?? "never"}
        {connection.lastError && <> · {connection.lastError}</>}
      </p>
      <div className="source-actions">
        {connection.sourceId && (
          <>
            <button
              disabled={working || !connection.enabled}
              onClick={() =>
                void action(() =>
                  glanceletApi.syncSource(connection.sourceId as string),
                )
              }
            >
              Sync now
            </button>
            <button
              disabled={working}
              onClick={() =>
                void action(() =>
                  glanceletApi.updateSlackSource(
                    connection.sourceId as string,
                    reaction,
                    connection.enabled,
                  ),
                )
              }
            >
              Save
            </button>
            <button
              disabled={working}
              onClick={() =>
                void action(() =>
                  glanceletApi.updateSlackSource(
                    connection.sourceId as string,
                    reaction,
                    !connection.enabled,
                  ),
                )
              }
            >
              {connection.enabled ? "Disable" : "Enable"}
            </button>
          </>
        )}
        <button
          className="danger"
          disabled={working || connection.status === "disconnected"}
          onClick={() =>
            void action(() =>
              glanceletApi.disconnectSlack(connection.connectionId),
            )
          }
        >
          Disconnect
        </button>
      </div>
    </article>
  );
}
