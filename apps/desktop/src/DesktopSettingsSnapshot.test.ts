import { beforeEach, expect, test, vi } from "vitest";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

import { glanceletApi, type DesktopSettings } from "./api";

beforeEach(() => mocks.invoke.mockReset());

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

test("re-reads desktop settings when a mutation overlaps an older snapshot", async () => {
  const staleRead = deferred<DesktopSettings>();
  const save = deferred<void>();
  let settingsReads = 0;

  mocks.invoke.mockImplementation((command: string) => {
    if (command === "desktop_settings") {
      settingsReads += 1;
      if (settingsReads === 1) return staleRead.promise;
      return Promise.resolve({ alwaysOnTop: true, launchAtStartup: true });
    }
    if (command === "set_launch_at_startup") return save.promise;
    return Promise.resolve(undefined);
  });

  const snapshot = glanceletApi.desktopSettings();
  const mutation = glanceletApi.setLaunchAtStartup(true);

  staleRead.resolve({ alwaysOnTop: true, launchAtStartup: false });
  await Promise.resolve();
  expect(settingsReads).toBe(1);

  save.resolve();
  await mutation;

  await expect(snapshot).resolves.toEqual({
    alwaysOnTop: true,
    launchAtStartup: true,
  });
  expect(settingsReads).toBe(2);
});
