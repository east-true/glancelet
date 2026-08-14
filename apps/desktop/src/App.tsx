import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  connectionTone,
  connectionToneLabel,
  glanceletApi,
  syncReportMessage,
  type GoogleConnection,
  type GitlabConnection,
  type GithubConnection,
  type NotionConnection,
  type NotionDataSource,
  type NotionDataSourceSummary,
  type NotionPreviewRow,
  type NotionPropertyMapping,
  type NotionSource,
  type NotionSourceSettings,
  type SlackConnection,
  type DesktopSettings,
  type WidgetInstance,
  type WorkCommand,
  type WorkDashboard,
} from "./api";
import { DesktopSurface } from "./DesktopSurface";
import { ErrorBanner } from "./ErrorBanner";
import { GoogleSettings } from "./GoogleSettings";
import { GithubSettings } from "./GithubSettings";
import { GitlabSettings } from "./GitlabSettings";
import { Modal } from "./Modal";
import { SettingsOverlay, type SettingsSection } from "./SettingsOverlay";
import { useDismissingError } from "./useDismissingError";
import "./styles.css";

const emptyDashboard: WorkDashboard = {
  today: [],
  inbox: [],
  upcoming: [],
  attention: [],
  sourceHealth: { sourceCount: 0, issues: [] },
};
const defaultLayout: WidgetInstance[] = [
  { widgetType: "today", position: 0, size: "wide", settings: {} },
  { widgetType: "inbox", position: 1, size: "compact", settings: {} },
  { widgetType: "attention", position: 2, size: "compact", settings: {} },
];
const DASHBOARD_TIME_REFRESH_MS = 60_000;
type Tab = "surface" | "sources" | "settings";

function normalizedDashboard(value: WorkDashboard | undefined): WorkDashboard {
  const today = value?.today ?? [];
  const inbox = value?.inbox ?? [];
  const attention = value?.attention ?? [];
  return {
    today,
    inbox,
    upcoming: value?.upcoming ?? [],
    attention,
    sourceHealth: value?.sourceHealth ?? {
      sourceCount: today.length + inbox.length + attention.length > 0 ? 1 : 0,
      issues: [],
    },
  };
}

export default function App() {
  const [dashboard, setDashboard] = useState(emptyDashboard);
  const [tab, setTab] = useState<Tab>("surface");
  const [layout, setLayout] = useState<WidgetInstance[]>(defaultLayout);
  const [editingLayout, setEditingLayout] = useState(false);
  const [desktopSettings, setDesktopSettings] = useState<DesktopSettings>({
    alwaysOnTop: false,
    launchAtStartup: false,
  });
  const [pendingDesktopSettings, setPendingDesktopSettings] = useState<
    Set<keyof DesktopSettings>
  >(() => new Set());
  const pendingDesktopSettingsRef = useRef(new Set<keyof DesktopSettings>());
  const [slackConnections, setSlackConnections] = useState<SlackConnection[]>(
    [],
  );
  const [notionConnections, setNotionConnections] = useState<
    NotionConnection[]
  >([]);
  const [googleConnections, setGoogleConnections] = useState<
    GoogleConnection[]
  >([]);
  const [githubConnections, setGithubConnections] = useState<
    GithubConnection[]
  >([]);
  const [gitlabConnections, setGitlabConnections] = useState<
    GitlabConnection[]
  >([]);
  const [initialLoading, setInitialLoading] = useState(true);
  const [syncing, setSyncing] = useState(false);
  const [connectingSlack, setConnectingSlack] = useState(false);
  const [slackConnectError, setSlackConnectError] = useDismissingError();
  const [pendingWorkIds, setPendingWorkIds] = useState<Set<string>>(
    () => new Set(),
  );
  const pendingWorkIdsRef = useRef(new Set<string>());
  const [error, setError] = useState<string | null>(null);
  const dashboardRequest = useRef(0);
  const tabRef = useRef<Tab>("surface");

  const refresh = useCallback(async (clearError = true) => {
    const request = ++dashboardRequest.current;
    try {
      if (clearError) setError(null);
      const next = await glanceletApi.dashboard();
      if (request === dashboardRequest.current)
        setDashboard(normalizedDashboard(next));
    } catch (reason) {
      if (request === dashboardRequest.current) setError(String(reason));
    } finally {
      if (request === dashboardRequest.current) setInitialLoading(false);
    }
  }, []);

  const refreshSlack = useCallback(async (clearError = true) => {
    try {
      if (clearError) setError(null);
      setSlackConnections((await glanceletApi.slackConnections()) ?? []);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  const refreshNotion = useCallback(async (clearError = true) => {
    try {
      if (clearError) setError(null);
      setNotionConnections((await glanceletApi.notionConnections()) ?? []);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  const refreshGoogle = useCallback(async (clearError = true) => {
    try {
      if (clearError) setError(null);
      setGoogleConnections((await glanceletApi.googleConnections()) ?? []);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  const refreshGithub = useCallback(async (clearError = true) => {
    try {
      if (clearError) setError(null);
      setGithubConnections((await glanceletApi.githubConnections()) ?? []);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  const refreshGitlab = useCallback(async (clearError = true) => {
    try {
      if (clearError) setError(null);
      setGitlabConnections((await glanceletApi.gitlabConnections()) ?? []);
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  const refreshSources = useCallback(
    async (clearError = true) => {
      await Promise.all([
        refreshSlack(clearError),
        refreshNotion(clearError),
        refreshGoogle(clearError),
        refreshGithub(clearError),
        refreshGitlab(clearError),
      ]);
    },
    [refreshGithub, refreshGitlab, refreshGoogle, refreshNotion, refreshSlack],
  );

  useEffect(() => {
    tabRef.current = tab;
  }, [tab]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen("glancelet://work-changed", () => {
      void refresh(false);
      if (tabRef.current === "sources") void refreshSources(false);
    }).then((dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
    });
    const timer = window.setInterval(
      () => void refresh(false),
      DASHBOARD_TIME_REFRESH_MS,
    );
    const initialRefresh = window.setTimeout(() => {
      void refresh();
      void Promise.resolve(glanceletApi.widgetLayout())
        .then((widgets) =>
          setLayout(
            Array.isArray(widgets) && widgets.length > 0
              ? widgets
              : defaultLayout,
          ),
        )
        .catch((reason) => setError(String(reason)));
    }, 0);
    return () => {
      disposed = true;
      dashboardRequest.current += 1;
      unlisten?.();
      window.clearTimeout(initialRefresh);
      window.clearInterval(timer);
    };
  }, [refresh, refreshSources]);

  useEffect(() => {
    let disposed = false;
    let unlistenNavigation: (() => void) | undefined;
    let unlistenSettings: (() => void) | undefined;
    void listen<string>("glancelet://navigate", (event) => {
      if (event.payload === "sources" || event.payload === "settings") {
        tabRef.current = event.payload;
        setTab(event.payload);
        if (event.payload === "sources") void refreshSources();
        else {
          void glanceletApi.desktopSettings().then((settings) => {
            if (settings) setDesktopSettings(settings);
          });
        }
      }
    }).then((dispose) => {
      if (disposed) dispose();
      else unlistenNavigation = dispose;
    });
    void listen("glancelet://desktop-settings-changed", () => {
      if (tabRef.current === "settings") {
        void glanceletApi.desktopSettings().then((settings) => {
          if (settings) setDesktopSettings(settings);
        });
      }
    }).then((dispose) => {
      if (disposed) dispose();
      else unlistenSettings = dispose;
    });
    return () => {
      disposed = true;
      unlistenNavigation?.();
      unlistenSettings?.();
    };
  }, [refreshSources]);

  async function sync() {
    if (syncing) return;
    setSyncing(true);
    try {
      setError(null);
      const report = await glanceletApi.sync();
      setError(syncReportMessage(report));
      await Promise.all([
        refresh(false),
        tabRef.current === "sources"
          ? refreshSources(false)
          : Promise.resolve(),
      ]);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSyncing(false);
    }
  }

  async function selectTab(next: Tab) {
    tabRef.current = next;
    setTab(next);
    if (next === "sources") await refreshSources();
    if (next === "settings") await refreshDesktopSettings();
  }

  async function refreshDesktopSettings() {
    try {
      const settings = await glanceletApi.desktopSettings();
      if (settings) setDesktopSettings(settings);
    } catch (reason) {
      setError(String(reason));
    }
  }

  async function saveLayout(next: WidgetInstance[]) {
    const previous = layout;
    setLayout(next);
    try {
      setError(null);
      await glanceletApi.saveWidgetLayout(next);
    } catch (reason) {
      setLayout(previous);
      setError(String(reason));
    }
  }

  async function updateDesktopSetting(
    key: keyof DesktopSettings,
    enabled: boolean,
  ) {
    if (pendingDesktopSettingsRef.current.has(key)) return;
    const previous = desktopSettings[key];
    pendingDesktopSettingsRef.current.add(key);
    setPendingDesktopSettings(new Set(pendingDesktopSettingsRef.current));
    setDesktopSettings((current) => ({ ...current, [key]: enabled }));
    try {
      setError(null);
      if (key === "alwaysOnTop") await glanceletApi.setAlwaysOnTop(enabled);
      else await glanceletApi.setLaunchAtStartup(enabled);
    } catch (reason) {
      setDesktopSettings((current) => ({ ...current, [key]: previous }));
      setError(String(reason));
    } finally {
      pendingDesktopSettingsRef.current.delete(key);
      setPendingDesktopSettings(new Set(pendingDesktopSettingsRef.current));
    }
  }

  async function connectSlack() {
    if (connectingSlack) return;
    setConnectingSlack(true);
    try {
      setSlackConnectError(null);
      await glanceletApi.connectSlack();
      await Promise.all([refreshSlack(false), refresh(false)]);
    } catch (reason) {
      setSlackConnectError(String(reason));
    } finally {
      setConnectingSlack(false);
    }
  }

  async function run(workId: string, command: WorkCommand) {
    if (pendingWorkIdsRef.current.has(workId)) return;
    pendingWorkIdsRef.current.add(workId);
    setPendingWorkIds(new Set(pendingWorkIdsRef.current));
    try {
      setError(null);
      await glanceletApi.command(workId, command);
      await refresh(false);
    } catch (reason) {
      setError(String(reason));
    } finally {
      pendingWorkIdsRef.current.delete(workId);
      setPendingWorkIds(new Set(pendingWorkIdsRef.current));
    }
  }

  async function open(workId: string) {
    try {
      setError(null);
      await glanceletApi.openSource(workId);
    } catch (reason) {
      setError(String(reason));
    }
  }

  const globalBusy = initialLoading || syncing;
  const syncLabel = initialLoading ? "Loading…" : syncing ? "Syncing…" : "Sync";

  return (
    <main className="app-shell">
      <div className="toolbar-row">
        <button
          type="button"
          className="icon-button btn-primary"
          aria-label={editingLayout ? "Done editing layout" : "Edit layout"}
          aria-pressed={editingLayout}
          title={editingLayout ? "Done editing layout" : "Edit layout"}
          onClick={() => setEditingLayout(!editingLayout)}
        >
          <svg
            viewBox="0 0 24 24"
            width="19"
            height="19"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M17 3a2.828 2.828 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5L17 3z" />
          </svg>
        </button>
        <button
          className="icon-button sync-button btn-primary"
          data-busy={globalBusy}
          disabled={globalBusy}
          aria-label={syncLabel}
          title={syncLabel}
          onClick={() => void sync()}
        >
          <svg
            viewBox="0 0 24 24"
            width="19"
            height="19"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M23 4v6h-6" />
            <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10" />
          </svg>
        </button>
        <button
          type="button"
          className="icon-button btn-primary"
          aria-label="Open settings"
          onClick={() => void selectTab("sources")}
        >
          <svg
            viewBox="0 0 24 24"
            width="19"
            height="19"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </div>

      {tab === "surface" && <ErrorBanner message={error} />}

      <DesktopSurface
        data={dashboard}
        layout={layout}
        loading={initialLoading}
        editing={editingLayout}
        pendingWorkIds={pendingWorkIds}
        onEdit={setEditingLayout}
        onLayout={saveLayout}
        onRun={run}
        onOpen={open}
        onSources={() => void selectTab("sources")}
      />

      {tab !== "surface" && (
        <SettingsOverlay
          section={tab}
          onSection={(next: SettingsSection) => void selectTab(next)}
          onClose={() => void selectTab("surface")}
        >
          <ErrorBanner message={error} />
          {tab === "sources" ? (
            <div className="settings-stack">
              <SlackSettings
                busy={globalBusy || connectingSlack}
                connections={slackConnections}
                connect={connectSlack}
                connectError={slackConnectError}
                refresh={refreshSlack}
                refreshWork={refresh}
                setError={setError}
              />
              <NotionSettings
                busy={globalBusy}
                connections={notionConnections}
                refresh={refreshNotion}
                refreshWork={refresh}
                setError={setError}
              />
              <GoogleSettings
                busy={globalBusy}
                connections={googleConnections}
                refresh={refreshGoogle}
                refreshWork={refresh}
                setError={setError}
              />
              <GithubSettings
                busy={globalBusy}
                connections={githubConnections}
                refresh={refreshGithub}
                refreshWork={refresh}
                setError={setError}
              />
              <GitlabSettings
                busy={globalBusy}
                connections={gitlabConnections}
                refresh={refreshGitlab}
                refreshWork={refresh}
                setError={setError}
              />
            </div>
          ) : (
            <GeneralSettings
              settings={desktopSettings}
              pending={pendingDesktopSettings}
              update={updateDesktopSetting}
            />
          )}
        </SettingsOverlay>
      )}
    </main>
  );
}

function GeneralSettings({
  settings,
  pending,
  update,
}: {
  settings: DesktopSettings;
  pending: Set<keyof DesktopSettings>;
  update: (key: keyof DesktopSettings, enabled: boolean) => Promise<void>;
}) {
  return (
    <section className="general-settings" aria-label="General settings">
      <div>
        <h2>Desktop</h2>
        <p>Control how Glancelet stays available throughout your day.</p>
      </div>
      <label>
        <span>
          <strong>Always on Top</strong>
          <small>Keep the Surface above other windows.</small>
        </span>
        <input
          type="checkbox"
          checked={settings.alwaysOnTop}
          disabled={pending.has("alwaysOnTop")}
          onChange={(event) => void update("alwaysOnTop", event.target.checked)}
        />
      </label>
      <label>
        <span>
          <strong>Launch Glancelet at startup</strong>
          <small>Opt in to opening Glancelet when you sign in.</small>
        </span>
        <input
          type="checkbox"
          checked={settings.launchAtStartup}
          disabled={pending.has("launchAtStartup")}
          onChange={(event) =>
            void update("launchAtStartup", event.target.checked)
          }
        />
      </label>
      <p className="tray-hint">
        Closing the window hides Glancelet to the system tray. Use Quit from the
        tray menu to exit.
      </p>
    </section>
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
  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
  setError: (error: string | null) => void;
}) {
  const [token, setToken] = useState("");
  const [working, setWorking] = useState(false);
  const [modalOpen, setModalOpen] = useState(false);
  const [modalError, setModalError] = useDismissingError();

  function openModal() {
    setToken("");
    setModalError(null);
    setModalOpen(true);
  }

  async function connect() {
    setWorking(true);
    try {
      setModalError(null);
      await glanceletApi.connectNotion(token);
      setToken("");
      setModalOpen(false);
      await refresh();
    } catch (reason) {
      setModalError(String(reason));
    } finally {
      setWorking(false);
    }
  }

  const tone = connectionTone(connections);

  return (
    <section className="source-settings" aria-label="Notion sources">
      <div className="settings-heading">
        <div>
          <div className="source-title">
            <span
              className={`status-dot status-dot-${tone}`}
              role="img"
              aria-label={`Notion: ${connectionToneLabel(tone)}`}
            />
            <h2>Notion</h2>
          </div>
          <p>Mirror tasks from a mapped Notion data source.</p>
        </div>
        <button
          className="btn-primary"
          disabled={busy}
          aria-label="Connect Notion"
          onClick={openModal}
        >
          Connect
        </button>
      </div>
      <Modal
        open={modalOpen}
        title="Connect Notion"
        onClose={() => setModalOpen(false)}
      >
        <form
          className="modal-form"
          onSubmit={(event) => {
            event.preventDefault();
            void connect();
          }}
        >
          <label className="modal-field">
            <span>
              Personal Access Token<span className="required-mark">*</span>
            </span>
            <input
              aria-label="Notion Personal Access Token"
              type="password"
              autoComplete="off"
              required
              placeholder="secret_…"
              value={token}
              onChange={(event) => setToken(event.target.value)}
            />
          </label>
          <ErrorBanner message={modalError} />
          <div className="modal-actions">
            <button
              type="button"
              className="btn-quiet"
              onClick={() => setModalOpen(false)}
            >
              Cancel
            </button>
            <button
              type="submit"
              className="btn-primary"
              disabled={working || token.trim() === ""}
            >
              {working ? "Connecting…" : "Connect"}
            </button>
          </div>
        </form>
      </Modal>
      {connections.map((connection) => (
        <NotionConnectionCard
          key={connection.connectionId}
          connection={connection}
          refresh={refresh}
          refreshWork={refreshWork}
          setError={setError}
        />
      ))}
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
  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
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
    <article className="connection-card notion-card">
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
                  const report = await glanceletApi.syncSource(source.sourceId);
                  setError(syncReportMessage(report));
                  await Promise.all([refresh(false), refreshWork(false)]);
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
              className="btn-danger"
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
                  className="btn-primary"
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
      <button
        className="disconnect-button btn-danger"
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

function SlackSettings({
  busy,
  connections,
  connect,
  connectError,
  refresh,
  refreshWork,
  setError,
}: {
  busy: boolean;
  connections: SlackConnection[];
  connect: () => Promise<void>;
  connectError: string | null;
  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
  setError: (error: string | null) => void;
}) {
  const tone = connectionTone(connections);

  return (
    <section className="source-settings" aria-label="Sources">
      <div className="settings-heading">
        <div>
          <div className="source-title">
            <span
              className={`status-dot status-dot-${tone}`}
              role="img"
              aria-label={`Slack: ${connectionToneLabel(tone)}`}
            />
            <h2>Slack</h2>
          </div>
          <p>Capture messages you react to with a configured emoji.</p>
        </div>
        <button
          className="btn-primary"
          disabled={busy}
          aria-label="Connect Slack"
          onClick={() => void connect()}
        >
          Connect
        </button>
      </div>
      {connections.map((connection) => (
        <SlackConnectionCard
          key={connection.connectionId}
          connection={connection}
          refresh={refresh}
          refreshWork={refreshWork}
          setError={setError}
        />
      ))}
      <ErrorBanner message={connectError} />
    </section>
  );
}

function SlackConnectionCard({
  connection,
  refresh,
  refreshWork,
  setError,
}: {
  connection: SlackConnection;
  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
  setError: (error: string | null) => void;
}) {
  const [reaction, setReaction] = useState(connection.reactionName);
  const [working, setWorking] = useState(false);

  async function action(task: () => Promise<void>) {
    setWorking(true);
    try {
      setError(null);
      await task();
      await refresh(false);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  }

  async function syncSource(sourceId: string) {
    setWorking(true);
    try {
      setError(null);
      const report = await glanceletApi.syncSource(sourceId);
      setError(syncReportMessage(report));
      await Promise.all([refresh(false), refreshWork(false)]);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  }

  return (
    <article className="connection-card">
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
              onClick={() => void syncSource(connection.sourceId as string)}
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
      </div>
      <button
        className="disconnect-button btn-danger"
        disabled={working || connection.status === "disconnected"}
        onClick={() =>
          void action(() =>
            glanceletApi.disconnectSlack(connection.connectionId),
          )
        }
      >
        Disconnect Slack
      </button>
    </article>
  );
}
