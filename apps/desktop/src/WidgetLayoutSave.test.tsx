import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import { DesktopSurface } from "./DesktopSurface";
import type { WidgetInstance, WorkDashboard } from "./api";

const layout: WidgetInstance[] = [
  { widgetType: "today", position: 0, size: "wide", settings: {} },
  { widgetType: "inbox", position: 1, size: "compact", settings: {} },
  { widgetType: "attention", position: 2, size: "compact", settings: {} },
];

const data: WorkDashboard = {
  today: [],
  inbox: [],
  upcoming: [],
  attention: [],
  sourceHealth: { sourceCount: 1, issues: [] },
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

test("blocks overlapping widget layout saves", async () => {
  const save = deferred<void>();
  const onLayout = vi.fn(() => save.promise);

  render(
    <DesktopSurface
      data={data}
      layout={layout}
      loading={false}
      editing
      pendingWorkIds={new Set()}
      onEdit={() => undefined}
      onLayout={onLayout}
      onRun={async () => undefined}
      onOpen={async () => undefined}
      onSources={() => undefined}
    />,
  );

  const resize = screen.getByRole("button", { name: "Resize Today" });
  const move = screen.getByRole("button", { name: "Move Inbox up" });
  const add = screen.getByRole("button", { name: /Upcoming.*Add/ });

  fireEvent.click(resize);
  expect(onLayout).toHaveBeenCalledTimes(1);
  expect(resize).toBeDisabled();
  expect(move).toBeDisabled();
  expect(add).toBeDisabled();

  fireEvent.click(move);
  fireEvent.click(add);
  expect(onLayout).toHaveBeenCalledTimes(1);

  await act(async () => save.resolve(undefined));

  await waitFor(() => expect(resize).toBeEnabled());
  expect(move).toBeEnabled();
  expect(add).toBeEnabled();
});
