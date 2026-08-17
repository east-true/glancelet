import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { QuickCapture } from "./QuickCapture";

afterEach(cleanup);

test("captures a trimmed title with the selected planning and keyboard submit", async () => {
  const capture = vi.fn().mockResolvedValue(undefined);
  const close = vi.fn();
  render(<QuickCapture open onClose={close} onCapture={capture} />);

  const title = screen.getByRole("textbox", { name: "Capture title" });
  await waitFor(() => expect(title).toHaveFocus());
  fireEvent.change(title, { target: { value: "  Review deployment issue  " } });
  fireEvent.click(screen.getByRole("radio", { name: "Today" }));
  fireEvent.submit(screen.getByRole("dialog", { name: "Quick Capture" }));

  await waitFor(() => expect(capture).toHaveBeenCalledOnce());
  expect(capture).toHaveBeenCalledWith(
    expect.stringMatching(/^[0-9a-f-]{36}$/),
    "Review deployment issue",
    "today",
  );
  expect(close).toHaveBeenCalledOnce();
});

test("rejects blank input and suppresses duplicate submits while pending", async () => {
  let finish!: () => void;
  const pending = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const capture = vi.fn().mockReturnValue(pending);
  render(<QuickCapture open onClose={() => undefined} onCapture={capture} />);

  const title = screen.getByRole("textbox", { name: "Capture title" });
  fireEvent.submit(screen.getByRole("dialog", { name: "Quick Capture" }));
  expect(
    await screen.findByText("Enter a title to capture."),
  ).toBeInTheDocument();
  expect(capture).not.toHaveBeenCalled();

  fireEvent.change(title, { target: { value: "One capture" } });
  fireEvent.click(screen.getByRole("button", { name: "Add" }));
  fireEvent.submit(screen.getByRole("dialog", { name: "Quick Capture" }));
  expect(capture).toHaveBeenCalledOnce();
  finish();
  await pending;
});

test("Escape closes Quick Capture without submitting", () => {
  const close = vi.fn();
  const capture = vi.fn();
  render(<QuickCapture open onClose={close} onCapture={capture} />);
  fireEvent.keyDown(document, { key: "Escape" });
  expect(close).toHaveBeenCalledOnce();
  expect(capture).not.toHaveBeenCalled();
});
