use std::sync::Arc;

use chrono::{DateTime, Days, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    application::{Clock, StoredWork, TimeContext, WorkMutation, WorkStore},
    domain::{
        LocalDisposition, ProgressAuthority, TemporalValue, WorkKind, WorkLifecycle, WorkPlanning,
        WorkProgress,
    },
    extension::ExtensionRegistry,
    GlanceletError, Result,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    NeverSynced,
    Fresh,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkAction {
    Plan,
    MoveToInbox,
    MoveToBacklog,
    Snooze,
    Dismiss,
    Pin,
    Unpin,
    StartWork,
    Complete,
    OpenSource,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    pub provider_id: String,
    pub provider_name: String,
    pub source_name: String,
    pub config_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkView {
    pub id: String,
    pub kind: WorkKind,
    pub title: String,
    pub summary: Option<String>,
    pub priority: Option<i32>,
    pub lifecycle: WorkLifecycle,
    pub progress: Option<WorkProgress>,
    pub planning: Option<WorkPlanning>,
    pub disposition: LocalDisposition,
    pub pinned: bool,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub start: Option<TemporalValue>,
    pub end: Option<TemporalValue>,
    pub due: Option<TemporalValue>,
    pub source: SourceView,
    pub can_navigate: bool,
    pub freshness: Freshness,
    pub dimensions: Value,
    pub facets: Value,
    pub available_actions: Vec<WorkAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkDashboard {
    pub today: Vec<WorkView>,
    pub inbox: Vec<WorkView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpcomingBasis {
    Event,
    Planned,
    Due,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpcomingWorkView {
    pub date: NaiveDate,
    pub basis: UpcomingBasis,
    pub work: WorkView,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceHealthIssue {
    pub source_id: String,
    pub source_name: String,
    pub kind: super::SourceFailureKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceHealthView {
    pub source_count: usize,
    pub issues: Vec<SourceHealthIssue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkWidgets {
    pub today: Vec<WorkView>,
    pub inbox: Vec<WorkView>,
    pub upcoming: Vec<UpcomingWorkView>,
    pub attention: Vec<WorkView>,
    pub source_health: SourceHealthView,
}

pub struct WorkReadService {
    store: Arc<dyn WorkStore>,
    registry: Arc<ExtensionRegistry>,
    clock: Arc<dyn Clock>,
    time_context: TimeContext,
}

impl WorkReadService {
    pub fn new(
        store: Arc<dyn WorkStore>,
        registry: Arc<ExtensionRegistry>,
        clock: Arc<dyn Clock>,
        time_context: TimeContext,
    ) -> Self {
        Self {
            store,
            registry,
            clock,
            time_context,
        }
    }

    pub fn dashboard(&self) -> Result<WorkDashboard> {
        let widgets = self.widgets(7)?;
        Ok(WorkDashboard {
            today: widgets.today,
            inbox: widgets.inbox,
        })
    }

    pub fn widgets(&self, upcoming_days: u64) -> Result<WorkWidgets> {
        self.widgets_with_privacy(upcoming_days, false)
    }

    pub fn widgets_with_privacy(
        &self,
        upcoming_days: u64,
        privacy_mode: bool,
    ) -> Result<WorkWidgets> {
        let now = self.clock.now();
        let today = self.time_context.local_date(now);
        let mut today_items = Vec::new();
        let mut inbox_items = Vec::new();
        let mut upcoming_items = Vec::new();
        let mut attention_items = Vec::new();

        for stored in self.store.dashboard_work(now)? {
            if !is_visible(&stored, now) {
                continue;
            }
            let is_today = belongs_to_today(&stored, today, self.time_context);
            let is_inbox = stored.entry.kind == WorkKind::Action
                && stored.entry.planning == Some(WorkPlanning::Inbox);
            let upcoming = upcoming_entries(&stored, today, upcoming_days, self.time_context);
            let is_attention = stored.entry.kind == WorkKind::Attention;
            let view = self.to_view(stored, now, privacy_mode)?;
            if is_today {
                today_items.push(view.clone());
            }
            if is_inbox {
                inbox_items.push(view.clone());
            }
            if is_attention {
                attention_items.push(view.clone());
            }
            for (date, basis) in upcoming {
                upcoming_items.push(UpcomingWorkView {
                    date,
                    basis,
                    work: view.clone(),
                });
            }
        }

        sort_views(&mut today_items);
        sort_views(&mut inbox_items);
        sort_views(&mut attention_items);
        upcoming_items.sort_by(|a, b| {
            a.date
                .cmp(&b.date)
                .then_with(|| upcoming_rank(a.basis).cmp(&upcoming_rank(b.basis)))
                .then_with(|| a.work.title.cmp(&b.work.title))
        });

        let mut issues = Vec::new();
        let source_configs = self.store.source_configs()?;
        let active_sources = source_configs
            .into_iter()
            .filter(|source| source.enabled && source.removed_at.is_none())
            .collect::<Vec<_>>();
        for source in &active_sources {
            let runtime = self.store.source_runtime(&source.id)?;
            if let Some(kind) = runtime.failure_kind {
                issues.push(SourceHealthIssue {
                    source_id: source.id.clone(),
                    source_name: source.display_name.clone(),
                    kind,
                });
            }
        }
        if privacy_mode {
            for (index, issue) in issues.iter_mut().enumerate() {
                issue.source_id = format!("private-source-{index}");
                issue.source_name = "Private source".into();
            }
        }

        Ok(WorkWidgets {
            today: today_items,
            inbox: inbox_items,
            upcoming: upcoming_items,
            attention: attention_items,
            source_health: SourceHealthView {
                source_count: active_sources.len(),
                issues,
            },
        })
    }

    fn to_view(
        &self,
        stored: StoredWork,
        now: DateTime<Utc>,
        privacy_mode: bool,
    ) -> Result<WorkView> {
        let metadata = self
            .registry
            .display_metadata(&stored.source_config.source_type_id)?;
        let freshness = match stored.runtime.last_success_at {
            None => Freshness::NeverSynced,
            Some(last_success) => {
                let stale_after = chrono::Duration::seconds(
                    stored
                        .source_config
                        .expected_sync_interval_seconds
                        .saturating_mul(2),
                );
                if now - last_success > stale_after {
                    Freshness::Stale
                } else {
                    Freshness::Fresh
                }
            }
        };
        let mut actions = vec![WorkAction::Snooze, WorkAction::Dismiss];
        if stored.entry.kind == WorkKind::Action {
            actions.extend([
                WorkAction::Plan,
                WorkAction::MoveToInbox,
                WorkAction::MoveToBacklog,
            ]);
        }
        actions.push(if stored.entry.pinned {
            WorkAction::Unpin
        } else {
            WorkAction::Pin
        });
        if stored.binding.progress_authority == ProgressAuthority::Local
            && stored.entry.lifecycle == WorkLifecycle::Active
        {
            if stored.entry.progress == Some(WorkProgress::Todo) {
                actions.push(WorkAction::StartWork);
            }
            actions.push(WorkAction::Complete);
        }
        let can_navigate = super::navigation::validated_target(&stored.navigation).is_ok();
        if can_navigate {
            actions.push(WorkAction::OpenSource);
        }

        let mut view = WorkView {
            id: stored.entry.id,
            kind: stored.entry.kind,
            title: stored.entry.title,
            summary: stored.entry.summary,
            priority: stored.entry.priority,
            lifecycle: stored.entry.lifecycle,
            progress: stored.entry.progress,
            planning: stored.entry.planning,
            disposition: if stored.entry.disposition == LocalDisposition::Snoozed
                && stored.entry.snoozed_until.is_some_and(|until| until <= now)
            {
                LocalDisposition::Normal
            } else {
                stored.entry.disposition
            },
            pinned: stored.entry.pinned,
            snoozed_until: stored.entry.snoozed_until,
            start: stored.entry.start,
            end: stored.entry.end,
            due: stored.entry.due,
            source: SourceView {
                provider_id: metadata.provider_id.0,
                provider_name: metadata.provider_name,
                source_name: metadata.source_name,
                config_name: stored.source_config.display_name,
            },
            can_navigate,
            freshness,
            dimensions: stored.entry.dimensions,
            facets: stored.entry.facets,
            available_actions: actions,
        };
        if privacy_mode {
            redact_work_view(&mut view);
        }
        Ok(view)
    }
}

fn redact_work_view(view: &mut WorkView) {
    view.title = if view.kind == WorkKind::Event {
        "Private event".into()
    } else {
        "Private work item".into()
    };
    view.summary = None;
    view.source = SourceView {
        provider_id: "private".into(),
        provider_name: "Private".into(),
        source_name: "Private".into(),
        config_name: "Private".into(),
    };
    view.dimensions = serde_json::json!({});
    view.facets = serde_json::json!({});
}

fn upcoming_entries(
    stored: &StoredWork,
    today: NaiveDate,
    days: u64,
    time_context: TimeContext,
) -> Vec<(NaiveDate, UpcomingBasis)> {
    if days == 0 || stored.entry.kind == WorkKind::Attention {
        return Vec::new();
    }
    let Some(first) = today.checked_add_days(Days::new(1)) else {
        return Vec::new();
    };
    let Some(last) = today.checked_add_days(Days::new(days)) else {
        return Vec::new();
    };
    let in_range = |date: NaiveDate| first <= date && date <= last;
    let mut entries = Vec::new();

    if stored.entry.kind == WorkKind::Event {
        if let Some(date) = (0..days)
            .filter_map(|offset| first.checked_add_days(Days::new(offset)))
            .find(|date| {
                event_overlaps_today(
                    stored.entry.start.as_ref(),
                    stored.entry.end.as_ref(),
                    *date,
                    time_context,
                )
            })
        {
            entries.push((date, UpcomingBasis::Event));
        }
    }

    if let Some(WorkPlanning::Planned(date)) = stored.entry.planning {
        if in_range(date) {
            entries.push((date, UpcomingBasis::Planned));
        }
    }
    if let Some(date) = stored
        .entry
        .due
        .as_ref()
        .and_then(|value| temporal_local_date(value, time_context))
    {
        if in_range(date) {
            entries.push((date, UpcomingBasis::Due));
        }
    }
    entries
}

fn temporal_local_date(value: &TemporalValue, time_context: TimeContext) -> Option<NaiveDate> {
    match value {
        TemporalValue::Date { date } => Some(*date),
        TemporalValue::DateTime { instant, .. } => Some(time_context.local_date(*instant)),
    }
}

fn upcoming_rank(basis: UpcomingBasis) -> u8 {
    match basis {
        UpcomingBasis::Event => 0,
        UpcomingBasis::Planned => 1,
        UpcomingBasis::Due => 2,
    }
}

pub struct WorkCommandService {
    store: Arc<dyn WorkStore>,
    clock: Arc<dyn Clock>,
}

impl WorkCommandService {
    pub fn new(store: Arc<dyn WorkStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    pub fn plan(&self, id: &str, date: NaiveDate) -> Result<()> {
        self.require_action(id)?;
        self.apply(id, WorkMutation::SetPlanning(WorkPlanning::Planned(date)))
    }

    pub fn move_to_inbox(&self, id: &str) -> Result<()> {
        self.require_action(id)?;
        self.apply(id, WorkMutation::SetPlanning(WorkPlanning::Inbox))
    }

    pub fn move_to_backlog(&self, id: &str) -> Result<()> {
        self.require_action(id)?;
        self.apply(id, WorkMutation::SetPlanning(WorkPlanning::Backlog))
    }

    pub fn snooze(&self, id: &str, until: DateTime<Utc>) -> Result<()> {
        if until <= self.clock.now() {
            return Err(GlanceletError::InvalidOperation(
                "snooze time must be in the future".into(),
            ));
        }
        self.apply(id, WorkMutation::Snooze(until))
    }

    pub fn dismiss(&self, id: &str) -> Result<()> {
        self.apply(id, WorkMutation::Dismiss)
    }

    pub fn pin(&self, id: &str) -> Result<()> {
        self.apply(id, WorkMutation::SetPinned(true))
    }

    pub fn unpin(&self, id: &str) -> Result<()> {
        self.apply(id, WorkMutation::SetPinned(false))
    }

    pub fn start_work(&self, id: &str) -> Result<()> {
        self.store.transition_local_progress(
            id,
            &[WorkProgress::Todo],
            WorkProgress::Doing,
            self.clock.now(),
        )
    }

    pub fn complete(&self, id: &str) -> Result<()> {
        self.store.transition_local_progress(
            id,
            &[WorkProgress::Todo, WorkProgress::Doing],
            WorkProgress::Done,
            self.clock.now(),
        )
    }

    fn require_action(&self, id: &str) -> Result<()> {
        if self.store.stored_work_by_id(id)?.entry.kind != WorkKind::Action {
            return Err(GlanceletError::InvalidOperation(
                "planning is only available for actions".into(),
            ));
        }
        Ok(())
    }

    fn apply(&self, id: &str, mutation: WorkMutation) -> Result<()> {
        self.store.mutate_work(id, mutation, self.clock.now())
    }
}

fn is_visible(stored: &StoredWork, now: DateTime<Utc>) -> bool {
    if stored.entry.lifecycle != WorkLifecycle::Active
        || stored.entry.disposition == LocalDisposition::Dismissed
    {
        return false;
    }
    stored.entry.disposition != LocalDisposition::Snoozed
        || stored.entry.snoozed_until.is_some_and(|until| until <= now)
}

fn belongs_to_today(stored: &StoredWork, today: NaiveDate, time_context: TimeContext) -> bool {
    if stored.entry.pinned || stored.entry.kind == WorkKind::Attention {
        return true;
    }
    match stored.entry.kind {
        WorkKind::Action => stored.entry.planning == Some(WorkPlanning::Planned(today)),
        WorkKind::Event => event_overlaps_today(
            stored.entry.start.as_ref(),
            stored.entry.end.as_ref(),
            today,
            time_context,
        ),
        WorkKind::Attention => true,
    }
}

/// Event ranges are provider-neutral half-open intervals: `[start, end)`.
fn event_overlaps_today(
    start: Option<&TemporalValue>,
    end: Option<&TemporalValue>,
    today: NaiveDate,
    time_context: TimeContext,
) -> bool {
    match (start, end) {
        (Some(TemporalValue::Date { date: start }), Some(TemporalValue::Date { date: end })) => {
            *start <= today && today < *end
        }
        (
            Some(TemporalValue::DateTime { instant: start, .. }),
            Some(TemporalValue::DateTime { instant: end, .. }),
        ) => {
            let timezone = time_context.timezone();
            let start = start.with_timezone(&timezone);
            let end = end.with_timezone(&timezone);
            start.date_naive() <= today
                && (end.date_naive() > today
                    || end.date_naive() == today && end.time() > NaiveTime::MIN)
        }
        (Some(TemporalValue::Date { date }), _) => *date == today,
        (Some(TemporalValue::DateTime { instant, .. }), _) => {
            time_context.local_date(*instant) == today
        }
        (None, _) => false,
    }
}

fn sort_views(items: &mut [WorkView]) {
    items.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| a.title.cmp(&b.title)));
}
