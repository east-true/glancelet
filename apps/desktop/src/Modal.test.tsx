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
      </Modal>
    </SettingsOverlay>
  );
}

test("keeps Escape scoped to the topmost dialog and restores focus", async () => {
  render(<NestedDialogs />);
  const opener = screen.getByRole("button", { name: "Open nested modal" });
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
