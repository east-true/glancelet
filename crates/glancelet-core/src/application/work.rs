use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
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
        let now = self.clock.now();
        let today = self.time_context.local_date(now);
        let mut today_items = Vec::new();
        let mut inbox_items = Vec::new();

        for stored in self.store.stored_work()? {
            if !is_visible(&stored, now) {
                continue;
            }
            let is_today = belongs_to_today(&stored, today, self.time_context);
            let is_inbox = stored.entry.kind == WorkKind::Action
                && stored.entry.planning == Some(WorkPlanning::Inbox);
            let view = self.to_view(stored, now)?;
            if is_today {
                today_items.push(view.clone());
            }
            if is_inbox {
                inbox_items.push(view);
            }
        }

        sort_views(&mut today_items);
        sort_views(&mut inbox_items);
        Ok(WorkDashboard {
            today: today_items,
            inbox: inbox_items,
        })
    }

    fn to_view(&self, stored: StoredWork, now: DateTime<Utc>) -> Result<WorkView> {
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

        Ok(WorkView {
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
        })
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
        self.require_local_progress(id)?;
        self.apply(id, WorkMutation::SetProgress(WorkProgress::Doing))
    }

    pub fn complete(&self, id: &str) -> Result<()> {
        self.require_local_progress(id)?;
        self.apply(id, WorkMutation::SetProgress(WorkProgress::Done))
    }

    fn require_action(&self, id: &str) -> Result<()> {
        if self.store.stored_work_by_id(id)?.entry.kind != WorkKind::Action {
            return Err(GlanceletError::InvalidOperation(
                "planning is only available for actions".into(),
            ));
        }
        Ok(())
    }

    fn require_local_progress(&self, id: &str) -> Result<()> {
        let work = self.store.stored_work_by_id(id)?;
        if work.binding.progress_authority != ProgressAuthority::Local {
            return Err(GlanceletError::InvalidOperation(
                "progress is not locally controlled".into(),
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
