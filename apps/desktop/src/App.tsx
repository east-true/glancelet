import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";
import {
  glanceletApi,
  type GoogleConnection,
  type NotionConnection,
  type NotionDataSource,
  type NotionDataSourceSummary,
  type NotionPreviewRow,
  type NotionPropertyMapping,
  type NotionSource,
  type NotionSourceSettings,
  type SlackConnection,
  type WorkAction,
  type WorkCommand,
  type WorkDashboard,
  type WorkView,
} from "./api";
import { GoogleSettings } from "./GoogleSettings";
import { localDateString } from "./local-time";
import "./styles.css";

const emptyDashboard: WorkDashboard = { today: [], inbox: [] };
const DASHBOARD_TIME_REFRESH_MS = 60_000;
type Tab = keyof WorkDashboard | "settings";

export default function App() {
  const [dashboard, setDashboard] = useState(emptyDashboard);
  const [tab, setTab] = useState<Tab>("today");
  const [slackConnections, setSlackConnections] = useState<SlackConnection[]>(
    [],
  );
  const [notionConnections, setNotionConnections] = useState<
    NotionConnection[]
  >([]);
  const [googleConnections, setGoogleConnections] = useState<
    GoogleConnection[]
  >([]);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async (clearError = true) => {
    try {
      if (clearError) setError(null);
      setDashboard(await glanceletApi.dashboard());
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen("glancelet://work-changed", () => void refresh(false)).then(
      (dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      },
    );
    const timer = window.setInterval(
      () => void refresh(false),
      DASHBOARD_TIME_REFRESH_MS,
    );
    const initialRefresh = window.setTimeout(() => void refresh(), 0);
    return () => {
      disposed = true;
      unlisten?.();
      window.clearTimeout(initialRefresh);
      window.clearInterval(timer);
    };
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

  const refreshNotion = useCallback(async () => {
    try {
      setError(null);
      setNotionConnections(await glanceletApi.notionConnections());
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  const refreshGoogle = useCallback(async () => {
    try {
      setError(null);
      setGoogleConnections(await glanceletApi.googleConnections());
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  async function selectTab(next: Tab) {
    setTab(next);
    if (next === "settings") {
      await Promise.all([refreshSlack(), refreshNotion(), refreshGoogle()]);
    }
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
        <div className="settings-stack">
          <SlackSettings
            busy={busy}
            connections={slackConnections}
            connect={connectSlack}
            refresh={refreshSlack}
            setError={setError}
          />
          <NotionSettings
            busy={busy}
            connections={notionConnections}
            refresh={refreshNotion}
            refreshWork={refresh}
            setError={setError}
          />
          <GoogleSettings
            busy={busy}
            connections={googleConnections}
            refresh={refreshGoogle}
            refreshWork={refresh}
            setError={setError}
          />
        </div>
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

function NotionSettings({
  busy,
  connections,
  refresh,
  refreshWork,
  setError,
}: {
  busy: boolean;
  connections: NotionConnection[];
  refresh: () => Promise<void>;
  refreshWork: () => Promise<void>;
  setError: (error: string | null) => void;
}) {
  const [token, setToken] = useState("");
  const [working, setWorking] = useState(false);

  async function connect() {
    setWorking(true);
    try {
      setError(null);
      await glanceletApi.connectNotion(token);
      setToken("");
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  }

  return (
    <section className="source-settings" aria-label="Notion sources">
      <div className="settings-heading notion-connect-heading">
        <div>
          <h2>Notion</h2>
          <p>Mirror tasks from a mapped Notion data source.</p>
        </div>
        <div className="token-connect">
          <input
            aria-label="Notion Personal Access Token"
            type="password"
            autoComplete="off"
            placeholder="Personal Access Token"
            value={token}
            onChange={(event) => setToken(event.target.value)}
          />
          <button
            disabled={busy || working || token.trim() === ""}
            onClick={() => void connect()}
          >
            Connect Notion
          </button>
        </div>
      </div>
      {connections.length === 0 ? (
        <div className="empty-source">No Notion account connected.</div>
      ) : (
        connections.map((connection) => (
          <NotionConnectionCard
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

function NotionConnectionCard({
  connection,
  refresh,
  refreshWork,
  setError,
}: {
  connection: NotionConnection;
  refresh: () => Promise<void>;
  refreshWork: () => Promise<void>;
  setError: (error: string | null) => void;
}) {
  const [working, setWorking] = useState(false);
  const [query, setQuery] = useState("");
  const [dataSourceId, setDataSourceId] = useState("");
  const [searchResults, setSearchResults] = useState<NotionDataSourceSummary[]>(
    [],
  );
  const [schema, setSchema] = useState<NotionDataSource | null>(null);
  const [titleId, setTitleId] = useState("");
  const [assigneeId, setAssigneeId] = useState("");
  const [statusId, setStatusId] = useState("");
  const [dueId, setDueId] = useState("");
  const [onlyMe, setOnlyMe] = useState(true);
  const [activeStatusIds, setActiveStatusIds] = useState<string[]>([]);
  const [preview, setPreview] = useState<NotionPreviewRow[]>([]);
  const [editingSourceId, setEditingSourceId] = useState<string | null>(null);

  async function action(task: () => Promise<void>) {
    setWorking(true);
    try {
      setError(null);
      await task();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  }

  function property(id: string): NotionPropertyMapping | null {
    const match = schema?.properties.find((candidate) => candidate.id === id);
    return match ? { id: match.id, name: match.name } : null;
  }

  function settings(): NotionSourceSettings | null {
    const title = property(titleId);
    if (!schema || !title) return null;
    return {
      dataSourceId: schema.id,
      dataSourceName: schema.title,
      properties: {
        title,
        assignee: property(assigneeId),
        status: property(statusId),
        due: property(dueId),
      },
      onlyAssignedToMe: assigneeId ? onlyMe : false,
      activeStatusIds: statusId ? activeStatusIds : [],
    };
  }

  async function loadSchema(id: string, existing?: NotionSource) {
    const value = await glanceletApi.notionDataSourceSchema(
      connection.connectionId,
      id,
    );
    setSchema(value);
    setDataSourceId(value.id);
    const defaultStatus = value.properties.find(
      (candidate) => candidate.type === "status",
    );
    setTitleId(
      existing?.settings.properties.title.id ??
        value.properties.find((candidate) => candidate.type === "title")?.id ??
        "",
    );
    const nextAssigneeId =
      existing?.settings.properties.assignee?.id ??
      value.properties.find((candidate) => candidate.type === "people")?.id ??
      "";
    setAssigneeId(nextAssigneeId);
    setStatusId(
      existing?.settings.properties.status?.id ?? defaultStatus?.id ?? "",
    );
    setDueId(
      existing?.settings.properties.due?.id ??
        value.properties.find((candidate) => candidate.type === "date")?.id ??
        "",
    );
    setOnlyMe(
      nextAssigneeId ? (existing?.settings.onlyAssignedToMe ?? true) : false,
    );
    setActiveStatusIds(
      existing?.settings.activeStatusIds ??
        defaultStatus?.status?.groups
          .slice(0, 2)
          .flatMap((group) => group.optionIds) ??
        [],
    );
    setEditingSourceId(existing?.sourceId ?? null);
    setPreview([]);
  }

  function toggleStatus(id: string) {
    setActiveStatusIds((current) =>
      current.includes(id)
        ? current.filter((value) => value !== id)
        : [...current, id],
    );
  }

  const statusSchema = schema?.properties.find(
    (candidate) => candidate.id === statusId,
  )?.status;

  return (
    <article className="source-card notion-card">
      <div className="source-identity">
        <strong>{connection.user}</strong>
        <span>{connection.status.replace("_", " ")}</span>
      </div>
      {connection.sources.map((source) => (
        <div className="notion-source-row" key={source.sourceId}>
          <div>
            <strong>{source.name}</strong>
            <span>
              {source.enabled ? "enabled" : "disabled"} · Last sync:{" "}
              {source.lastSync ?? "never"}
            </span>
            {source.lastError && <small>{source.lastError}</small>}
          </div>
          <div className="source-actions">
            <button
              disabled={working || !source.enabled}
              onClick={() =>
                void action(async () => {
                  await glanceletApi.syncSource(source.sourceId);
                  await Promise.all([refresh(), refreshWork()]);
                })
              }
            >
              Sync now
            </button>
            <button
              disabled={working}
              onClick={() =>
                void action(() => loadSchema(source.dataSourceId, source))
              }
            >
              Reconfigure
            </button>
            <button
              disabled={working}
              onClick={() =>
                void action(async () => {
                  await glanceletApi.updateNotionSource(
                    source.sourceId,
                    !source.enabled,
                  );
                  await refresh();
                })
              }
            >
              {source.enabled ? "Disable" : "Enable"}
            </button>
            <button
              className="danger"
              disabled={working}
              onClick={() =>
                void action(async () => {
                  await glanceletApi.removeNotionSource(source.sourceId);
                  await refresh();
                })
              }
            >
              Remove
            </button>
          </div>
        </div>
      ))}

      {connection.status !== "disconnected" && (
        <div className="notion-setup">
          <h3>
            {editingSourceId ? "Reconfigure task source" : "Add task source"}
          </h3>
          <div className="inline-fields">
            <input
              aria-label="Search Notion data sources"
              placeholder="Search accessible sources"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
            <button
              disabled={working}
              onClick={() =>
                void action(async () => {
                  setSearchResults(
                    await glanceletApi.searchNotionDataSources(
                      connection.connectionId,
                      query,
                    ),
                  );
                })
              }
            >
              Search
            </button>
          </div>
          {searchResults.length > 0 && (
            <div className="search-results">
              {searchResults.map((result) => (
                <button
                  key={result.id}
                  disabled={working}
                  onClick={() => void action(() => loadSchema(result.id))}
                >
                  {result.title}
                </button>
              ))}
            </div>
          )}
          <div className="inline-fields">
            <input
              aria-label="Notion Data Source ID"
              placeholder="Data Source ID"
              value={dataSourceId}
              onChange={(event) => setDataSourceId(event.target.value)}
            />
            <button
              disabled={working || dataSourceId.trim() === ""}
              onClick={() => void action(() => loadSchema(dataSourceId.trim()))}
            >
              Load schema
            </button>
          </div>

          {schema && (
            <>
              <p className="mapped-source-name">Mapping: {schema.title}</p>
              <PropertySelect
                label="Title"
                required
                properties={schema.properties.filter(
                  (candidate) => candidate.type === "title",
                )}
                value={titleId}
                onChange={setTitleId}
              />
              <PropertySelect
                label="Assignee"
                properties={schema.properties.filter(
                  (candidate) => candidate.type === "people",
                )}
                value={assigneeId}
                onChange={(value) => {
                  setAssigneeId(value);
                  if (!value) setOnlyMe(false);
                }}
              />
              <PropertySelect
                label="Status"
                properties={schema.properties.filter(
                  (candidate) => candidate.type === "status",
                )}
                value={statusId}
                onChange={(value) => {
                  setStatusId(value);
                  const next = schema.properties.find(
                    (candidate) => candidate.id === value,
                  )?.status;
                  setActiveStatusIds(
                    next?.groups
                      .slice(0, 2)
                      .flatMap((group) => group.optionIds) ?? [],
                  );
                }}
              />
              <PropertySelect
                label="Due Date"
                properties={schema.properties.filter(
                  (candidate) => candidate.type === "date",
                )}
                value={dueId}
                onChange={setDueId}
              />
              {assigneeId && (
                <label className="check-row">
                  <input
                    type="checkbox"
                    checked={onlyMe}
                    onChange={(event) => setOnlyMe(event.target.checked)}
                  />
                  Only tasks assigned to me
                </label>
              )}
              {statusSchema && (
                <fieldset>
                  <legend>Active statuses</legend>
                  {statusSchema.groups.map((group) => (
                    <div key={group.id} className="status-group">
                      <strong>{group.name}</strong>
                      {group.optionIds.map((id) => {
                        const option = statusSchema.options.find(
                          (candidate) => candidate.id === id,
                        );
                        return option ? (
                          <label key={id} className="check-row">
                            <input
                              type="checkbox"
                              checked={activeStatusIds.includes(id)}
                              onChange={() => toggleStatus(id)}
                            />
                            {option.name}
                          </label>
                        ) : null;
                      })}
                    </div>
                  ))}
                </fieldset>
              )}
              <div className="source-actions">
                <button
                  disabled={working || settings() === null}
                  onClick={() =>
                    void action(async () => {
                      const value = settings();
                      if (value) {
                        setPreview(
                          await glanceletApi.previewNotionSource(
                            connection.connectionId,
                            value,
                          ),
                        );
                      }
                    })
                  }
                >
                  Preview
                </button>
                <button
                  disabled={working || settings() === null}
                  onClick={() =>
                    void action(async () => {
                      const value = settings();
                      if (!value) return;
                      await glanceletApi.saveNotionSource(
                        connection.connectionId,
                        editingSourceId,
                        value,
                      );
                      setSchema(null);
                      setEditingSourceId(null);
                      setPreview([]);
                      await refresh();
                    })
                  }
                >
                  {editingSourceId ? "Save changes" : "Add Source"}
                </button>
              </div>
              {preview.length > 0 && (
                <div className="notion-preview">
                  <strong>
                    {preview.length} matching tasks (up to 10 shown)
                  </strong>
                  <ul>
                    {preview.map((row) => (
                      <li key={row.externalId}>{row.title}</li>
                    ))}
                  </ul>
                </div>
              )}
            </>
          )}
        </div>
      )}
      <div className="source-actions">
        <button
          className="danger"
          disabled={working || connection.status === "disconnected"}
          onClick={() =>
            void action(async () => {
              await glanceletApi.disconnectNotion(connection.connectionId);
              await refresh();
            })
          }
        >
          Disconnect Notion
        </button>
      </div>
    </article>
  );
}

function PropertySelect({
  label,
  required = false,
  properties,
  value,
  onChange,
}: {
  label: string;
  required?: boolean;
  properties: NotionDataSource["properties"];
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <label>
      {label}
      <select
        aria-label={label}
        required={required}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      >
        {!required && <option value="">None</option>}
        {required && properties.length === 0 && (
          <option value="">No compatible property</option>
        )}
        {properties.map((property) => (
          <option key={property.id} value={property.id}>
            {property.name}
          </option>
        ))}
      </select>
    </label>
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
            onClick={() => {
              const until = new Date(Date.now() + 60 * 60 * 1000).toISOString();
              void run(work.id, { type: "snooze", until });
            }}
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
