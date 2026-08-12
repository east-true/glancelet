from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


app_path = Path("apps/desktop/src/App.tsx")
app = app_path.read_text()
app = replace_once(
    app,
    '''      const report = await glanceletApi.sync();
      await Promise.all([
        refresh(false),
        tabRef.current === "settings"
          ? refreshSources(false)
          : Promise.resolve(),
      ]);
      setError(syncReportMessage(report));
''',
    '''      const report = await glanceletApi.sync();
      setError(syncReportMessage(report));
      await Promise.all([
        refresh(false),
        tabRef.current === "settings"
          ? refreshSources(false)
          : Promise.resolve(),
      ]);
''',
    "global sync report ordering",
)
app = replace_once(
    app,
    '''          <SlackSettings
            busy={globalBusy || connectingSlack}
            connections={slackConnections}
            connect={connectSlack}
            refresh={refreshSlack}
            setError={setError}
          />
''',
    '''          <SlackSettings
            busy={globalBusy || connectingSlack}
            connections={slackConnections}
            connect={connectSlack}
            refresh={refreshSlack}
            refreshWork={refresh}
            setError={setError}
          />
''',
    "Slack settings wiring",
)
app = replace_once(
    app,
    '''function SlackSettings({
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
''',
    '''function SlackSettings({
  busy,
  connections,
  connect,
  refresh,
  refreshWork,
  setError,
}: {
  busy: boolean;
  connections: SlackConnection[];
  connect: () => Promise<void>;
  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
  setError: (error: string | null) => void;
}) {
''',
    "Slack settings props",
)
app = replace_once(
    app,
    '''          <SlackConnectionCard
            key={connection.connectionId}
            connection={connection}
            refresh={refresh}
            setError={setError}
          />
''',
    '''          <SlackConnectionCard
            key={connection.connectionId}
            connection={connection}
            refresh={refresh}
            refreshWork={refreshWork}
            setError={setError}
          />
''',
    "Slack connection wiring",
)
app = replace_once(
    app,
    '''function SlackConnectionCard({
  connection,
  refresh,
  setError,
}: {
  connection: SlackConnection;
  refresh: () => Promise<void>;
  setError: (error: string | null) => void;
}) {
''',
    '''function SlackConnectionCard({
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
''',
    "Slack connection props",
)
app = replace_once(
    app,
    '''  async function action(task: () => Promise<string | null | void>) {
    setWorking(true);
    try {
      setError(null);
      const message = await task();
      await refresh();
      if (message) setError(message);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setWorking(false);
    }
  }

  return (
''',
    '''  async function action(task: () => Promise<void>) {
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
''',
    "Slack action helpers",
)
app = replace_once(
    app,
    '''              onClick={() =>
                void action(async () =>
                  syncReportMessage(
                    await glanceletApi.syncSource(
                      connection.sourceId as string,
                    ),
                  ),
                )
              }
''',
    '''              onClick={() =>
                void syncSource(connection.sourceId as string)
              }
''',
    "Slack source sync action",
)
app = app.replace(
    '''  refresh: () => Promise<void>;
  refreshWork: () => Promise<void>;
''',
    '''  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
''',
)
app = replace_once(
    app,
    '''                  const report = await glanceletApi.syncSource(source.sourceId);
                  await Promise.all([refresh(), refreshWork()]);
                  setError(syncReportMessage(report));
''',
    '''                  const report = await glanceletApi.syncSource(source.sourceId);
                  setError(syncReportMessage(report));
                  await Promise.all([refresh(false), refreshWork(false)]);
''',
    "Notion sync report ordering",
)
app_path.write_text(app)

google_path = Path("apps/desktop/src/GoogleSettings.tsx")
google = google_path.read_text().replace(
    '''  refresh: () => Promise<void>;
  refreshWork: () => Promise<void>;
''',
    '''  refresh: (clearError?: boolean) => Promise<void>;
  refreshWork: (clearError?: boolean) => Promise<void>;
''',
)
google = replace_once(
    google,
    '''      const report = await glanceletApi.syncSource(sourceId);
      await Promise.all([refresh(), refreshWork()]);
      setError(syncReportMessage(report));
''',
    '''      const report = await glanceletApi.syncSource(sourceId);
      setError(syncReportMessage(report));
      await Promise.all([refresh(false), refreshWork(false)]);
''',
    "Google sync report ordering",
)
google_path.write_text(google)

test_path = Path("apps/desktop/src/App.test.tsx")
tests = test_path.read_text()
marker = "\nfunction deferred<T>() {\n"
if tests.count(marker) != 1:
    raise SystemExit("test helper marker not found exactly once")
new_tests = r'''

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
'''
test_path.write_text(tests.replace(marker, new_tests + marker, 1))
