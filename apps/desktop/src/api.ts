import { invoke } from "@tauri-apps/api/core";

export type WorkKind = "action" | "event" | "attention";
export type WorkProgress = "todo" | "doing" | "done";
export type Freshness = "never_synced" | "fresh" | "stale";
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

export const glanceletApi = {
  dashboard: () => invoke<WorkDashboard>("dashboard"),
  sync: () => invoke<void>("sync_all"),
  slackConnections: () => invoke<SlackConnection[]>("slack_connections"),
  connectSlack: () => invoke<void>("connect_slack"),
  syncSource: (sourceId: string) => invoke<void>("sync_source", { sourceId }),
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
  command: (workId: string, command: WorkCommand) =>
    invoke<void>("run_work_command", { workId, command }),
  openSource: (workId: string) => invoke<void>("open_source", { workId }),
};
