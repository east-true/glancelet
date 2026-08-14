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
});
afterEach(cleanup);

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

test("re-reads settings after an overlapping write", async () => {
  const staleRead = deferred<{
    alwaysOnTop: boolean;
    launchAtStartup: boolean;
  }>();
  const startupWrite = deferred<void>();
  let settingsChanged: (() => void) | undefined;
  let settingsReads = 0;

  mocks.listen.mockImplementation(
    async (event: string, handler: () => void) => {
      if (event === "glancelet://desktop-settings-changed") {
        settingsChanged = handler;
      }
      return () => undefined;
    },
  );
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard") {
      return Promise.resolve({
        today: [],
        inbox: [],
        upcoming: [],
        attention: [],
      });
    }
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
      settingsReads += 1;
      if (settingsReads === 1) {
        return Promise.resolve({
          alwaysOnTop: false,
          launchAtStartup: false,
        });
      }
      if (settingsReads === 2) return staleRead.promise;
      return Promise.resolve({ alwaysOnTop: true, launchAtStartup: true });
    }
    if (command === "set_launch_at_startup") return startupWrite.promise;
    return Promise.resolve(undefined);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Open settings" }));
  fireEvent.click(await screen.findByRole("button", { name: "Settings" }));

  const always = await screen.findByRole("checkbox", {
    name: /Always on Top/,
  });
  const startup = screen.getByRole("checkbox", {
    name: /Launch Glancelet at startup/,
  });
  expect(always).not.toBeChecked();
  expect(startup).not.toBeChecked();
  await waitFor(() => expect(settingsChanged).toBeDefined());

  await act(async () => settingsChanged?.());
  await waitFor(() => expect(settingsReads).toBe(2));

  fireEvent.click(startup);
  expect(startup).toBeChecked();

  await act(async () => {
    staleRead.resolve({ alwaysOnTop: true, launchAtStartup: false });
    startupWrite.resolve(undefined);
  });

  await waitFor(() => expect(settingsReads).toBe(3));
  await waitFor(() => expect(always).toBeChecked());
  expect(startup).toBeChecked();
});
