import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import App from "./App";
import { localDateString } from "./local-time";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

beforeEach(() => mocks.invoke.mockReset());
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
    .mockResolvedValueOnce([]);
  render(<App />);
  await waitFor(() => expect(mocks.invoke).toHaveBeenCalledWith("dashboard"));
  fireEvent.click(screen.getByRole("button", { name: "Sources" }));
  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("slack_connections"),
  );
  expect(screen.getByText("No Slack workspace connected.")).toBeInTheDocument();
});
