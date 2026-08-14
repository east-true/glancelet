import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { GitlabSettings } from "./GitlabSettings";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));

beforeEach(() => mocks.invoke.mockReset());
afterEach(cleanup);

test("shows a refresh failure after self-managed GitLab connection succeeds", async () => {
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "connect_gitlab_pat") return Promise.resolve(undefined);
    return Promise.resolve(undefined);
  });
  const refresh = vi.fn().mockRejectedValue(new Error("GitLab refresh failed"));

  render(
    <GitlabSettings
      busy={false}
      connections={[]}
      refresh={refresh}
      refreshWork={vi.fn().mockResolvedValue(undefined)}
      setError={vi.fn()}
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "Connect GitLab" }));
  fireEvent.click(screen.getByRole("tab", { name: "Self-managed" }));
  fireEvent.change(screen.getByRole("textbox", { name: "GitLab instance URL" }), {
    target: { value: "https://gitlab.example.com" },
  });
  fireEvent.change(screen.getByLabelText("GitLab Personal Access Token"), {
    target: { value: "dummy-token" },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Connect self-managed GitLab" }),
  );

  await waitFor(() =>
    expect(mocks.invoke).toHaveBeenCalledWith("connect_gitlab_pat", {
      instanceUrl: "https://gitlab.example.com",
      token: "dummy-token",
    }),
  );
  await waitFor(() =>
    expect(
      screen.queryByRole("dialog", { name: "Connect GitLab" }),
    ).not.toBeInTheDocument(),
  );
  expect(await screen.findByText(/GitLab refresh failed/)).toBeInTheDocument();
  expect(refresh).toHaveBeenCalledTimes(1);
});
