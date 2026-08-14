import { useEffect, useRef, useState } from "react";
import {
  connectionTone,
  connectionToneLabel,
  glanceletApi,
  syncReportMessage,
  type GithubConnection,
  type GithubDeviceAuthorization,
  type GithubRepository,
  type GithubSource,
} from "./api";
import { ErrorBanner } from "./ErrorBanner";
import { useDismissingError } from "./useDismissingError";

const REVIEW_REQUESTS = "github.review_requests";
const ASSIGNED_ISSUES = "github.assigned_issues";

export function GithubSettings({
  busy,
  connections,
  refresh,
  refreshWork,
  setError,
}: {
  busy: boolean;
  connections: GithubConnection[];
  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
  setError: (value: string | null) => void;
}) {
  const [authorization, setAuthorization] =
    useState<GithubDeviceAuthorization | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [connectError, setConnectError] = useDismissingError();
  const activeSession = useRef<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      const sessionId = activeSession.current;
      activeSession.current = null;
      if (sessionId) void glanceletApi.cancelGithubConnection(sessionId);
    };
  }, []);

  async function connect() {
    if (connecting) return;
    setConnecting(true);
    try {
      setConnectError(null);
      const challenge = await glanceletApi.startGithubConnection();
      if (!mounted.current) {
        void glanceletApi.cancelGithubConnection(challenge.sessionId);
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

  async function pollAuthorization(challenge: GithubDeviceAuthorization) {
    let delaySeconds = challenge.retryAfterSeconds;
    try {
      while (activeSession.current === challenge.sessionId) {
        await new Promise((resolve) =>
          window.setTimeout(resolve, delaySeconds * 1_000),
        );
        if (activeSession.current !== challenge.sessionId) return;
        const result = await glanceletApi.pollGithubConnection(
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
    if (sessionId) void glanceletApi.cancelGithubConnection(sessionId);
  }

  const tone = connectionTone(connections);

  return (
    <section className="source-settings" aria-label="GitHub sources">
      <div className="source-heading">
        <div>
          <div className="source-title">
            <span
              className={`status-dot status-dot-${tone}`}
              role="img"
              aria-label={`GitHub: ${connectionToneLabel(tone)}`}
            />
            <h2>GitHub</h2>
          </div>
          <p>Review requests, assigned issues, and workflow failures.</p>
        </div>
        <button
          className="btn-primary"
          disabled={busy || connecting}
          aria-label="Connect GitHub"
          onClick={() => void connect()}
        >
          Connect
        </button>
      </div>

      {authorization && (
        <div className="source-card github-device-code" aria-live="polite">
          <strong>Enter this code on GitHub</strong>
          <code>{authorization.userCode}</code>
          <span>{authorization.verificationUri}</span>
          <button className="btn-quiet" onClick={cancel}>
            Cancel
          </button>
        </div>
      )}

      {connections.map((connection) => (
        <GithubConnectionCard
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

function GithubConnectionCard({
  connection,
  refresh,
  refreshWork,
  setError,
}: {
  connection: GithubConnection;
  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
  setError: (value: string | null) => void;
}) {
  const [repositories, setRepositories] = useState<GithubRepository[]>([]);
  const [loading, setLoading] = useState(false);

  async function act(operation: () => Promise<unknown>, work = false) {
    setLoading(true);
    try {
      setError(null);
      await operation();
      await refresh(false);
      if (work) await refreshWork(false);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }

  async function discover() {
    setLoading(true);
    try {
      setError(null);
      setRepositories(
        await glanceletApi.githubRepositories(connection.connectionId),
      );
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  }

  async function syncSource(sourceId: string) {
    await act(async () => {
      const report = await glanceletApi.syncSource(sourceId);
      setError(syncReportMessage(report));
    }, true);
  }

  const review = connection.sources.find(
    (source) => source.sourceType === REVIEW_REQUESTS,
  );
  const issues = connection.sources.find(
    (source) => source.sourceType === ASSIGNED_ISSUES,
  );
  const workflowRepositories = new Set(
    connection.sources.flatMap((source) =>
      source.repository ? [source.repository] : [],
    ),
  );

  return (
    <article className="connection-card">
      <div className="connection-title">
        <div>
          <strong>@{connection.login}</strong>
          <span>{connection.status.replaceAll("_", " ")}</span>
        </div>
        <button disabled={loading} onClick={() => void discover()}>
          Refresh repositories
        </button>
      </div>

      <p className="source-runtime">
        Only repositories where the GitHub App is installed are visible.
      </p>

      <div className="source-actions">
        <button
          disabled={loading || Boolean(review)}
          onClick={() =>
            void act(() =>
              glanceletApi.saveGithubGlobalSource(
                connection.connectionId,
                REVIEW_REQUESTS,
              ),
            )
          }
        >
          {review ? "Review Requests added" : "Add Review Requests"}
        </button>
        <button
          disabled={loading || Boolean(issues)}
          onClick={() =>
            void act(() =>
              glanceletApi.saveGithubGlobalSource(
                connection.connectionId,
                ASSIGNED_ISSUES,
              ),
            )
          }
        >
          {issues ? "Assigned Issues added" : "Add Assigned Issues"}
        </button>
      </div>

      {connection.sources.map((source) => (
        <GithubSourceRow
          key={source.sourceId}
          source={source}
          loading={loading}
          sync={() => syncSource(source.sourceId)}
          toggle={() =>
            act(() =>
              glanceletApi.updateGithubSource(source.sourceId, !source.enabled),
            )
          }
          remove={() =>
            act(() => glanceletApi.removeGithubSource(source.sourceId), true)
          }
        />
      ))}

      {repositories.length > 0 && (
        <fieldset className="calendar-picker">
          <legend>Workflow Failures repositories</legend>
          {repositories.map((repository) => {
            const added = workflowRepositories.has(repository.fullName);
            return (
              <div className="source-row" key={repository.id}>
                <div>
                  <strong>{repository.fullName}</strong>
                  <span>Default branch: {repository.defaultBranch}</span>
                </div>
                <button
                  disabled={loading || added}
                  onClick={() =>
                    void act(() =>
                      glanceletApi.saveGithubWorkflowSource(
                        connection.connectionId,
                        repository.id,
                      ),
                    )
                  }
                >
                  {added ? "Added" : "Add"}
                </button>
              </div>
            );
          })}
        </fieldset>
      )}

      {repositories.length === 0 && (
        <p className="source-runtime">
          Refresh repositories to select workflow sources. An empty result means
          no installed repositories are available.
        </p>
      )}

      <button
        className="disconnect-button btn-danger"
        disabled={loading}
        onClick={() =>
          void act(
            () => glanceletApi.disconnectGithub(connection.connectionId),
            true,
          )
        }
      >
        Disconnect GitHub
      </button>
    </article>
  );
}

function GithubSourceRow({
  source,
  loading,
  sync,
  toggle,
  remove,
}: {
  source: GithubSource;
  loading: boolean;
  sync: () => Promise<void>;
  toggle: () => Promise<void>;
  remove: () => Promise<void>;
}) {
  return (
    <div className="source-row">
      <div>
        <strong>{source.name}</strong>
        <span>
          {source.lastError ?? (source.enabled ? "Enabled" : "Disabled")}
        </span>
        {source.lastSync && <span>Last sync: {source.lastSync}</span>}
      </div>
      <div className="source-actions">
        <button
          disabled={loading || !source.enabled}
          onClick={() => void sync()}
        >
          Sync now
        </button>
        <button disabled={loading} onClick={() => void toggle()}>
          {source.enabled ? "Disable" : "Enable"}
        </button>
        <button
          className="btn-danger"
          disabled={loading}
          onClick={() => void remove()}
        >
          Remove
        </button>
      </div>
    </div>
  );
}
