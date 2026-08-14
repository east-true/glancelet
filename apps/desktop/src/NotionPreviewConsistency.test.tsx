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
import type { NotionPreviewRow } from "./api";

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));

beforeEach(() => {
  mocks.invoke.mockReset();
  mocks.listen.mockReset();
  mocks.listen.mockResolvedValue(() => undefined);
});
afterEach(cleanup);

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

function emptyDashboard() {
  return {
    today: [],
    inbox: [],
    upcoming: [],
    attention: [],
    sourceHealth: { sourceCount: 0, issues: [] },
  };
}

test("shows a Notion preview only for the mapping that produced it", async () => {
  const stalePreview = deferred<NotionPreviewRow[]>();
  let previewReads = 0;

  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard") return Promise.resolve(emptyDashboard());
    if (command === "widget_layout") return Promise.resolve(undefined);
    if (
      command === "slack_connections" ||
      command === "google_connections" ||
      command === "github_connections" ||
      command === "gitlab_connections"
    ) {
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
          { id: "due-a", name: "Due A", type: "date", status: null },
          { id: "due-b", name: "Due B", type: "date", status: null },
        ],
      });
    }
    if (command === "preview_notion_source") {
      previewReads += 1;
      if (previewReads === 1) return stalePreview.promise;
      return Promise.resolve([
        {
          externalId: "current",
          title: "Current preview",
          status: null,
          due: null,
        },
      ]);
    }
    return Promise.resolve(undefined);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Open settings" }));
  const dataSourceId = await screen.findByRole("textbox", {
    name: "Notion Data Source ID",
  });
  fireEvent.change(dataSourceId, {
    target: { value: "ds-1" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Load schema" }));

  const due = await screen.findByRole("combobox", { name: "Due Date" });
  expect(due).toHaveValue("due-a");
  fireEvent.click(screen.getByRole("button", { name: "Preview" }));

  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith(
      "preview_notion_source",
      expect.objectContaining({
        settings: expect.objectContaining({
          properties: expect.objectContaining({
            due: { id: "due-a", name: "Due A" },
          }),
        }),
      }),
    ),
  );

  fireEvent.change(due, { target: { value: "due-b" } });
  expect(due).toHaveValue("due-b");

  await act(async () => {
    stalePreview.resolve([
      {
        externalId: "stale",
        title: "Stale preview",
        status: null,
        due: null,
      },
    ]);
  });

  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Preview" })).toBeEnabled(),
  );
  expect(screen.queryByText("Stale preview")).not.toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Preview" }));
  expect(await screen.findByText("Current preview")).toBeInTheDocument();
  expect(previewReads).toBe(2);
  expect(mocks.invoke).toHaveBeenLastCalledWith(
    "preview_notion_source",
    expect.objectContaining({
      settings: expect.objectContaining({
        properties: expect.objectContaining({
          due: { id: "due-b", name: "Due B" },
        }),
      }),
    }),
  );
});
