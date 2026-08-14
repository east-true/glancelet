import { useState } from "react";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { DesktopSurface } from "./DesktopSurface";
import type { WidgetInstance, WorkDashboard, WorkView } from "./api";

afterEach(cleanup);

const defaultLayout: WidgetInstance[] = [
  { widgetType: "today", position: 0, size: "wide", settings: {} },
  { widgetType: "inbox", position: 1, size: "compact", settings: {} },
  { widgetType: "attention", position: 2, size: "compact", settings: {} },
];

function work(
  title: string,
  availableActions: WorkView["availableActions"] = [
    "plan",
    "move_to_backlog",
    "snooze",
    "dismiss",
    "pin",
    "open_source",
  ],
): WorkView {
  return {
    id: title.toLowerCase().replaceAll(" ", "-"),
    kind: "action",
    title,
    summary: null,
    priority: null,
    lifecycle: "active",
    progress: null,
    planning: { type: "inbox" },
    disposition: "normal",
    pinned: false,
    snoozedUntil: null,
    start: null,
    end: null,
    due: null,
    source: {
      providerId: "test",
      providerName: "Test",
      sourceName: "Tasks",
      configName: "Project",
    },
    canNavigate: true,
    freshness: "fresh",
    dimensions: {},
    facets: {},
    availableActions,
  };
}

function dashboard(overrides: Partial<WorkDashboard> = {}): WorkDashboard {
  return {
    today: [],
    inbox: [],
    upcoming: [],
    attention: [],
    sourceHealth: { sourceCount: 1, issues: [] },
    ...overrides,
  };
}

function SurfaceHarness({ data = dashboard() }: { data?: WorkDashboard }) {
  const [editing, setEditing] = useState(false);
  const [layout, setLayout] = useState(defaultLayout);
  return (
    <>
      <button onClick={() => setEditing(!editing)}>
        {editing ? "Done" : "Edit Layout"}
      </button>
      <DesktopSurface
        data={data}
        layout={layout}
        loading={false}
        editing={editing}
        pendingWorkIds={new Set()}
        onEdit={setEditing}
        onLayout={async (next) => setLayout(next)}
        onRun={async () => undefined}
        onOpen={async () => undefined}
        onSources={() => undefined}
      />
    </>
  );
}

test("shows product empty states and first-run source CTA", () => {
  render(
    <SurfaceHarness
      data={dashboard({ sourceHealth: { sourceCount: 0, issues: [] } })}
    />,
  );
  expect(
    screen.getByText("Nothing needs your attention today."),
  ).toBeInTheDocument();
  expect(screen.getByText("Inbox is clear.")).toBeInTheDocument();
  expect(screen.getByText("No active alerts.")).toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "Connect a Source" }),
  ).toBeInTheDocument();
});

test("edit mode adds and removes built-in widgets", async () => {
  render(<SurfaceHarness />);
  fireEvent.click(screen.getByRole("button", { name: "Edit Layout" }));
  fireEvent.click(screen.getByRole("button", { name: /Upcoming.*Add/ }));
  expect(
    await screen.findByRole("heading", { name: "Upcoming" }),
  ).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Remove Upcoming" }));
  await waitFor(() =>
    expect(
      screen.queryByRole("heading", { name: "Upcoming" }),
    ).not.toBeInTheDocument(),
  );
});

test("edit mode reorders and resizes with keyboard-accessible controls", async () => {
  render(<SurfaceHarness />);
  fireEvent.click(screen.getByRole("button", { name: "Edit Layout" }));
  fireEvent.click(screen.getByRole("button", { name: "Move Inbox up" }));
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Move Inbox up" }),
    ).toBeDisabled(),
  );
  fireEvent.click(screen.getByRole("button", { name: "Resize Inbox" }));
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Resize Inbox" }),
    ).toHaveTextContent("wide"),
  );
});

test("Escape leaves layout edit mode", () => {
  render(<SurfaceHarness />);
  fireEvent.click(screen.getByRole("button", { name: "Edit Layout" }));
  expect(screen.getByRole("button", { name: "Done" })).toBeInTheDocument();
  fireEvent.keyDown(window, { key: "Escape" });
  expect(
    screen.getByRole("button", { name: "Edit Layout" }),
  ).toBeInTheDocument();
});

test("quick planning uses the available actions supplied by WorkView", async () => {
  const run = vi.fn().mockResolvedValue(undefined);
  const open = vi.fn().mockResolvedValue(undefined);
  const item = work("Plan release");
  render(
    <DesktopSurface
      data={dashboard({ inbox: [item] })}
      layout={[defaultLayout[1]]}
      loading={false}
      editing={false}
      pendingWorkIds={new Set()}
      onEdit={() => undefined}
      onLayout={async () => undefined}
      onRun={run}
      onOpen={open}
      onSources={() => undefined}
    />,
  );
  expect(open).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Today" }));
  await waitFor(() =>
    expect(run).toHaveBeenCalledWith(
      item.id,
      expect.objectContaining({ type: "plan" }),
    ),
  );
  expect(
    screen.queryByRole("button", { name: "Complete" }),
  ).not.toBeInTheDocument();
});

test("quick actions run only the commands advertised by WorkView", async () => {
  const run = vi.fn().mockResolvedValue(undefined);
  const open = vi.fn().mockResolvedValue(undefined);
  const item = work("Triage incident", [
    "snooze",
    "dismiss",
    "pin",
    "open_source",
  ]);
  render(
    <DesktopSurface
      data={dashboard({ today: [item] })}
      layout={[defaultLayout[0]]}
      loading={false}
      editing={false}
      pendingWorkIds={new Set()}
      onEdit={() => undefined}
      onLayout={async () => undefined}
      onRun={run}
      onOpen={open}
      onSources={() => undefined}
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "Snooze" }));
  fireEvent.click(screen.getByRole("button", { name: "Pin Triage incident" }));
  fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
  fireEvent.click(screen.getByRole("button", { name: "Open" }));

  await waitFor(() => {
    expect(run).toHaveBeenCalledWith(
      item.id,
      expect.objectContaining({ type: "snooze" }),
    );
    expect(run).toHaveBeenCalledWith(item.id, { type: "pin" });
    expect(run).toHaveBeenCalledWith(item.id, { type: "dismiss" });
    expect(open).toHaveBeenCalledWith(item.id);
  });
  expect(
    screen.queryByRole("button", { name: "Complete" }),
  ).not.toBeInTheDocument();
});

test("generic work rendering opens the source without provider-specific cards", async () => {
  const open = vi.fn().mockResolvedValue(undefined);
  const item = work("Review merge request", ["open_source"]);
  render(
    <DesktopSurface
      data={dashboard({ today: [item] })}
      layout={[defaultLayout[0]]}
      loading={false}
      editing={false}
      pendingWorkIds={new Set()}
      onEdit={() => undefined}
      onLayout={async () => undefined}
      onRun={async () => undefined}
      onOpen={open}
      onSources={() => undefined}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /Review merge request/ }));
  await waitFor(() => expect(open).toHaveBeenCalledWith(item.id));
});

test("source health remains non-blocking and links to Sources", () => {
  const sources = vi.fn();
  render(
    <DesktopSurface
      data={dashboard({
        today: [work("Cached work")],
        sourceHealth: {
          sourceCount: 2,
          issues: [
            {
              sourceId: "github",
              sourceName: "GitHub",
              kind: "authentication_required",
            },
          ],
        },
      })}
      layout={[defaultLayout[0]]}
      loading={false}
      editing={false}
      pendingWorkIds={new Set()}
      onEdit={() => undefined}
      onLayout={async () => undefined}
      onRun={async () => undefined}
      onOpen={async () => undefined}
      onSources={sources}
    />,
  );
  expect(screen.getByText("Cached work")).toBeInTheDocument();
  fireEvent.click(
    screen.getByRole("button", { name: /1 source needs attention/ }),
  );
  expect(sources).toHaveBeenCalledOnce();
});
