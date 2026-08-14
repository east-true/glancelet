import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { GoogleSettings } from "./GoogleSettings";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

beforeEach(() => mocks.invoke.mockReset());
afterEach(cleanup);

test("prevents duplicate Google connection requests while one is pending", async () => {
  let resolveConnect: (() => void) | undefined;
  const pendingConnect = new Promise<void>((resolve) => {
    resolveConnect = resolve;
  });
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "connect_google") return pendingConnect;
    return Promise.resolve(undefined);
  });
  const refresh = vi.fn().mockResolvedValue(undefined);

  render(
    <GoogleSettings
      busy={false}
      connections={[]}
      refresh={refresh}
      refreshWork={vi.fn().mockResolvedValue(undefined)}
      setError={vi.fn()}
    />,
  );

  const connect = screen.getByRole("button", { name: "Connect Google" });
  fireEvent.click(connect);
  fireEvent.click(connect);

  expect(connect).toBeDisabled();
  expect(
    mocks.invoke.mock.calls.filter(([command]) => command === "connect_google"),
  ).toHaveLength(1);

  await act(async () => resolveConnect?.());
  await waitFor(() => expect(connect).toBeEnabled());
  expect(refresh).toHaveBeenCalledTimes(1);
});

test("keeps selected calendars when saving fails", async () => {
  mocks.invoke.mockImplementation((command: string) => {
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
    if (command === "save_google_calendars") {
      return Promise.reject(new Error("save failed"));
    }
    return Promise.resolve(undefined);
  });
  const setError = vi.fn();

  render(
    <GoogleSettings
      busy={false}
      connections={[
        {
          connectionId: "google-1",
          email: "user@example.com",
          status: "connected",
          sources: [],
        },
      ]}
      refresh={vi.fn().mockResolvedValue(undefined)}
      refreshWork={vi.fn().mockResolvedValue(undefined)}
      setError={setError}
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "Refresh calendars" }));
  const work = await screen.findByRole("checkbox", { name: "Work" });
  const engineering = screen.getByRole("checkbox", { name: "Engineering" });
  fireEvent.click(engineering);

  expect(work).toBeChecked();
  expect(engineering).toBeChecked();

  fireEvent.click(
    screen.getByRole("button", { name: "Add selected calendars" }),
  );

  await waitFor(() =>
    expect(setError).toHaveBeenLastCalledWith("Error: save failed"),
  );
  expect(mocks.invoke).toHaveBeenCalledWith("save_google_calendars", {
    connectionId: "google-1",
    selections: [
      { calendarId: "work@example.com" },
      { calendarId: "team@example.com" },
    ],
  });
  expect(work).toBeChecked();
  expect(engineering).toBeChecked();
});
