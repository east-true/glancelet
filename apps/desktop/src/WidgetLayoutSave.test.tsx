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
import type { WidgetInstance } from "./api";

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));

const initialLayout: WidgetInstance[] = [
  { widgetType: "today", position: 0, size: "wide", settings: {} },
  { widgetType: "inbox", position: 1, size: "compact", settings: {} },
  { widgetType: "attention", position: 2, size: "compact", settings: {} },
];

beforeEach(() => {
  mocks.invoke.mockReset();
  mocks.listen.mockReset();
  mocks.listen.mockResolvedValue(() => undefined);
});
afterEach(cleanup);

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}

function mockApp(save: (widgets: WidgetInstance[]) => Promise<void>) {
  mocks.invoke.mockImplementation((command: string, args?: unknown) => {
    if (command === "dashboard") {
      return Promise.resolve({
        today: [],
        inbox: [],
        upcoming: [],
        attention: [],
      });
    }
    if (command === "widget_layout") return Promise.resolve(initialLayout);
    if (command === "save_widget_layout") {
      return save((args as { widgets: WidgetInstance[] }).widgets);
    }
    return Promise.resolve([]);
  });
}

async function enterEditMode() {
  const edit = await screen.findByRole("button", { name: "Edit layout" });
  await waitFor(() => expect(edit).toBeEnabled());
  fireEvent.click(edit);
  return screen.getByRole("button", { name: "Resize Today" });
}

test("serializes layout saves and rolls back the latest failure", async () => {
  const first = deferred<void>();
  const second = deferred<void>();
  const saved: WidgetInstance[][] = [];
  mockApp((widgets) => {
    saved.push(widgets);
    return saved.length === 1 ? first.promise : second.promise;
  });

  render(<App />);
  const resize = await enterEditMode();

  fireEvent.click(resize);
  await waitFor(() => expect(resize).toHaveTextContent("tall"));
  fireEvent.click(resize);
  await waitFor(() => expect(resize).toHaveTextContent("compact"));

  expect(saved).toHaveLength(1);
  expect(saved[0].find((widget) => widget.widgetType === "today")?.size).toBe(
    "tall",
  );

  await act(async () => first.resolve(undefined));
  await waitFor(() => expect(saved).toHaveLength(2));
  expect(saved[1].find((widget) => widget.widgetType === "today")?.size).toBe(
    "compact",
  );

  await act(async () => second.reject(new Error("second layout save failed")));

  await waitFor(() => expect(resize).toHaveTextContent("tall"));
  expect(screen.getByText(/second layout save failed/)).toBeInTheDocument();
});

test("preserves newer layout intent after an older save fails", async () => {
  const first = deferred<void>();
  const second = deferred<void>();
  const saved: WidgetInstance[][] = [];
  mockApp((widgets) => {
    saved.push(widgets);
    return saved.length === 1 ? first.promise : second.promise;
  });

  render(<App />);
  const resize = await enterEditMode();

  fireEvent.click(resize);
  await waitFor(() => expect(resize).toHaveTextContent("tall"));
  fireEvent.click(resize);
  await waitFor(() => expect(resize).toHaveTextContent("compact"));
  expect(saved).toHaveLength(1);

  await act(async () => first.reject(new Error("first layout save failed")));
  await waitFor(() => expect(saved).toHaveLength(2));
  expect(resize).toHaveTextContent("compact");

  await act(async () => second.resolve(undefined));
  expect(resize).toHaveTextContent("compact");
  expect(
    screen.queryByText(/first layout save failed/),
  ).not.toBeInTheDocument();
});

test("waits for layout hydration before editing", async () => {
  const hydration = deferred<WidgetInstance[]>();
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard") {
      return Promise.resolve({
        today: [],
        inbox: [],
        upcoming: [],
        attention: [],
      });
    }
    if (command === "widget_layout") return hydration.promise;
    return Promise.resolve([]);
  });

  render(<App />);
  const edit = await screen.findByRole("button", { name: "Edit layout" });
  expect(edit).toBeDisabled();

  await act(async () => hydration.resolve(initialLayout));
  await waitFor(() => expect(edit).toBeEnabled());
});
