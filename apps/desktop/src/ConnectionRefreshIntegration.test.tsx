import {
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

function emptyDashboard() {
  return {
    today: [],
    inbox: [],
    upcoming: [],
    attention: [],
    sourceHealth: { sourceCount: 0, issues: [] },
  };
}

test("surfaces a GitHub post-authorization refresh failure through App", async () => {
  let githubReads = 0;
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard") return Promise.resolve(emptyDashboard());
    if (command === "widget_layout") return Promise.resolve(undefined);
    if (
      command === "slack_connections" ||
      command === "notion_connections" ||
      command === "google_connections" ||
      command === "gitlab_connections"
    ) {
      return Promise.resolve([]);
    }
    if (command === "github_connections") {
      githubReads += 1;
      if (githubReads === 1) return Promise.resolve([]);
      return Promise.reject(new Error("GitHub refresh failed"));
    }
    if (command === "start_github_connection") {
      return Promise.resolve({
        sessionId: "github-refresh-session",
        userCode: "REFRESH-GH",
        verificationUri: "https://github.com/login/device",
        expiresAt: "2026-08-14T12:00:00Z",
        retryAfterSeconds: 0,
      });
    }
    if (command === "poll_github_connection") {
      return Promise.resolve({ status: "authorized", retryAfterSeconds: null });
    }
    return Promise.resolve(undefined);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Open settings" }));
  fireEvent.click(await screen.findByRole("button", { name: "Connect GitHub" }));

  expect(await screen.findByText(/GitHub refresh failed/)).toBeInTheDocument();
  expect(githubReads).toBe(2);
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Connect GitHub" })).toBeEnabled(),
  );
});

test("surfaces a GitLab PAT refresh failure through App after connection succeeds", async () => {
  let gitlabReads = 0;
  mocks.invoke.mockImplementation((command: string) => {
    if (command === "dashboard") return Promise.resolve(emptyDashboard());
    if (command === "widget_layout") return Promise.resolve(undefined);
    if (
      command === "slack_connections" ||
      command === "notion_connections" ||
      command === "google_connections" ||
      command === "github_connections"
    ) {
      return Promise.resolve([]);
    }
    if (command === "gitlab_connections") {
      gitlabReads += 1;
      if (gitlabReads === 1) return Promise.resolve([]);
      return Promise.reject(new Error("GitLab refresh failed"));
    }
    if (command === "connect_gitlab_pat") return Promise.resolve(undefined);
    return Promise.resolve(undefined);
  });

  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "Open settings" }));
  fireEvent.click(await screen.findByRole("button", { name: "Connect GitLab" }));
  fireEvent.click(await screen.findByRole("tab", { name: "Self-managed" }));
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
  expect(await screen.findByText(/GitLab refresh failed/)).toBeInTheDocument();
  expect(gitlabReads).toBe(2);
  expect(
    screen.queryByRole("dialog", { name: "Connect GitLab" }),
  ).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Connect GitLab" })).toBeEnabled();
});
