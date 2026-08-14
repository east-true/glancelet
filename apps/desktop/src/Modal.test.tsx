import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { useState } from "react";
import { afterEach, expect, test } from "vitest";
import { Modal } from "./Modal";
import { SettingsOverlay } from "./SettingsOverlay";

afterEach(cleanup);

function NestedDialogs() {
  const [settingsOpen, setSettingsOpen] = useState(true);
  const [modalOpen, setModalOpen] = useState(false);

  if (!settingsOpen) return <p>Settings closed</p>;

  return (
    <>
      <button type="button">Background action</button>
      <SettingsOverlay
        section="sources"
        onSection={() => undefined}
        onClose={() => setSettingsOpen(false)}
      >
        <button type="button" onClick={() => setModalOpen(true)}>
          Open nested modal
        </button>
        <Modal
          open={modalOpen}
          title="Nested dialog"
          onClose={() => setModalOpen(false)}
        >
          <label>
            Token
            <input aria-label="Token" />
          </label>
          <button type="button">Save token</button>
        </Modal>
      </SettingsOverlay>
    </>
  );
}

test("keeps Escape scoped to the topmost dialog and restores focus", async () => {
  render(<NestedDialogs />);
  const opener = screen.getByRole("button", { name: "Open nested modal" });
  await waitFor(() => expect(screen.getByRole("button", { name: "Sources" })).toHaveFocus());
  opener.focus();
  fireEvent.click(opener);

  const input = screen.getByRole("textbox", { name: "Token" });
  await waitFor(() => expect(input).toHaveFocus());

  fireEvent.keyDown(input, { key: "Escape" });

  await waitFor(() =>
    expect(
      screen.queryByRole("dialog", { name: "Nested dialog" }),
    ).not.toBeInTheDocument(),
  );
  expect(screen.getByRole("dialog", { name: "Settings" })).toBeInTheDocument();
  await waitFor(() => expect(opener).toHaveFocus());

  fireEvent.keyDown(opener, { key: "Escape" });
  expect(await screen.findByText("Settings closed")).toBeInTheDocument();
});

test("traps Tab within the nested modal", async () => {
  render(<NestedDialogs />);
  const opener = screen.getByRole("button", { name: "Open nested modal" });
  fireEvent.click(opener);

  const input = screen.getByRole("textbox", { name: "Token" });
  const save = screen.getByRole("button", { name: "Save token" });
  const close = screen.getByRole("button", { name: "Close" });
  await waitFor(() => expect(input).toHaveFocus());

  close.focus();
  fireEvent.keyDown(close, { key: "Tab" });
  expect(input).toHaveFocus();

  input.focus();
  fireEvent.keyDown(input, { key: "Tab", shiftKey: true });
  expect(close).toHaveFocus();

  save.focus();
  expect(screen.getByRole("button", { name: "Background action" })).not.toHaveFocus();
});

test("focuses and traps keyboard navigation inside Settings", async () => {
  render(<NestedDialogs />);

  const sources = screen.getByRole("button", { name: "Sources" });
  const close = screen.getByRole("button", { name: "Close settings" });
  const background = screen.getByRole("button", { name: "Background action" });
  await waitFor(() => expect(sources).toHaveFocus());

  close.focus();
  fireEvent.keyDown(close, { key: "Tab" });
  expect(sources).toHaveFocus();

  sources.focus();
  fireEvent.keyDown(sources, { key: "Tab", shiftKey: true });
  expect(close).toHaveFocus();
  expect(background).not.toHaveFocus();
});
