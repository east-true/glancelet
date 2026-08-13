import { invoke } from "@tauri-apps/api/core";

export type WorkKind = "action" | "event" | "attention";
export type WorkProgress = "todo" | "doing" | "done";
export type Freshness = "never_synced" | "fresh" | "stale";
export type SyncFailureKind =
  | "authentication_required"
  | "configuration_required"
  | "rate_limited"
  | "transient_network"
  | "provider_failure"
  | "other";

export interface SyncSourceSuccess {
  sourceId: string;
  sourceName: string;
  changedEntities: number;
}

export interface SyncSourceFailure {
  sourceId: string;
  sourceName: string;
  kind: SyncFailureKind;
  message: string;
  nextRetryAt: string | null;
}

export interface SyncReport {
  refreshRequired: boolean;
  succeeded: SyncSourceSuccess[];
  failed: SyncSourceFailure[];
  projectionFailures: string[];
}
export type WorkAction =
  | "plan"
  | "move_to_inbox"
  | "move_to_backlog"
  | "snooze"
  | "dismiss"
  | "pin"
  | "unpin"
  | "start_work"
  | "complete"
  | "open_source";

export type Planning =
  { type: "inbox" } | { type: "backlog" } | { type: "planned"; date: string };

export interface WorkView {
  id: string;
  kind: WorkKind;
  title: string;
  summary: string | null;
  priority: number | null;
  lifecycle: "active" | "resolved";
  progress: WorkProgress | null;
  planning: Planning | null;
  disposition: "normal" | "snoozed" | "dismissed";
  pinned: boolean;
  snoozedUntil: string | null;
  start: unknown | null;
  end: unknown | null;
  due: unknown | null;
  source: {
    providerId: string;
    providerName: string;
    sourceName: string;
    configName: string;
  };
  canNavigate: boolean;
  freshness: Freshness;
  dimensions: Record<string, string | number | boolean>;
  facets: Record<string, unknown>;
  availableActions: WorkAction[];
}

export interface WorkDashboard {
  today: WorkView[];
  inbox: WorkView[];
  upcoming: UpcomingWorkView[];
  attention: WorkView[];
  sourceHealth: SourceHealth;
}

export interface UpcomingWorkView {
  date: string;
  basis: "event" | "planned" | "due";
  work: WorkView;
}

export interface SourceHealth {
  sourceCount: number;
  issues: {
    sourceId: string;
    sourceName: string;
    kind: SyncFailureKind;
  }[];
}

export type WidgetType = "today" | "inbox" | "upcoming" | "attention";
export type WidgetSize = "compact" | "wide" | "tall";

export interface WidgetInstance {
  widgetType: WidgetType;
  position: number;
  size: WidgetSize;
  settings: Record<string, unknown>;
}

export interface DesktopSettings {
  alwaysOnTop: boolean;
  launchAtStartup: boolean;
}

export interface SlackConnection {
  connectionId: string;
  sourceId: string | null;
  workspace: string;
  user: string;
  reactionName: string;
  enabled: boolean;
  status: "connected" | "reauth_required" | "disconnected";
  lastSync: string | null;
  lastError: string | null;
}

export interface NotionPropertyMapping {
  id: string;
  name: string;
}

export interface NotionSourceSettings {
  dataSourceId: string;
  dataSourceName: string;
  properties: {
    title: NotionPropertyMapping;
    assignee: NotionPropertyMapping | null;
    status: NotionPropertyMapping | null;
    due: NotionPropertyMapping | null;
  };
  onlyAssignedToMe: boolean;
  activeStatusIds: string[];
}

export interface NotionStatusSchema {
  options: { id: string; name: string }[];
  groups: { id: string; name: string; optionIds: string[] }[];
}

export interface NotionPropertySchema {
  id: string;
  name: string;
  type: string;
  status: NotionStatusSchema | null;
}

export interface NotionDataSource {
  id: string;
  title: string;
  properties: NotionPropertySchema[];
}

export interface NotionSource {
  sourceId: string;
  dataSourceId: string;
  name: string;
  enabled: boolean;
  settings: NotionSourceSettings;
  lastSync: string | null;
  lastError: string | null;
}

export interface NotionConnection {
  connectionId: string;
  user: string;
  status: "connected" | "reauth_required" | "disconnected";
  sources: NotionSource[];
}

export interface NotionDataSourceSummary {
  id: string;
  title: string;
}

export interface NotionPreviewRow {
  externalId: string;
  title: string;
  status: string | null;
  due: unknown | null;
}

export interface GoogleCalendar {
  id: string;
  summary: string;
  summaryOverride: string | null;
  timeZone: string | null;
  primary: boolean;
  selected: boolean;
}

export interface GoogleCalendarSource {
  sourceId: string;
  calendarId: string;
  name: string;
  enabled: boolean;
  lastSync: string | null;
  lastError: string | null;
}

export interface GoogleConnection {
  connectionId: string;
  email: string;
  status: "connected" | "reauth_required" | "disconnected";
  sources: GoogleCalendarSource[];
}

export interface GithubDeviceAuthorization {
  sessionId: string;
  userCode: string;
  verificationUri: string;
  expiresAt: string;
  retryAfterSeconds: number;
}

export interface GithubDevicePoll {
  status: "pending" | "authorized";
  retryAfterSeconds: number | null;
}

export interface GithubRepository {
  id: number;
  nodeId: string;
  fullName: string;
  defaultBranch: string;
}

export interface GithubSource {
  sourceId: string;
  sourceType:
    | "github.review_requests"
    | "github.assigned_issues"
    | "github.workflow_failures";
  name: string;
  repository: string | null;
  enabled: boolean;
  lastSync: string | null;
  lastError: string | null;
}

export interface GithubConnection {
  connectionId: string;
  login: string;
  status: "connected" | "reauth_required" | "disconnected";
  sources: GithubSource[];
}

export interface GitlabDeviceAuthorization {
  sessionId: string;
  userCode: string;
  verificationUri: string;
  verificationUriComplete: string | null;
  expiresAt: string;
  retryAfterSeconds: number;
}

export interface GitlabDevicePoll {
  status: "pending" | "authorized";
  retryAfterSeconds: number | null;
}

export interface GitlabSource {
  sourceId: string;
  name: string;
  enabled: boolean;
  lastSync: string | null;
  lastError: string | null;
}

export interface GitlabConnection {
  connectionId: string;
  username: string;
  instanceOrigin: string;
  instanceLabel: string;
  authMode: "oauth" | "pat";
  status: "connected" | "reauth_required" | "disconnected";
  source: GitlabSource | null;
}

export type WorkCommand =
  | { type: "plan"; date: string }
  | { type: "move_to_inbox" }
  | { type: "move_to_backlog" }
  | { type: "snooze"; until: string }
  | { type: "dismiss" }
  | { type: "pin" }
  | { type: "unpin" }
  | { type: "start_work" }
  | { type: "complete" };

export function syncReportMessage(
  report: SyncReport | null | undefined,
): string | null {
  if (!report) return null;
  const failures = report.failed.map((failure) => {
    const retry = failure.nextRetryAt
      ? ` (next retry ${failure.nextRetryAt})`
      : "";
    return `${failure.sourceName}: ${failure.message}${retry}`;
  });
  failures.push(
    ...report.projectionFailures.map((failure) => `Projection: ${failure}`),
  );
  return failures.length > 0 ? failures.join("; ") : null;
}

export const glanceletApi = {
  dashboard: () => invoke<WorkDashboard>("dashboard"),
  widgetLayout: () => invoke<WidgetInstance[]>("widget_layout"),
  saveWidgetLayout: (widgets: WidgetInstance[]) =>
    invoke<void>("save_widget_layout", { widgets }),
  desktopSettings: () => invoke<DesktopSettings>("desktop_settings"),
  setAlwaysOnTop: (enabled: boolean) =>
    invoke<void>("set_always_on_top", { enabled }),
  setLaunchAtStartup: (enabled: boolean) =>
    invoke<void>("set_launch_at_startup", { enabled }),
  sync: () => invoke<SyncReport>("sync_all"),
  slackConnections: () => invoke<SlackConnection[]>("slack_connections"),
  connectSlack: () => invoke<void>("connect_slack"),
  syncSource: (sourceId: string) =>
    invoke<SyncReport>("sync_source", { sourceId }),
  updateSlackSource: (
    sourceId: string,
    reactionName: string,
    enabled: boolean,
  ) =>
    invoke<void>("update_slack_source", {
      sourceId,
      reactionName,
      enabled,
    }),
  disconnectSlack: (connectionId: string) =>
    invoke<void>("disconnect_slack", { connectionId }),
  notionConnections: () => invoke<NotionConnection[]>("notion_connections"),
  connectNotion: (token: string) => invoke<void>("connect_notion", { token }),
  searchNotionDataSources: (connectionId: string, query: string) =>
    invoke<NotionDataSourceSummary[]>("search_notion_data_sources", {
      connectionId,
      query,
    }),
  notionDataSourceSchema: (connectionId: string, dataSourceId: string) =>
    invoke<NotionDataSource>("notion_data_source_schema", {
      connectionId,
      dataSourceId,
    }),
  previewNotionSource: (connectionId: string, settings: NotionSourceSettings) =>
    invoke<NotionPreviewRow[]>("preview_notion_source", {
      connectionId,
      settings,
    }),
  saveNotionSource: (
    connectionId: string,
    sourceId: string | null,
    settings: NotionSourceSettings,
  ) =>
    invoke<string>("save_notion_source", {
      connectionId,
      sourceId,
      settings,
    }),
  updateNotionSource: (sourceId: string, enabled: boolean) =>
    invoke<void>("update_notion_source", { sourceId, enabled }),
  removeNotionSource: (sourceId: string) =>
    invoke<void>("remove_notion_source", { sourceId }),
  disconnectNotion: (connectionId: string) =>
    invoke<void>("disconnect_notion", { connectionId }),
  googleConnections: () => invoke<GoogleConnection[]>("google_connections"),
  connectGoogle: () => invoke<void>("connect_google"),
  googleCalendars: (connectionId: string) =>
    invoke<GoogleCalendar[]>("google_calendars", { connectionId }),
  saveGoogleCalendars: (connectionId: string, calendarIds: string[]) =>
    invoke<string[]>("save_google_calendars", {
      connectionId,
      selections: calendarIds.map((calendarId) => ({ calendarId })),
    }),
  updateGoogleSource: (sourceId: string, enabled: boolean) =>
    invoke<void>("update_google_source", { sourceId, enabled }),
  removeGoogleSource: (sourceId: string) =>
    invoke<void>("remove_google_source", { sourceId }),
  disconnectGoogle: (connectionId: string) =>
    invoke<void>("disconnect_google", { connectionId }),
  githubConnections: () => invoke<GithubConnection[]>("github_connections"),
  startGithubConnection: () =>
    invoke<GithubDeviceAuthorization>("start_github_connection"),
  pollGithubConnection: (sessionId: string) =>
    invoke<GithubDevicePoll>("poll_github_connection", { sessionId }),
  cancelGithubConnection: (sessionId: string) =>
    invoke<void>("cancel_github_connection", { sessionId }),
  githubRepositories: (connectionId: string) =>
    invoke<GithubRepository[]>("github_repositories", { connectionId }),
  saveGithubGlobalSource: (connectionId: string, sourceType: string) =>
    invoke<string>("save_github_global_source", {
      connectionId,
      sourceType,
    }),
  saveGithubWorkflowSource: (connectionId: string, repositoryId: number) =>
    invoke<string>("save_github_workflow_source", {
      connectionId,
      repositoryId,
    }),
  updateGithubSource: (sourceId: string, enabled: boolean) =>
    invoke<void>("update_github_source", { sourceId, enabled }),
  removeGithubSource: (sourceId: string) =>
    invoke<void>("remove_github_source", { sourceId }),
  disconnectGithub: (connectionId: string) =>
    invoke<void>("disconnect_github", { connectionId }),
  gitlabConnections: () => invoke<GitlabConnection[]>("gitlab_connections"),
  startGitlabConnection: () =>
    invoke<GitlabDeviceAuthorization>("start_gitlab_connection"),
  pollGitlabConnection: (sessionId: string) =>
    invoke<GitlabDevicePoll>("poll_gitlab_connection", { sessionId }),
  cancelGitlabConnection: (sessionId: string) =>
    invoke<void>("cancel_gitlab_connection", { sessionId }),
  connectGitlabPat: (instanceUrl: string, token: string) =>
    invoke<void>("connect_gitlab_pat", { instanceUrl, token }),
  saveGitlabTodosSource: (connectionId: string) =>
    invoke<string>("save_gitlab_todos_source", { connectionId }),
  updateGitlabSource: (sourceId: string, enabled: boolean) =>
    invoke<void>("update_gitlab_source", { sourceId, enabled }),
  removeGitlabSource: (sourceId: string) =>
    invoke<void>("remove_gitlab_source", { sourceId }),
  disconnectGitlab: (connectionId: string) =>
    invoke<void>("disconnect_gitlab", { connectionId }),
  command: (workId: string, command: WorkCommand) =>
    invoke<void>("run_work_command", { workId, command }),
  openSource: (workId: string) => invoke<void>("open_source", { workId }),
};
