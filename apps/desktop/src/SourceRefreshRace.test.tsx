import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import App from "./App";
import type { SlackConnection } from "./api";

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));

beforeEach(() => {
  mocks.invoke.mockReset();
  mocks.listen.mockReset();
});
afterEach(cleanup);

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

function slack(enabled: boolean): SlackConnection[] {
  return [
    {
      connectionId: "slack-1",
      sourceId: "slack-source",
      workspace: "Example workspace",
      user: "Tester",
      reactionName: "todo",
      enabled,
      status: "connected",
      lastSync: null,
      lastError: null,
    },
  ];
}

test("ignores a stale provider refresh after a newer source update", async () => {
  const staleRefresh = deferred<SlackConnection[]>();
  let invalidate: (() => void) | undefined;
  let slackReads = 0;

  mocks.listen.mockImplementation(
    async (event: string, handler: () => void) => {
      if (event === "glancelet://work-changed") invalidate = handler;
      return () => undefined;
    },
  );
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard") {
      return Promise.resolve({ today: [], inbox: [], upcoming: [], attention: [] });
    }
    if (command === "widget_layout") return Promise.resolve(undefined);
    if (command === "slack_connections") {
      slackReads += 1;
      if (slackReads === 1) return Promise.resolve(slack(true));
      if (slackReads === 2) return staleRefresh.promise;
      return Promise.resolve(slack(false));
    }
    if (
      command === "notion_connections" ||
      command === "google_connections" ||
      command === "github_connections" ||
      command === "gitlab_connections"
    ) {
      return Promise.resolve([]);
    }
    return Promise.resolve(undefined);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Open settings" }));
  await screen.findByText("Example workspace");
  await waitFor(() => expect(invalidate).toBeDefined());

  await act(async () => invalidate?.());
  await waitFor(() => expect(slackReads).toBe(2));

  fireEvent.click(screen.getByRole("button", { name: "Disable" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("update_slack_source", {
      sourceId: "slack-source",
      reactionName: "todo",
      enabled: false,
    }),
  );
  await screen.findByRole("button", { name: "Enable" });

  await act(async () => staleRefresh.resolve(slack(true)));

  expect(screen.getByRole("button", { name: "Enable" })).toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Disable" }),
  ).not.toBeInTheDocument();
});
