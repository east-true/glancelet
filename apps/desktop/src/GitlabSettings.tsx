import { useEffect, useRef, useState } from "react";
import {
  connectionTone,
  connectionToneLabel,
  glanceletApi,
  syncReportMessage,
  type GitlabConnection,
  type GitlabDeviceAuthorization,
} from "./api";
import { ErrorBanner } from "./ErrorBanner";
import { Modal } from "./Modal";
import { useDismissingError } from "./useDismissingError";

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
  const [modalOpen, setModalOpen] = useState(false);
  const [mode, setMode] = useState<"cloud" | "self-managed">("cloud");
  const [selfManagedError, setSelfManagedError] = useDismissingError();
  const [connectError, setConnectError] = useDismissingError();
  const activeSession = useRef<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      const sessionId = activeSession.current;
      activeSession.current = null;
      if (sessionId) void glanceletApi.cancelGitlabConnection(sessionId);
    };
  }, []);

  async function connectGitlabCom() {
    if (connecting) return;
    setConnecting(true);
    try {
      setConnectError(null);
      const challenge = await glanceletApi.startGitlabConnection();
      if (!mounted.current) {
        void glanceletApi.cancelGitlabConnection(challenge.sessionId);
        return;
      }
      activeSession.current = challenge.sessionId;
      setAuthorization(challenge);
      void pollAuthorization(challenge);
    } catch (reason) {
      if (!mounted.current) return;
      setConnecting(false);
      setConnectError(String(reason));
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
        if (activeSession.current !== challenge.sessionId) return;
        if (result.status === "authorized") {
          activeSession.current = null;
          setAuthorization(null);
          try {
            await refresh();
          } catch (reason) {
            if (mounted.current) setConnectError(String(reason));
          }
          return;
        }
        delaySeconds = result.retryAfterSeconds ?? delaySeconds;
      }
    } catch (reason) {
      if (activeSession.current === challenge.sessionId) {
        activeSession.current = null;
        setAuthorization(null);
        setConnectError(String(reason));
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

  function openModal() {
    setMode("cloud");
    setToken("");
    setSelfManagedError(null);
    setModalOpen(true);
  }

  function closeModal() {
    setToken("");
    setModalOpen(false);
  }

  async function connectPat() {
    if (connecting) return;
    setConnecting(true);
    try {
      setSelfManagedError(null);
      await glanceletApi.connectGitlabPat(instanceUrl, token);
      setToken("");
      setModalOpen(false);
      try {
        await refresh();
      } catch (reason) {
        setConnectError(String(reason));
      }
    } catch (reason) {
      setSelfManagedError(String(reason));
    } finally {
      setConnecting(false);
    }
  }

  const tone = connectionTone(connections);

  return (
    <section className="source-settings" aria-label="GitLab sources">
      <div className="source-heading">
        <div>
          <div className="source-title">
            <span
              className={`status-dot status-dot-${tone}`}
              role="img"
              aria-label={`GitLab: ${connectionToneLabel(tone)}`}
            />
            <h2>GitLab</h2>
          </div>
          <p>Pending GitLab To-Dos from GitLab.com or self-managed GitLab.</p>
        </div>
        <button
          className="btn-primary"
          disabled={busy || connecting}
          aria-label="Connect GitLab"
          onClick={openModal}
        >
          Connect
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
          <button className="btn-quiet" onClick={cancel}>
            Cancel
          </button>
        </div>
      )}

      <Modal open={modalOpen} title="Connect GitLab" onClose={closeModal}>
        <div className="modal-form">
          <div
            className="mode-toggle"
            role="tablist"
            aria-label="GitLab connection method"
          >
            <button
              type="button"
              role="tab"
              aria-selected={mode === "cloud"}
              className={mode === "cloud" ? "active" : ""}
              onClick={() => setMode("cloud")}
            >
              GitLab.com
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={mode === "self-managed"}
              className={mode === "self-managed" ? "active" : ""}
              onClick={() => setMode("self-managed")}
            >
              Self-managed
            </button>
          </div>

          {mode === "cloud" ? (
            <>
              <p className="modal-help">
                Sign in with your GitLab.com account through a device code.
              </p>
              <div className="modal-actions">
                <button
                  type="button"
                  className="btn-quiet"
                  onClick={closeModal}
                >
                  Cancel
                </button>
                <button
                  type="button"
                  className="btn-primary"
                  aria-label="Connect GitLab.com"
                  disabled={busy || connecting}
                  onClick={() => {
                    setModalOpen(false);
                    void connectGitlabCom();
                  }}
                >
                  Connect
                </button>
              </div>
            </>
          ) : (
            <form
              className="modal-form"
              onSubmit={(event) => {
                event.preventDefault();
                void connectPat();
              }}
            >
              <label className="modal-field">
                <span>
                  Instance URL<span className="required-mark">*</span>
                </span>
                <input
                  aria-label="GitLab instance URL"
                  required
                  value={instanceUrl}
                  onChange={(event) => setInstanceUrl(event.target.value)}
                />
              </label>
              <label className="modal-field">
                <span>
                  Personal Access Token<span className="required-mark">*</span>
                </span>
                <input
                  aria-label="GitLab Personal Access Token"
                  type="password"
                  autoComplete="off"
                  required
                  value={token}
                  onChange={(event) => setToken(event.target.value)}
                />
              </label>
              <small>
                Requires an HTTPS instance and a PAT with read_api scope.
              </small>
              <ErrorBanner message={selfManagedError} />
              <div className="modal-actions">
                <button
                  type="button"
                  className="btn-quiet"
                  onClick={closeModal}
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  className="btn-primary"
                  aria-label="Connect self-managed GitLab"
                  disabled={connecting || !instanceUrl.trim() || !token.trim()}
                >
                  {connecting ? "Connecting…" : "Connect"}
                </button>
              </div>
            </form>
          )}
        </div>
      </Modal>

      {connections.map((connection) => (
        <GitlabConnectionCard
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
          className="btn-primary"
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
              className="btn-danger"
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
        className="disconnect-button btn-danger"
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
