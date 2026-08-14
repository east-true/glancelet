import { useEffect, useMemo, useState, type DragEvent } from "react";
import {
  type UpcomingWorkView,
  type WidgetInstance,
  type WidgetSize,
  type WidgetType,
  type WorkAction,
  type WorkCommand,
  type WorkDashboard,
  type WorkView,
} from "./api";
import { localDateString } from "./local-time";

const widgetNames: Record<WidgetType, string> = {
  today: "Today",
  inbox: "Inbox",
  upcoming: "Upcoming",
  attention: "Attention",
};

const emptyCopy: Record<WidgetType, string> = {
  today: "Nothing needs your attention today.",
  inbox: "Inbox is clear.",
  upcoming: "Nothing coming up.",
  attention: "No active alerts.",
};

const allWidgetTypes = Object.keys(widgetNames) as WidgetType[];
const maxVisibleItems = 12;

export function DesktopSurface({
  data,
  layout,
  loading,
  editing,
  pendingWorkIds,
  onEdit,
  onLayout,
  onRun,
  onOpen,
  onSources,
}: {
  data: WorkDashboard;
  layout: WidgetInstance[];
  loading: boolean;
  editing: boolean;
  pendingWorkIds: Set<string>;
  onEdit: (editing: boolean) => void;
  onLayout: (layout: WidgetInstance[]) => Promise<void>;
  onRun: (id: string, command: WorkCommand) => Promise<void>;
  onOpen: (id: string) => Promise<void>;
  onSources: () => void;
}) {
  const [dragged, setDragged] = useState<WidgetType | null>(null);
  const ordered = useMemo(
    () => [...layout].sort((a, b) => a.position - b.position),
    [layout],
  );
  const workCount = new Set([
    ...data.today.map((work) => work.id),
    ...data.inbox.map((work) => work.id),
    ...data.attention.map((work) => work.id),
    ...data.upcoming.map((item) => item.work.id),
  ]).size;
  const staleSourceCount = new Set(
    [
      ...data.today,
      ...data.inbox,
      ...data.attention,
      ...data.upcoming.map((item) => item.work),
    ]
      .filter((work) => work.freshness !== "fresh")
      .map((work) => `${work.source.providerId}:${work.source.configName}`),
  ).size;

  useEffect(() => {
    if (!editing) return;
    const leaveEditMode = (event: KeyboardEvent) => {
      if (event.key === "Escape") onEdit(false);
    };
    window.addEventListener("keydown", leaveEditMode);
    return () => window.removeEventListener("keydown", leaveEditMode);
  }, [editing, onEdit]);

  function normalized(next: WidgetInstance[]) {
    return next.map((widget, position) => ({ ...widget, position }));
  }

  async function reorder(source: WidgetType, target: WidgetType) {
    if (source === target) return;
    const next = [...ordered];
    const sourceIndex = next.findIndex((item) => item.widgetType === source);
    const targetIndex = next.findIndex((item) => item.widgetType === target);
    if (sourceIndex < 0 || targetIndex < 0) return;
    const [item] = next.splice(sourceIndex, 1);
    next.splice(targetIndex, 0, item);
    await onLayout(normalized(next));
  }

  async function move(widgetType: WidgetType, direction: -1 | 1) {
    const index = ordered.findIndex((item) => item.widgetType === widgetType);
    const target = index + direction;
    if (index < 0 || target < 0 || target >= ordered.length) return;
    const next = [...ordered];
    [next[index], next[target]] = [next[target], next[index]];
    await onLayout(normalized(next));
  }

  async function add(widgetType: WidgetType) {
    await onLayout(
      normalized([
        ...ordered,
        {
          widgetType,
          position: ordered.length,
          size: widgetType === "today" ? "wide" : "compact",
          settings: {},
        },
      ]),
    );
  }

  async function remove(widgetType: WidgetType) {
    if (ordered.length === 1) return;
    await onLayout(
      normalized(ordered.filter((item) => item.widgetType !== widgetType)),
    );
  }

  async function resize(widgetType: WidgetType) {
    const sizes: WidgetSize[] = ["compact", "wide", "tall"];
    await onLayout(
      ordered.map((item) =>
        item.widgetType === widgetType
          ? {
              ...item,
              size: sizes[(sizes.indexOf(item.size) + 1) % sizes.length],
            }
          : item,
      ),
    );
  }

  function drop(event: DragEvent, target: WidgetType) {
    event.preventDefault();
    const source =
      dragged ?? (event.dataTransfer.getData("text/plain") as WidgetType);
    setDragged(null);
    if (allWidgetTypes.includes(source)) void reorder(source, target);
  }

  return (
    <section className="desktop-surface" aria-label="Desktop Surface">
      <div className="surface-toolbar">
        <p className="surface-summary">
          {workCount === 0 ? "Quiet right now" : `${workCount} items in view`}
        </p>
      </div>

      {data.sourceHealth.issues.length > 0 && (
        <button className="health-banner" onClick={onSources}>
          <span aria-hidden="true">!</span>
          {data.sourceHealth.issues.length} source
          {data.sourceHealth.issues.length === 1 ? " needs" : "s need"}{" "}
          attention
          <b>Review</b>
        </button>
      )}

      {data.sourceHealth.issues.length === 0 && staleSourceCount > 0 && (
        <button className="health-banner freshness-banner" onClick={onSources}>
          <span aria-hidden="true">↻</span>
          {staleSourceCount} source{staleSourceCount === 1 ? " may" : "s may"}{" "}
          be stale
          <b>Review</b>
        </button>
      )}

      {!loading && data.sourceHealth.sourceCount === 0 && workCount === 0 && (
        <div className="first-run-card">
          <span>Your work will appear here.</span>
          <p>
            Connect a source to start collecting work that needs your attention.
          </p>
          <button className="btn-primary" onClick={onSources}>
            Connect a Source
          </button>
        </div>
      )}

      {loading ? (
        <div className="surface-loading">Loading your workspace…</div>
      ) : (
        <div className={`widget-grid ${editing ? "editing" : ""}`}>
          {ordered.map((instance, index) => (
            <section
              key={instance.widgetType}
              className={`widget widget-${instance.size}`}
              draggable={editing}
              onDragStart={(event) => {
                setDragged(instance.widgetType);
                event.dataTransfer.setData("text/plain", instance.widgetType);
                event.dataTransfer.effectAllowed = "move";
              }}
              onDragOver={(event) => editing && event.preventDefault()}
              onDrop={(event) => editing && drop(event, instance.widgetType)}
            >
              <WidgetHeader
                instance={instance}
                editing={editing}
                first={index === 0}
                last={index === ordered.length - 1}
                move={move}
                resize={resize}
                remove={remove}
              />
              <WidgetBody
                type={instance.widgetType}
                data={data}
                pendingWorkIds={pendingWorkIds}
                run={onRun}
                open={onOpen}
              />
            </section>
          ))}
          {editing && (
            <section className="add-widget-panel" aria-label="Add Widget">
              <strong>Add Widget</strong>
              <div>
                {allWidgetTypes.map((type) => {
                  const added = ordered.some(
                    (item) => item.widgetType === type,
                  );
                  return (
                    <button
                      key={type}
                      disabled={added}
                      onClick={() => void add(type)}
                    >
                      {widgetNames[type]}
                      <small>{added ? "Added" : "+ Add"}</small>
                    </button>
                  );
                })}
              </div>
            </section>
          )}
        </div>
      )}
    </section>
  );
}

function WidgetHeader({
  instance,
  editing,
  first,
  last,
  move,
  resize,
  remove,
}: {
  instance: WidgetInstance;
  editing: boolean;
  first: boolean;
  last: boolean;
  move: (type: WidgetType, direction: -1 | 1) => Promise<void>;
  resize: (type: WidgetType) => Promise<void>;
  remove: (type: WidgetType) => Promise<void>;
}) {
  return (
    <header className="widget-header">
      <div>
        {editing && (
          <span className="drag-handle" aria-hidden="true">
            ⠿
          </span>
        )}
        <h2>{widgetNames[instance.widgetType]}</h2>
      </div>
      {editing && (
        <div className="layout-actions">
          <button
            aria-label={`Move ${widgetNames[instance.widgetType]} up`}
            disabled={first}
            onClick={() => void move(instance.widgetType, -1)}
          >
            ↑
          </button>
          <button
            aria-label={`Move ${widgetNames[instance.widgetType]} down`}
            disabled={last}
            onClick={() => void move(instance.widgetType, 1)}
          >
            ↓
          </button>
          <button
            aria-label={`Resize ${widgetNames[instance.widgetType]}`}
            onClick={() => void resize(instance.widgetType)}
          >
            {instance.size}
          </button>
          <button
            aria-label={`Remove ${widgetNames[instance.widgetType]}`}
            onClick={() => void remove(instance.widgetType)}
          >
            ×
          </button>
        </div>
      )}
    </header>
  );
}

function WidgetBody({
  type,
  data,
  pendingWorkIds,
  run,
  open,
}: {
  type: WidgetType;
  data: WorkDashboard;
  pendingWorkIds: Set<string>;
  run: (id: string, command: WorkCommand) => Promise<void>;
  open: (id: string) => Promise<void>;
}) {
  if (type === "upcoming") {
    if (data.upcoming.length === 0) return <WidgetEmpty type={type} />;
    const groups = groupUpcoming(data.upcoming.slice(0, maxVisibleItems));
    return (
      <div className="widget-list upcoming-list">
        {groups.map(([date, items]) => (
          <div className="upcoming-group" key={date}>
            <h3>{dateLabel(date)}</h3>
            {items.map((item) => (
              <WorkItem
                key={`${item.work.id}-${item.basis}-${item.date}`}
                work={item.work}
                context={
                  item.basis === "due"
                    ? "Due"
                    : item.basis === "planned"
                      ? "Planned"
                      : undefined
                }
                pending={pendingWorkIds.has(item.work.id)}
                run={run}
                open={open}
              />
            ))}
          </div>
        ))}
        <Overflow count={data.upcoming.length - maxVisibleItems} />
      </div>
    );
  }
  const items = data[type];
  if (items.length === 0) return <WidgetEmpty type={type} />;
  return (
    <div className="widget-list">
      {items.slice(0, maxVisibleItems).map((work) => (
        <WorkItem
          key={work.id}
          work={work}
          pending={pendingWorkIds.has(work.id)}
          run={run}
          open={open}
        />
      ))}
      <Overflow count={items.length - maxVisibleItems} />
    </div>
  );
}

function WidgetEmpty({ type }: { type: WidgetType }) {
  return <p className="widget-empty">{emptyCopy[type]}</p>;
}

function Overflow({ count }: { count: number }) {
  return count > 0 ? <p className="item-overflow">+ {count} more</p> : null;
}

function WorkItem({
  work,
  context,
  pending,
  run,
  open,
}: {
  work: WorkView;
  context?: string;
  pending: boolean;
  run: (id: string, command: WorkCommand) => Promise<void>;
  open: (id: string) => Promise<void>;
}) {
  const supports = (action: WorkAction) =>
    work.availableActions.includes(action);
  const today = localDateString(new Date());
  const tomorrowDate = new Date();
  tomorrowDate.setDate(tomorrowDate.getDate() + 1);
  const tomorrow = localDateString(tomorrowDate);
  const snoozeUntil = new Date(tomorrowDate);
  snoozeUntil.setHours(9, 0, 0, 0);
  const time = work.kind === "event" ? temporalTime(work.start) : null;

  return (
    <article className={`surface-work kind-${work.kind}`} aria-busy={pending}>
      <button
        className={`surface-work-main ${time ? "with-time" : ""}`}
        disabled={pending || !work.canNavigate}
        onClick={() => void open(work.id)}
      >
        <span className="kind-dot" aria-hidden="true" />
        {time && <time>{time}</time>}
        <span className="surface-work-copy">
          <strong>{work.title}</strong>
          <small>
            {context ? `${context} · ` : ""}
            {work.pinned ? "Pinned · " : ""}
            {work.source.configName}
          </small>
        </span>
        {work.canNavigate && <span aria-hidden="true">↗</span>}
      </button>
      <div className="quick-actions">
        {supports("plan") && (
          <>
            <button
              disabled={pending}
              onClick={() => void run(work.id, { type: "plan", date: today })}
            >
              Today
            </button>
            <button
              disabled={pending}
              onClick={() =>
                void run(work.id, { type: "plan", date: tomorrow })
              }
            >
              Tomorrow
            </button>
          </>
        )}
        {supports("move_to_backlog") && (
          <button
            disabled={pending}
            onClick={() => void run(work.id, { type: "move_to_backlog" })}
          >
            Backlog
          </button>
        )}
        {supports("snooze") && (
          <button
            disabled={pending}
            onClick={() =>
              void run(work.id, {
                type: "snooze",
                until: snoozeUntil.toISOString(),
              })
            }
          >
            Snooze
          </button>
        )}
        {supports("start_work") && (
          <button
            disabled={pending}
            onClick={() => void run(work.id, { type: "start_work" })}
          >
            Start
          </button>
        )}
        {supports("complete") && (
          <button
            disabled={pending}
            onClick={() => void run(work.id, { type: "complete" })}
          >
            Complete
          </button>
        )}
        {(supports("pin") || supports("unpin")) && (
          <button
            aria-label={
              work.pinned ? `Unpin ${work.title}` : `Pin ${work.title}`
            }
            disabled={pending}
            onClick={() =>
              void run(work.id, { type: work.pinned ? "unpin" : "pin" })
            }
          >
            {work.pinned ? "Unpin" : "Pin"}
          </button>
        )}
        {supports("dismiss") && (
          <button
            className="secondary-action"
            disabled={pending}
            onClick={() => void run(work.id, { type: "dismiss" })}
          >
            Dismiss
          </button>
        )}
        {supports("open_source") && (
          <button disabled={pending} onClick={() => void open(work.id)}>
            Open
          </button>
        )}
      </div>
    </article>
  );
}

function groupUpcoming(items: UpcomingWorkView[]) {
  const groups = new Map<string, UpcomingWorkView[]>();
  for (const item of items)
    groups.set(item.date, [...(groups.get(item.date) ?? []), item]);
  return [...groups.entries()];
}

function dateLabel(date: string) {
  const today = localDateString(new Date());
  const tomorrowDate = new Date();
  tomorrowDate.setDate(tomorrowDate.getDate() + 1);
  if (date === today) return "Today";
  if (date === localDateString(tomorrowDate)) return "Tomorrow";
  return new Intl.DateTimeFormat(undefined, {
    weekday: "long",
    month: "short",
    day: "numeric",
    timeZone: "UTC",
  }).format(new Date(`${date}T00:00:00Z`));
}

function temporalTime(value: unknown) {
  if (!value || typeof value !== "object") return null;
  const temporal = value as { type?: string; instant?: string };
  if (temporal.type !== "date_time" || !temporal.instant) return null;
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(temporal.instant));
}
