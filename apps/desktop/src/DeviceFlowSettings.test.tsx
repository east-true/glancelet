import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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
