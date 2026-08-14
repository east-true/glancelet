import { beforeEach, expect, test, vi } from "vitest";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

import { glanceletApi, type SlackConnection } from "./api";

beforeEach(() => mocks.invoke.mockReset());

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

function slack(workspace: string): SlackConnection[] {
  return [
    {
      connectionId: workspace,
      sourceId: null,
      workspace,
      user: "user",
      reactionName: "eyes",
      enabled: true,
      status: "connected",
      lastSync: null,
      lastError: null,
    },
  ];
}

test("chains every stale snapshot to the newest overlapping request", async () => {
  const first = deferred<SlackConnection[]>();
  const second = deferred<SlackConnection[]>();
  const third = deferred<SlackConnection[]>();
  const reads = [first.promise, second.promise, third.promise];
  let readIndex = 0;

  mocks.invoke.mockImplementation((command: string) => {
    if (command === "slack_connections") return reads[readIndex++];
    return Promise.resolve(undefined);
  });

  const oldest = glanceletApi.slackConnections();
  const middle = glanceletApi.slackConnections();

  first.resolve(slack("oldest"));
  await Promise.resolve();

  const newest = glanceletApi.slackConnections();
  third.resolve(slack("newest"));
  await expect(newest).resolves.toEqual(slack("newest"));

  second.resolve(slack("middle"));
  await expect(middle).resolves.toEqual(slack("newest"));
  await expect(oldest).resolves.toEqual(slack("newest"));
  expect(readIndex).toBe(3);
});
