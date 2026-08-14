import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { GithubSettings } from "./GithubSettings";
import { GitlabSettings } from "./GitlabSettings";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

beforeEach(() => mocks.invoke.mockReset());
afterEach(cleanup);

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

test("cancels a GitHub device session that starts after unmount", async () => {
  const start = deferred<{
    sessionId: string;
    userCode: string;
    verificationUri: string;
    retryAfterSeconds: number;
  }>();
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "start_github_connection") return start.promise;
    return Promise.resolve(undefined);
  });

  const view = render(
    <GithubSettings
      busy={false}
      connections={[]}
      refresh={vi.fn().mockResolvedValue(undefined)}
      refreshWork={vi.fn().mockResolvedValue(undefined)}
      setError={vi.fn()}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Connect GitHub" }));
  view.unmount();

  start.resolve({
    sessionId: "github-session",
    userCode: "ABCD-EFGH",
    verificationUri: "https://github.com/login/device",
    retryAfterSeconds: 5,
  });

  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("cancel_github_connection", {
      sessionId: "github-session",
    }),
  );
  expect(
    mocks.invoke.mock.calls.filter(
      ([command]) => command === "poll_github_connection",
    ),
  ).toHaveLength(0);
});

test("keeps a GitHub device session active after StrictMode effect replay", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "start_github_connection") {
      return Promise.resolve({
        sessionId: "github-strict-session",
        userCode: "STRICT-GH",
        verificationUri: "https://github.com/login/device",
        retryAfterSeconds: 60,
      });
    }
    return Promise.resolve(undefined);
  });

  render(
    <StrictMode>
      <GithubSettings
        busy={false}
        connections={[]}
        refresh={vi.fn().mockResolvedValue(undefined)}
        refreshWork={vi.fn().mockResolvedValue(undefined)}
        setError={vi.fn()}
      />
    </StrictMode>,
  );
  fireEvent.click(screen.getByRole("button", { name: "Connect GitHub" }));

  expect(await screen.findByText("STRICT-GH")).toBeInTheDocument();
  expect(
    mocks.invoke.mock.calls.filter(
      ([command]) => command === "cancel_github_connection",
    ),
  ).toHaveLength(0);
});

test("shows a GitHub refresh failure after device authorization succeeds", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "start_github_connection") {
      return Promise.resolve({
        sessionId: "github-refresh-session",
        userCode: "REFRESH-GH",
        verificationUri: "https://github.com/login/device",
        retryAfterSeconds: 0,
      });
    }
    if (command === "poll_github_connection") {
      return Promise.resolve({ status: "authorized", retryAfterSeconds: null });
    }
    return Promise.resolve(undefined);
  });
  const refresh = vi.fn().mockRejectedValue(new Error("GitHub refresh failed"));

  render(
    <GithubSettings
      busy={false}
      connections={[]}
      refresh={refresh}
      refreshWork={vi.fn().mockResolvedValue(undefined)}
      setError={vi.fn()}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Connect GitHub" }));

  expect(await screen.findByText("GitHub refresh failed")).toBeInTheDocument();
  expect(refresh).toHaveBeenCalledTimes(1);
  expect(screen.getByRole("button", { name: "Connect GitHub" })).toBeEnabled();
});

test("cancels a GitLab device session that starts after unmount", async () => {
  const start = deferred<{
    sessionId: string;
    userCode: string;
    verificationUri: string;
    verificationUriComplete: string | null;
    retryAfterSeconds: number;
  }>();
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "start_gitlab_connection") return start.promise;
    return Promise.resolve(undefined);
  });

  const view = render(
    <GitlabSettings
      busy={false}
      connections={[]}
      refresh={vi.fn().mockResolvedValue(undefined)}
      refreshWork={vi.fn().mockResolvedValue(undefined)}
      setError={vi.fn()}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Connect GitLab" }));
  fireEvent.click(screen.getByRole("button", { name: "Connect GitLab.com" }));
  view.unmount();

  start.resolve({
    sessionId: "gitlab-session",
    userCode: "WXYZ-1234",
    verificationUri: "https://gitlab.com/oauth/device",
    verificationUriComplete: null,
    retryAfterSeconds: 5,
  });

  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("cancel_gitlab_connection", {
      sessionId: "gitlab-session",
    }),
  );
  expect(
    mocks.invoke.mock.calls.filter(
      ([command]) => command === "poll_gitlab_connection",
    ),
  ).toHaveLength(0);
});

test("keeps a GitLab device session active after StrictMode effect replay", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "start_gitlab_connection") {
      return Promise.resolve({
        sessionId: "gitlab-strict-session",
        userCode: "STRICT-GL",
        verificationUri: "https://gitlab.com/oauth/device",
        verificationUriComplete: null,
        retryAfterSeconds: 60,
      });
    }
    return Promise.resolve(undefined);
  });

  render(
    <StrictMode>
      <GitlabSettings
        busy={false}
        connections={[]}
        refresh={vi.fn().mockResolvedValue(undefined)}
        refreshWork={vi.fn().mockResolvedValue(undefined)}
        setError={vi.fn()}
      />
    </StrictMode>,
  );
  fireEvent.click(screen.getByRole("button", { name: "Connect GitLab" }));
  fireEvent.click(screen.getByRole("button", { name: "Connect GitLab.com" }));

  expect(await screen.findByText("STRICT-GL")).toBeInTheDocument();
  expect(
    mocks.invoke.mock.calls.filter(
      ([command]) => command === "cancel_gitlab_connection",
    ),
  ).toHaveLength(0);
});

test("shows a GitLab refresh failure after device authorization succeeds", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "start_gitlab_connection") {
      return Promise.resolve({
        sessionId: "gitlab-refresh-session",
        userCode: "REFRESH-GL",
        verificationUri: "https://gitlab.com/oauth/device",
        verificationUriComplete: null,
        retryAfterSeconds: 0,
      });
    }
    if (command === "poll_gitlab_connection") {
      return Promise.resolve({ status: "authorized", retryAfterSeconds: null });
    }
    return Promise.resolve(undefined);
  });
  const refresh = vi.fn().mockRejectedValue(new Error("GitLab refresh failed"));

  render(
    <GitlabSettings
      busy={false}
      connections={[]}
      refresh={refresh}
      refreshWork={vi.fn().mockResolvedValue(undefined)}
      setError={vi.fn()}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Connect GitLab" }));
  fireEvent.click(screen.getByRole("button", { name: "Connect GitLab.com" }));

  expect(await screen.findByText("GitLab refresh failed")).toBeInTheDocument();
  expect(refresh).toHaveBeenCalledTimes(1);
  expect(screen.getByRole("button", { name: "Connect GitLab" })).toBeEnabled();
});
