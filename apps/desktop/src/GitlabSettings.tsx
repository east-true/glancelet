import { useEffect, useRef, useState } from "react";
import {
  glanceletApi,
  syncReportMessage,
  type GitlabConnection,
  type GitlabDeviceAuthorization,
} from "./api";

export function GitlabSettings({
  busy,
  connections,
  refresh,
  refreshWork,
  setError,
}: {
  busy: boolean;
  connections: GitlabConnection[];
  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
  setError: (value: string | null) => void;
}) {
  const [authorization, setAuthorization] =
    useState<GitlabDeviceAuthorization | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [instanceUrl, setInstanceUrl] = useState("https://gitlab.example.com");
  const [token, setToken] = useState("");
  const activeSession = useRef<string | null>(null);

  useEffect(
    () => () => {
      const sessionId = activeSession.current;
      activeSession.current = null;
      if (sessionId) void glanceletApi.cancelGitlabConnection(sessionId);
    },
    [],
  );

  async function connectGitlabCom() {
    if (connecting) return;
    setConnecting(true);
    try {
      setError(null);
      const challenge = await glanceletApi.startGitlabConnection();
      activeSession.current = challenge.sessionId;
      setAuthorization(challenge);
      void pollAuthorization(challenge);
    } catch (reason) {
      setConnecting(false);
      setError(String(reason));
    }
  }

  async function pollAuthorization(challenge: GitlabDeviceAuthorization) {
    let delaySeconds = challenge.retryAfterSeconds;
    try {
      while (activeSession.current === challenge.sessionId) {
        await new Promise((resolve) =>
          window.setTimeout(resolve, delaySeconds * 1_000),
        );
        if (activeSession.current !== challenge.sessionId) return;
        const result = await glanceletApi.pollGitlabConnection(
          challenge.sessionId,
        );
        if (result.status === "authorized") {
          activeSession.current = null;
          setAuthorization(null);
          await refresh();
          return;
        }
        delaySeconds = result.retryAfterSeconds ?? delaySeconds;
      }
    } catch (reason) {
      if (activeSession.current === challenge.sessionId) {
        activeSession.current = null;
        setAuthorization(null);
        setError(String(reason));
      }
    } finally {
      if (activeSession.current === null) setConnecting(false);
    }
  }

  function cancel() {
    const sessionId = activeSession.current;
    activeSession.current = null;
    setAuthorization(null);
    setConnecting(false);
    if (sessionId) void glanceletApi.cancelGitlabConnection(sessionId);
  }

  async function connectPat() {
    if (connecting) return;
    setConnecting(true);
    try {
      setError(null);
      await glanceletApi.connectGitlabPat(instanceUrl, token);
      setToken("");
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setConnecting(false);
    }
  }

  return (
    <section className="source-settings" aria-label="GitLab sources">
      <div className="source-heading">
        <div>
          <h2>GitLab</h2>
          <p>Pending GitLab To-Dos from GitLab.com or self-managed GitLab.</p>
        </div>
        <button
          disabled={busy || connecting}
          onClick={() => void connectGitlabCom()}
        >
          Connect GitLab.com
        </button>
      </div>

      {authorization && (
        <div className="source-card github-device-code" aria-live="polite">
          <strong>Enter this code on GitLab</strong>
          <code>{authorization.userCode}</code>
          <span>
            {authorization.verificationUriComplete ??
              authorization.verificationUri}
          </span>
          <button onClick={cancel}>Cancel</button>
        </div>
      )}

      <div className="source-card">
        <strong>Self-managed GitLab</strong>
        <label>
          Instance URL
          <input
            aria-label="GitLab instance URL"
            value={instanceUrl}
            onChange={(event) => setInstanceUrl(event.target.value)}
          />
        </label>
        <label>
          Personal Access Token
          <input
            aria-label="GitLab Personal Access Token"
            type="password"
            autoComplete="off"
            value={token}
            onChange={(event) => setToken(event.target.value)}
          />
        </label>
        <button
          disabled={busy || connecting || !instanceUrl.trim() || !token.trim()}
          onClick={() => void connectPat()}
        >
          Connect self-managed
        </button>
        <small>Requires an HTTPS instance and a PAT with read_api scope.</small>
      </div>

      {connections.length === 0 ? (
        <div className="empty-source">No GitLab account connected.</div>
      ) : (
        connections.map((connection) => (
          <GitlabConnectionCard
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

function GitlabConnectionCard({
  connection,
  refresh,
  refreshWork,
  setError,
}: {
  connection: GitlabConnection;
  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
  setError: (value: string | null) => void;
}) {
  const [working, setWorking] = useState(false);

  async function act(operation: () => Promise<unknown>, work = false) {
    setWorking(true);
    try {
      setError(null);
      await operation();
      await refresh(false);
      if (work) await refreshWork(false);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  }

  async function syncSource(sourceId: string) {
    await act(async () => {
      const report = await glanceletApi.syncSource(sourceId);
      setError(syncReportMessage(report));
    }, true);
  }

  return (
    <article className="connection-card">
      <div className="connection-title">
        <div>
          <strong>@{connection.username}</strong>
          <span>
            {connection.instanceLabel} · {connection.authMode.toUpperCase()} ·{" "}
            {connection.status.replaceAll("_", " ")}
          </span>
        </div>
      </div>
      {!connection.source ? (
        <button
          disabled={working || connection.status === "disconnected"}
          onClick={() =>
            void act(() =>
              glanceletApi.saveGitlabTodosSource(connection.connectionId),
            )
          }
        >
          Add GitLab To-Dos
        </button>
      ) : (
        <div className="source-row">
          <div>
            <strong>{connection.source.name}</strong>
            <span>
              {connection.source.lastError ??
                (connection.source.enabled ? "Enabled" : "Disabled")}
            </span>
            {connection.source.lastSync && (
              <span>Last sync: {connection.source.lastSync}</span>
            )}
          </div>
          <div className="source-actions">
            <button
              disabled={working || !connection.source.enabled}
              onClick={() => void syncSource(connection.source!.sourceId)}
            >
              Sync now
            </button>
            <button
              disabled={working}
              onClick={() =>
                void act(() =>
                  glanceletApi.updateGitlabSource(
                    connection.source!.sourceId,
                    !connection.source!.enabled,
                  ),
                )
              }
            >
              {connection.source.enabled ? "Disable" : "Enable"}
            </button>
            <button
              disabled={working}
              onClick={() =>
                void act(
                  () =>
                    glanceletApi.removeGitlabSource(
                      connection.source!.sourceId,
                    ),
                  true,
                )
              }
            >
              Remove
            </button>
          </div>
        </div>
      )}
      <button
        className="disconnect-button"
        disabled={working || connection.status === "disconnected"}
        onClick={() =>
          void act(
            () => glanceletApi.disconnectGitlab(connection.connectionId),
            true,
          )
        }
      >
        Disconnect GitLab
      </button>
    </article>
  );
}
