import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import App from "./App";
import { localDateString } from "./local-time";

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));

beforeEach(() => {
  mocks.invoke.mockReset();
  mocks.listen.mockReset();
  mocks.listen.mockResolvedValue(() => undefined);
});
afterEach(cleanup);

test("renders the presentation-safe WorkView returned by the application", async () => {
  mocks.invoke.mockResolvedValueOnce({
    today: [
      {
        id: "work-1",
        kind: "attention",
        title: "Review failed build",
        summary: null,
        priority: 1,
        lifecycle: "active",
        progress: null,
        planning: null,
        disposition: "normal",
        pinned: false,
        snoozedUntil: null,
        start: null,
        end: null,
        due: null,
        source: {
          providerId: "dev.test",
          providerName: "Test",
          sourceName: "Builds",
          configName: "Backend",
        },
        canNavigate: true,
        freshness: "fresh",
        dimensions: {},
        facets: {},
        availableActions: ["dismiss", "open_source"],
      },
    ],
    inbox: [],
  });
  render(<App />);
  await waitFor(() =>
    expect(screen.getByText("Review failed build")).toBeInTheDocument(),
  );
  expect(screen.getByText(/attention · Backend/)).toBeInTheDocument();
  expect(mocks.invoke).toHaveBeenCalledWith("dashboard");
});

test("formats Today using the browser local date rather than UTC", () => {
  const localMidnight = new Date(2026, 7, 12, 0, 30, 0);
  expect(localDateString(localMidnight)).toBe("2026-08-12");
});

test("loads Slack settings through Tauri commands", async () => {
  mocks.invoke
    .mockResolvedValueOnce({ today: [], inbox: [] })
    .mockResolvedValueOnce([])
    .mockResolvedValueOnce([])
    .mockResolvedValueOnce([])
    .mockResolvedValueOnce([]);
  render(<App />);
  await waitFor(() => expect(mocks.invoke).toHaveBeenCalledWith("dashboard"));
  fireEvent.click(screen.getByRole("button", { name: "Sources" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("slack_connections"),
  );
  expect(mocks.invoke).toHaveBeenCalledWith("notion_connections");
  expect(mocks.invoke).toHaveBeenCalledWith("google_connections");
  expect(mocks.invoke).toHaveBeenCalledWith("github_connections");
  expect(screen.getByText("No Slack workspace connected.")).toBeInTheDocument();
  expect(screen.getByText("No Notion account connected.")).toBeInTheDocument();
  expect(screen.getByText("No Google account connected.")).toBeInTheDocument();
  expect(screen.getByText("No GitHub account connected.")).toBeInTheDocument();
});

test("shows the GitHub Device Flow code while authorization is pending", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard")
      return Promise.resolve({ today: [], inbox: [] });
    if (command.endsWith("_connections")) return Promise.resolve([]);
    if (command === "start_github_connection") {
      return Promise.resolve({
        sessionId: "device-session",
        userCode: "ABCD-EFGH",
        verificationUri: "https://github.com/login/device",
        expiresAt: "2026-08-13T01:00:00Z",
        retryAfterSeconds: 60,
      });
    }
    return Promise.resolve(undefined);
  });
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Sources" }));
  fireEvent.click(
    await screen.findByRole("button", { name: "Connect GitHub" }),
  );
  expect(await screen.findByText("ABCD-EFGH")).toBeInTheDocument();
  expect(
    screen.getByText("https://github.com/login/device"),
  ).toBeInTheDocument();
  expect(mocks.invoke).toHaveBeenCalledWith("start_github_connection");
});

test("configures global and repository-scoped GitHub sources", async () => {
  mocks.invoke.mockImplementation((command: string, payload?: unknown) => {
    if (command === "dashboard")
      return Promise.resolve({ today: [], inbox: [] });
    if (command === "slack_connections") return Promise.resolve([]);
    if (command === "notion_connections") return Promise.resolve([]);
    if (command === "google_connections") return Promise.resolve([]);
    if (command === "github_connections") {
      return Promise.resolve([
        {
          connectionId: "github-1",
          login: "octocat",
          status: "connected",
          sources: [],
        },
      ]);
    }
    if (command === "github_repositories") {
      return Promise.resolve([
        {
          id: 99,
          nodeId: "R_99",
          fullName: "acme/backend",
          defaultBranch: "main",
        },
      ]);
    }
    if (
      command === "save_github_global_source" ||
      command === "save_github_workflow_source"
    ) {
      return Promise.resolve("source-id");
    }
    return Promise.resolve(payload);
  });
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Sources" }));
  fireEvent.click(
    await screen.findByRole("button", { name: "Refresh repositories" }),
  );
  expect(await screen.findByText("acme/backend")).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Add Review Requests" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("save_github_global_source", {
      connectionId: "github-1",
      sourceType: "github.review_requests",
    }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Add" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("save_github_workflow_source", {
      connectionId: "github-1",
      repositoryId: 99,
    }),
  );
});

test("runs the GitHub source lifecycle through Tauri commands", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard")
      return Promise.resolve({ today: [], inbox: [] });
    if (
      command === "slack_connections" ||
      command === "notion_connections" ||
      command === "google_connections"
    ) {
      return Promise.resolve([]);
    }
    if (command === "github_connections") {
      return Promise.resolve([
        {
          connectionId: "github-1",
          login: "octocat",
          status: "connected",
          sources: [
            {
              sourceId: "github-source",
              sourceType: "github.review_requests",
              name: "GitHub Review Requests",
              repository: null,
              enabled: true,
              lastSync: "2026-08-13T00:00:00Z",
              lastError: null,
            },
          ],
        },
      ]);
    }
    if (command === "sync_source") {
      return Promise.resolve({
        refreshRequired: false,
        succeeded: [],
        failed: [],
        projectionFailures: [],
      });
    }
    return Promise.resolve(undefined);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Sources" }));
  expect(await screen.findByText("GitHub Review Requests")).toBeInTheDocument();
  expect(screen.getByText(/Last sync:/)).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Sync now" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("sync_source", {
      sourceId: "github-source",
    }),
  );
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Disable" })).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Disable" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("update_github_source", {
      sourceId: "github-source",
      enabled: false,
    }),
  );
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Remove" })).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Remove" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("remove_github_source", {
      sourceId: "github-source",
    }),
  );
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Disconnect GitHub" }),
    ).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Disconnect GitHub" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("disconnect_github", {
      connectionId: "github-1",
    }),
  );
});

test("maps a Notion data source by property id and previews without storing the token", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard")
      return Promise.resolve({ today: [], inbox: [] });
    if (command === "slack_connections") return Promise.resolve([]);
    if (command === "notion_connections") {
      return Promise.resolve([
        {
          connectionId: "notion-1",
          user: "Tester",
          status: "connected",
          sources: [],
        },
      ]);
    }
    if (command === "google_connections") return Promise.resolve([]);
    if (command === "notion_data_source_schema") {
      return Promise.resolve({
        id: "ds-1",
        title: "Tasks",
        properties: [
          { id: "title", name: "Task", type: "title", status: null },
          { id: "owner", name: "Owner", type: "people", status: null },
          {
            id: "status",
            name: "Status",
            type: "status",
            status: {
              options: [
                { id: "open", name: "Open" },
                { id: "doing", name: "Doing" },
                { id: "done", name: "Done" },
              ],
              groups: [
                { id: "todo", name: "To-do", optionIds: ["open"] },
                {
                  id: "progress",
                  name: "In progress",
                  optionIds: ["doing"],
                },
                { id: "complete", name: "Complete", optionIds: ["done"] },
              ],
            },
          },
          { id: "due", name: "Due", type: "date", status: null },
        ],
      });
    }
    if (command === "preview_notion_source") {
      return Promise.resolve([
        {
          externalId: "page-1",
          title: "Review API",
          status: "Open",
          due: null,
        },
      ]);
    }
    return Promise.resolve(undefined);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Sources" }));
  await screen.findByText("Tester");
  fireEvent.change(screen.getByLabelText("Notion Data Source ID"), {
    target: { value: "ds-1" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Load schema" }));
  await screen.findByText("Mapping: Tasks");
  fireEvent.click(screen.getByRole("button", { name: "Preview" }));
  await screen.findByText("Review API");

  expect(mocks.invoke).toHaveBeenCalledWith("preview_notion_source", {
    connectionId: "notion-1",
    settings: {
      dataSourceId: "ds-1",
      dataSourceName: "Tasks",
      properties: {
        title: { id: "title", name: "Task" },
        assignee: { id: "owner", name: "Owner" },
        status: { id: "status", name: "Status" },
        due: { id: "due", name: "Due" },
      },
      onlyAssignedToMe: true,
      activeStatusIds: ["open", "doing"],
    },
  });
});

test("selects multiple calendars for one Google connection", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard")
      return Promise.resolve({ today: [], inbox: [] });
    if (command === "slack_connections" || command === "notion_connections")
      return Promise.resolve([]);
    if (command === "google_connections") {
      return Promise.resolve([
        {
          connectionId: "google-1",
          email: "user@example.com",
          status: "connected",
          sources: [],
        },
      ]);
    }
    if (command === "google_calendars") {
      return Promise.resolve([
        {
          id: "work@example.com",
          summary: "Work",
          summaryOverride: null,
          timeZone: "Asia/Seoul",
          primary: true,
          selected: true,
        },
        {
          id: "team@example.com",
          summary: "Engineering",
          summaryOverride: null,
          timeZone: "Asia/Seoul",
          primary: false,
          selected: false,
        },
      ]);
    }
    if (command === "save_google_calendars")
      return Promise.resolve(["source-a", "source-b"]);
    return Promise.resolve(undefined);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Sources" }));
  await screen.findByText("user@example.com");
  fireEvent.click(screen.getByRole("button", { name: "Refresh calendars" }));
  await screen.findByLabelText("Work");
  fireEvent.click(screen.getByLabelText("Engineering"));
  fireEvent.click(
    screen.getByRole("button", { name: "Add selected calendars" }),
  );

  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("save_google_calendars", {
      connectionId: "google-1",
      selections: [
        { calendarId: "work@example.com" },
        { calendarId: "team@example.com" },
      ],
    }),
  );
});

test("manages a Google Calendar source through generic source commands", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard")
      return Promise.resolve({ today: [], inbox: [] });
    if (command === "slack_connections" || command === "notion_connections")
      return Promise.resolve([]);
    if (command === "google_connections") {
      return Promise.resolve([
        {
          connectionId: "google-1",
          email: "user@example.com",
          status: "connected",
          sources: [
            {
              sourceId: "calendar-source",
              calendarId: "work@example.com",
              name: "Work",
              enabled: true,
              lastSync: null,
              lastError: null,
            },
          ],
        },
      ]);
    }
    return Promise.resolve(undefined);
  });
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Sources" }));
  await screen.findByText("Work");
  fireEvent.click(screen.getByRole("button", { name: "Sync now" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("sync_source", {
      sourceId: "calendar-source",
    }),
  );
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Disable" })).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Disable" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("update_google_source", {
      sourceId: "calendar-source",
      enabled: false,
    }),
  );
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Disconnect Google" }),
    ).toBeEnabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Disconnect Google" }));
  await waitFor(() => {
    expect(mocks.invoke).toHaveBeenCalledWith("disconnect_google", {
      connectionId: "google-1",
    });
  });
});

test("refreshes the HUD when the backend invalidates work", async () => {
  let invalidate: (() => void) | undefined;
  let dashboardCalls = 0;
  mocks.listen.mockImplementation(
    async (_event: string, handler: () => void) => {
      invalidate = handler;
      return () => undefined;
    },
  );
  mocks.invoke.mockImplementation((command: string) => {
    if (command !== "dashboard") return Promise.resolve([]);
    dashboardCalls += 1;
    if (dashboardCalls === 1) return Promise.resolve({ today: [], inbox: [] });
    return Promise.resolve({
      today: [
        {
          id: "background-work",
          kind: "attention",
          title: "Arrived in background",
          summary: null,
          priority: null,
          lifecycle: "active",
          progress: null,
          planning: null,
          disposition: "normal",
          pinned: false,
          snoozedUntil: null,
          start: null,
          end: null,
          due: null,
          source: {
            providerId: "test",
            providerName: "Test",
            sourceName: "Test",
            configName: "Test",
          },
          canNavigate: false,
          freshness: "fresh",
          dimensions: {},
          facets: {},
          availableActions: ["dismiss"],
        },
      ],
      inbox: [],
    });
  });

  render(<App />);
  await waitFor(() => expect(invalidate).toBeDefined());
  await act(async () => invalidate?.());
  await screen.findByText("Arrived in background");
});

test("does not send assigned-to-me mode without an assignee mapping", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard")
      return Promise.resolve({ today: [], inbox: [] });
    if (command === "slack_connections" || command === "google_connections") {
      return Promise.resolve([]);
    }
    if (command === "notion_connections") {
      return Promise.resolve([
        {
          connectionId: "notion-1",
          user: "Tester",
          status: "connected",
          sources: [],
        },
      ]);
    }
    if (command === "notion_data_source_schema") {
      return Promise.resolve({
        id: "ds-1",
        title: "Tasks",
        properties: [
          { id: "title", name: "Task", type: "title", status: null },
        ],
      });
    }
    if (command === "preview_notion_source") return Promise.resolve([]);
    return Promise.resolve(undefined);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Sources" }));
  await screen.findByText("Tester");
  fireEvent.change(screen.getByLabelText("Notion Data Source ID"), {
    target: { value: "ds-1" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Load schema" }));
  await screen.findByText("Mapping: Tasks");
  fireEvent.click(screen.getByRole("button", { name: "Preview" }));

  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("preview_notion_source", {
      connectionId: "notion-1",
      settings: {
        dataSourceId: "ds-1",
        dataSourceName: "Tasks",
        properties: {
          title: { id: "title", name: "Task" },
          assignee: null,
          status: null,
          due: null,
        },
        onlyAssignedToMe: false,
        activeStatusIds: [],
      },
    }),
  );
});

test("renders successful provider data after a partial sync failure", async () => {
  let dashboardCalls = 0;
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard") {
      dashboardCalls += 1;
      return Promise.resolve(
        dashboardCalls === 1
          ? { today: [], inbox: [] }
          : dashboardWith("Captured from Slack"),
      );
    }
    if (command === "sync_all") {
      return Promise.resolve({
        refreshRequired: true,
        succeeded: [
          {
            sourceId: "slack-source",
            sourceName: "Slack :todo:",
            changedEntities: 1,
          },
        ],
        failed: [
          {
            sourceId: "notion-source",
            sourceName: "Notion Tasks",
            kind: "rate_limited",
            message: "Notion rate limited the request",
            nextRetryAt: "2026-08-12T12:00:00Z",
          },
        ],
        projectionFailures: [],
      });
    }
    return Promise.resolve([]);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Sync" }));

  await screen.findByText("Captured from Slack");
  expect(
    screen.getByText(/Notion Tasks: Notion rate limited/),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Sync" })).toBeEnabled();
});

test("ignores an older dashboard response that finishes last", async () => {
  let invalidate: (() => void) | undefined;
  const older = deferred<ReturnType<typeof dashboardWith>>();
  const newer = deferred<ReturnType<typeof dashboardWith>>();
  let dashboardCalls = 0;
  mocks.listen.mockImplementation(
    async (_event: string, handler: () => void) => {
      invalidate = handler;
      return () => undefined;
    },
  );
  mocks.invoke.mockImplementation((command: string) => {
    if (command !== "dashboard") return Promise.resolve([]);
    dashboardCalls += 1;
    return dashboardCalls === 1 ? older.promise : newer.promise;
  });

  render(<App />);
  await waitFor(() => expect(dashboardCalls).toBe(1));
  await waitFor(() => expect(invalidate).toBeDefined());
  await act(async () => invalidate?.());
  await waitFor(() => expect(dashboardCalls).toBe(2));

  await act(async () => newer.resolve(dashboardWith("Newest dashboard")));
  await screen.findByText("Newest dashboard");
  await act(async () => older.resolve(dashboardWith("Older dashboard")));

  expect(screen.queryByText("Older dashboard")).not.toBeInTheDocument();
  expect(screen.getByText("Newest dashboard")).toBeInTheDocument();
});

test("keeps manual sync busy while a background refresh completes", async () => {
  let invalidate: (() => void) | undefined;
  const manualSync = deferred<{
    refreshRequired: boolean;
    succeeded: unknown[];
    failed: unknown[];
    projectionFailures: string[];
  }>();
  let dashboardCalls = 0;
  mocks.listen.mockImplementation(
    async (_event: string, handler: () => void) => {
      invalidate = handler;
      return () => undefined;
    },
  );
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard") {
      dashboardCalls += 1;
      return Promise.resolve({ today: [], inbox: [] });
    }
    if (command === "sync_all") return manualSync.promise;
    return Promise.resolve([]);
  });

  render(<App />);
  const syncButton = await screen.findByRole("button", { name: "Sync" });
  fireEvent.click(syncButton);
  expect(screen.getByRole("button", { name: "Syncing…" })).toBeDisabled();

  await act(async () => invalidate?.());
  await waitFor(() => expect(dashboardCalls).toBeGreaterThan(1));
  expect(screen.getByRole("button", { name: "Syncing…" })).toBeDisabled();

  await act(async () =>
    manualSync.resolve({
      refreshRequired: true,
      succeeded: [],
      failed: [],
      projectionFailures: [],
    }),
  );
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Sync" })).toBeEnabled(),
  );
});

test("disables a work card while its command is pending", async () => {
  const command = deferred<void>();
  mocks.invoke.mockImplementation((name: string) => {
    if (name === "dashboard")
      return Promise.resolve(dashboardWith("Pending work", ["complete"]));
    if (name === "run_work_command") return command.promise;
    return Promise.resolve(undefined);
  });

  render(<App />);
  const complete = await screen.findByRole("button", { name: "Complete" });
  fireEvent.click(complete);
  expect(complete).toBeDisabled();
  fireEvent.click(complete);
  expect(
    mocks.invoke.mock.calls.filter(([name]) => name === "run_work_command"),
  ).toHaveLength(1);

  await act(async () => command.resolve(undefined));
  await waitFor(() => expect(complete).toBeEnabled());
});

test("refreshes the HUD after a Slack source sync", async () => {
  let dashboardCalls = 0;
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard") {
      dashboardCalls += 1;
      return Promise.resolve(
        dashboardCalls === 1
          ? { today: [], inbox: [] }
          : dashboardWith("Captured after Slack sync"),
      );
    }
    if (command === "slack_connections") {
      return Promise.resolve([
        {
          connectionId: "slack-1",
          sourceId: "slack-source",
          workspace: "Example workspace",
          user: "Tester",
          reactionName: "todo",
          enabled: true,
          status: "connected",
          lastSync: null,
          lastError: null,
        },
      ]);
    }
    if (command === "notion_connections" || command === "google_connections") {
      return Promise.resolve([]);
    }
    if (command === "sync_source") {
      return Promise.resolve({
        refreshRequired: true,
        succeeded: [
          {
            sourceId: "slack-source",
            sourceName: "Slack :todo:",
            changedEntities: 1,
          },
        ],
        failed: [],
        projectionFailures: [],
      });
    }
    return Promise.resolve(undefined);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Sources" }));
  await screen.findByText("Example workspace");
  fireEvent.click(screen.getByRole("button", { name: "Sync now" }));
  fireEvent.click(screen.getByRole("button", { name: /^Today/ }));

  await screen.findByText("Captured after Slack sync");
  expect(mocks.invoke).toHaveBeenCalledWith("sync_source", {
    sourceId: "slack-source",
  });
});

test("does not hide a dashboard refresh failure after sync", async () => {
  let dashboardCalls = 0;
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard") {
      dashboardCalls += 1;
      return dashboardCalls === 1
        ? Promise.resolve({ today: [], inbox: [] })
        : Promise.reject(new Error("dashboard refresh failed"));
    }
    if (command === "sync_all") {
      return Promise.resolve({
        refreshRequired: true,
        succeeded: [],
        failed: [],
        projectionFailures: [],
      });
    }
    return Promise.resolve([]);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Sync" }));

  await screen.findByText(/dashboard refresh failed/);
  expect(screen.getByRole("button", { name: "Sync" })).toBeEnabled();
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}

function dashboardWith(
  title: string,
  availableActions: string[] = ["dismiss"],
) {
  return {
    today: [
      {
        id: "test-work",
        kind: "attention",
        title,
        summary: null,
        priority: null,
        lifecycle: "active",
        progress: null,
        planning: null,
        disposition: "normal",
        pinned: false,
        snoozedUntil: null,
        start: null,
        end: null,
        due: null,
        source: {
          providerId: "test",
          providerName: "Test",
          sourceName: "Test",
          configName: "Test",
        },
        canNavigate: false,
        freshness: "fresh",
        dimensions: {},
        facets: {},
        availableActions,
      },
    ],
    inbox: [],
  };
}
