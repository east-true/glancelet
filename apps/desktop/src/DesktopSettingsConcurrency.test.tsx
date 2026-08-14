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
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}

test("rolls back only the desktop setting whose save fails", async () => {
  const alwaysOnTop = deferred<void>();
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard")
      return Promise.resolve({ today: [], inbox: [] });
    if (command === "widget_layout") return Promise.resolve(undefined);
    if (
      command === "slack_connections" ||
      command === "notion_connections" ||
      command === "google_connections" ||
      command === "github_connections" ||
      command === "gitlab_connections"
    ) {
      return Promise.resolve([]);
    }
    if (command === "desktop_settings") {
      return Promise.resolve({ alwaysOnTop: false, launchAtStartup: false });
    }
    if (command === "set_always_on_top") return alwaysOnTop.promise;
    if (command === "set_launch_at_startup") return Promise.resolve(undefined);
    return Promise.resolve(undefined);
  });

  render(<App />);
  await waitFor(() => expect(mocks.invoke).toHaveBeenCalledWith("dashboard"));
  fireEvent.click(screen.getByRole("button", { name: "Open settings" }));
  fireEvent.click(await screen.findByRole("button", { name: "Settings" }));

  const always = await screen.findByRole("checkbox", {
    name: /Always on Top/,
  });
  const autostart = screen.getByRole("checkbox", {
    name: /Launch Glancelet at startup/,
  });

  fireEvent.click(always);
  expect(always).toBeChecked();
  expect(always).toBeDisabled();
  fireEvent.click(always);
  expect(
    mocks.invoke.mock.calls.filter(
      ([command]) => command === "set_always_on_top",
    ),
  ).toHaveLength(1);

  fireEvent.click(autostart);
  await waitFor(() => expect(autostart).toBeChecked());
  await waitFor(() => expect(autostart).toBeEnabled());

  await act(async () => alwaysOnTop.reject(new Error("always-on-top failed")));

  await waitFor(() => expect(always).not.toBeChecked());
  expect(always).toBeEnabled();
  expect(autostart).toBeChecked();
  expect(screen.getByText(/always-on-top failed/)).toBeInTheDocument();
  expect(mocks.invoke).toHaveBeenCalledWith("set_launch_at_startup", {
    enabled: true,
  });
});
