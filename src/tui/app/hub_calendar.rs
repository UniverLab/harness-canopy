//! State, timeline merge, and quick actions for the Hub (`F1`) Calendar tab.

use anyhow::Result;
use chrono::{DateTime, Duration, Local, SecondsFormat, Utc};
use ratatui::layout::Rect;

use super::types::App;
use crate::db::calendar::CalendarMilestoneUpdate;
use crate::db::Database;
use crate::domain::calendar::{
    next_occurrence, parse_rfc3339_utc, CalendarMilestone, MilestoneStatus, Recurrence,
};
use crate::scheduler::cron_scheduler::next_cron_occurrence_utc;

const TIMELINE_HORIZON_DAYS: i64 = 14;
const SNOOZE_MINUTES: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CalendarSubView {
    #[default]
    Timeline,
    Milestones,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CalendarItemKind {
    Event { id: String },
    FeedItem { id: String, feed_name: String },
    CronGraph { graph_id: String },
    GraphAt { run_id: String, graph_id: String },
    MissedRun { run_id: String, graph_id: String },
    ScheduledPrompt { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimelineItem {
    pub at: DateTime<Utc>,
    pub tag: String,
    pub title: String,
    pub detail: Option<String>,
    pub pinned: bool,
    pub kind: CalendarItemKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AddFormMode {
    #[default]
    Event,
    Milestone,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CalendarAddForm {
    pub open: bool,
    pub mode: AddFormMode,
    pub field: usize,
    pub title: String,
    pub at: String,
    pub notes: String,
    pub project_path: String,
}

impl CalendarAddForm {
    pub(crate) fn field_count(&self) -> usize {
        match self.mode {
            AddFormMode::Event => 3,
            AddFormMode::Milestone => 4,
        }
    }

    pub(crate) fn active_buffer_mut(&mut self) -> &mut String {
        match (self.mode, self.field) {
            (AddFormMode::Event, 0) | (AddFormMode::Milestone, 0) => &mut self.title,
            (AddFormMode::Event, 1) | (AddFormMode::Milestone, 1) => &mut self.at,
            (AddFormMode::Event, _) => &mut self.notes,
            (AddFormMode::Milestone, 2) => &mut self.project_path,
            (AddFormMode::Milestone, _) => &mut self.notes,
        }
    }
}

#[derive(Default)]
pub(crate) struct HubCalendarState {
    pub sub_view: CalendarSubView,
    pub timeline: Vec<TimelineItem>,
    pub milestones: Vec<CalendarMilestone>,
    pub timeline_selected: usize,
    pub milestone_selected: usize,
    pub status_message: Option<String>,
    pub add_form: CalendarAddForm,
    pub timeline_rect: Option<Rect>,
    pub milestones_rect: Option<Rect>,
    pub list_rect: Option<Rect>,
    /// Maps visible row offsets in `list_rect` (Milestones sub-view) to
    /// indices in `milestones` (`None` for project group headers).
    pub milestone_row_indices: Vec<Option<usize>>,
}

/// Merge all 5 read-only calendar sources over the next 14 days, pinning
/// `missed` one-shot `at` graph runs at the top.
pub(crate) fn build_timeline(db: &Database, now: DateTime<Utc>) -> Result<Vec<TimelineItem>> {
    let horizon = now + Duration::days(TIMELINE_HORIZON_DAYS);
    let mut items = Vec::new();

    collect_calendar_events(db, now, horizon, &mut items)?;
    collect_feed_items(db, now, horizon, &mut items)?;
    collect_cron_graphs(db, now, horizon, &mut items)?;
    collect_scheduled_runs(db, horizon, &mut items)?;
    collect_scheduled_prompts(db, horizon, &mut items)?;

    items.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| a.at.cmp(&b.at)));
    Ok(items)
}

fn next_in_window(
    at_raw: &str,
    recurrence: Recurrence,
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let at = parse_rfc3339_utc(at_raw).ok()?;
    let occ = match recurrence {
        Recurrence::None => (at >= now && at <= horizon).then_some(at),
        _ => next_occurrence(at, recurrence, now).filter(|dt| *dt <= horizon),
    }?;
    Some(occ)
}

fn collect_calendar_events(
    db: &Database,
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
    out: &mut Vec<TimelineItem>,
) -> Result<()> {
    for ev in db.list_calendar_events(None, None, false)? {
        if let Some(at) = next_in_window(&ev.at, ev.recurrence, now, horizon) {
            out.push(TimelineItem {
                at,
                tag: "[event]".to_string(),
                title: ev.title,
                detail: ev.notes,
                pinned: false,
                kind: CalendarItemKind::Event { id: ev.id },
            });
        }
    }
    Ok(())
}

fn collect_feed_items(
    db: &Database,
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
    out: &mut Vec<TimelineItem>,
) -> Result<()> {
    for item in db.list_calendar_feed_items(None, false)? {
        if let Some(at) = next_in_window(&item.at, item.recurrence, now, horizon) {
            let tag = format!("[ics:{}]", item.feed_name);
            out.push(TimelineItem {
                at,
                tag,
                title: item.title,
                detail: item.notes,
                pinned: false,
                kind: CalendarItemKind::FeedItem {
                    id: item.id,
                    feed_name: item.feed_name,
                },
            });
        }
    }
    Ok(())
}

fn collect_cron_graphs(
    db: &Database,
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
    out: &mut Vec<TimelineItem>,
) -> Result<()> {
    let now_local = now.with_timezone(&Local);
    for graph in db.list_cron_graphs()? {
        if !graph.enabled || graph.is_expired() || !graph.is_fireable() {
            continue;
        }
        let Some(expr) = graph.schedule_expr().map(str::to_string) else {
            continue;
        };
        let Some(next) = next_cron_occurrence_utc(&expr, now_local) else {
            continue;
        };
        if next <= horizon {
            out.push(TimelineItem {
                at: next,
                tag: "[cron]".to_string(),
                title: graph.name,
                detail: Some(expr),
                pinned: false,
                kind: CalendarItemKind::CronGraph { graph_id: graph.id },
            });
        }
    }
    Ok(())
}

fn collect_scheduled_runs(
    db: &Database,
    horizon: DateTime<Utc>,
    out: &mut Vec<TimelineItem>,
) -> Result<()> {
    for run in db.list_graph_scheduled_runs(None, Some("missed"))? {
        let Ok(at) = parse_rfc3339_utc(&run.fire_at) else {
            continue;
        };
        let title = run
            .idea
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("Graph {}", run.graph_id));
        out.push(TimelineItem {
            at,
            tag: "[missed]".to_string(),
            title,
            detail: run.workdir,
            pinned: true,
            kind: CalendarItemKind::MissedRun {
                run_id: run.id,
                graph_id: run.graph_id,
            },
        });
    }

    for run in db.list_graph_scheduled_runs(None, Some("pending"))? {
        let Ok(at) = parse_rfc3339_utc(&run.fire_at) else {
            continue;
        };
        if at <= horizon {
            let title = run
                .idea
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("Graph {}", run.graph_id));
            out.push(TimelineItem {
                at,
                tag: "[graph-at]".to_string(),
                title,
                detail: run.workdir,
                pinned: false,
                kind: CalendarItemKind::GraphAt {
                    run_id: run.id,
                    graph_id: run.graph_id,
                },
            });
        }
    }
    Ok(())
}

fn collect_scheduled_prompts(
    db: &Database,
    horizon: DateTime<Utc>,
    out: &mut Vec<TimelineItem>,
) -> Result<()> {
    for prompt in db.list_all_pending_scheduled_prompts()? {
        if prompt.fire_at <= horizon {
            let first_line = prompt
                .prompt
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let title = if first_line.is_empty() {
                "Scheduled prompt".to_string()
            } else {
                first_line
            };
            out.push(TimelineItem {
                at: prompt.fire_at,
                tag: "[prompt]".to_string(),
                title,
                detail: Some(prompt.target_session_id),
                pinned: false,
                kind: CalendarItemKind::ScheduledPrompt { id: prompt.id },
            });
        }
    }
    Ok(())
}

impl App {
    pub(crate) fn refresh_calendar(&mut self) {
        match build_timeline(&self.db, Utc::now()) {
            Ok(timeline) => {
                self.hub.calendar.timeline = timeline;
                let len = self.hub.calendar.timeline.len();
                if len == 0 {
                    self.hub.calendar.timeline_selected = 0;
                } else {
                    self.hub.calendar.timeline_selected =
                        self.hub.calendar.timeline_selected.min(len - 1);
                }
            }
            Err(err) => {
                self.hub.calendar.status_message = Some(format!("Timeline error: {err}"));
            }
        }
        match self.db.list_calendar_milestones(None, None) {
            Ok(mut milestones) => {
                // Group milestones by project_path while preserving DB order within each project.
                milestones.sort_by(|a, b| a.project_path.cmp(&b.project_path));
                self.hub.calendar.milestones = milestones;
                let len = self.hub.calendar.milestones.len();
                if len == 0 {
                    self.hub.calendar.milestone_selected = 0;
                } else {
                    self.hub.calendar.milestone_selected =
                        self.hub.calendar.milestone_selected.min(len - 1);
                }
            }
            Err(err) => {
                self.hub.calendar.status_message = Some(format!("Milestones error: {err}"));
            }
        }
    }

    pub(crate) fn toggle_calendar_sub_view(&mut self) {
        self.hub.calendar.sub_view = match self.hub.calendar.sub_view {
            CalendarSubView::Timeline => CalendarSubView::Milestones,
            CalendarSubView::Milestones => CalendarSubView::Timeline,
        };
        self.hub.calendar.status_message = None;
    }

    pub(crate) fn set_calendar_sub_view(&mut self, view: CalendarSubView) {
        self.hub.calendar.sub_view = view;
        self.hub.calendar.status_message = None;
    }

    pub(crate) fn move_calendar_selection(&mut self, forward: bool) {
        match self.hub.calendar.sub_view {
            CalendarSubView::Timeline => {
                let len = self.hub.calendar.timeline.len();
                if len > 0 {
                    self.hub.calendar.timeline_selected = crate::tui::selection::move_index(
                        self.hub.calendar.timeline_selected,
                        len,
                        forward,
                    );
                }
            }
            CalendarSubView::Milestones => {
                let len = self.hub.calendar.milestones.len();
                if len > 0 {
                    self.hub.calendar.milestone_selected = crate::tui::selection::move_index(
                        self.hub.calendar.milestone_selected,
                        len,
                        forward,
                    );
                }
            }
        }
    }

    pub(crate) fn calendar_dismiss_selected(&mut self) {
        let Some(item) = self
            .hub
            .calendar
            .timeline
            .get(self.hub.calendar.timeline_selected)
            .cloned()
        else {
            return;
        };
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let res = match item.kind {
            CalendarItemKind::Event { id } => self.db.dismiss_calendar_event(&id, &now).map(|_| ()),
            CalendarItemKind::FeedItem { id, .. } => {
                self.db.dismiss_calendar_feed_item(&id, &now).map(|_| ())
            }
            _ => {
                self.hub.calendar.status_message =
                    Some("Only events and feed items can be dismissed".to_string());
                return;
            }
        };
        match res {
            Ok(()) => {
                self.hub.calendar.status_message = Some(format!("Dismissed '{}'", item.title));
                self.refresh_calendar();
            }
            Err(err) => {
                self.hub.calendar.status_message = Some(format!("Dismiss failed: {err}"));
            }
        }
    }

    pub(crate) fn calendar_snooze_selected(&mut self) {
        let Some(item) = self
            .hub
            .calendar
            .timeline
            .get(self.hub.calendar.timeline_selected)
            .cloned()
        else {
            return;
        };
        let until = (Utc::now() + Duration::minutes(SNOOZE_MINUTES))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let res = match item.kind {
            CalendarItemKind::Event { id } => {
                self.db.snooze_calendar_event(&id, &until).map(|_| ())
            }
            CalendarItemKind::FeedItem { id, .. } => {
                self.db.snooze_calendar_feed_item(&id, &until).map(|_| ())
            }
            _ => {
                self.hub.calendar.status_message =
                    Some("Only events and feed items can be snoozed".to_string());
                return;
            }
        };
        match res {
            Ok(()) => {
                self.hub.calendar.status_message =
                    Some(format!("Snoozed '{}' for {SNOOZE_MINUTES}m", item.title));
                self.refresh_calendar();
            }
            Err(err) => {
                self.hub.calendar.status_message = Some(format!("Snooze failed: {err}"));
            }
        }
    }

    pub(crate) fn calendar_run_selected(&mut self) {
        let Some(item) = self
            .hub
            .calendar
            .timeline
            .get(self.hub.calendar.timeline_selected)
            .cloned()
        else {
            return;
        };
        let CalendarItemKind::MissedRun { run_id, .. } = item.kind else {
            self.hub.calendar.status_message =
                Some("Select a [missed] run to launch immediately".to_string());
            return;
        };
        let port = self.daemon_port.to_string();
        let args = serde_json::json!({
            "action": "run_scheduled_run",
            "id": run_id,
        });
        match crate::tui::mcp_client::call_daemon_tool(&port, "graph_control", &args) {
            Ok(outcome) if outcome.is_error => {
                self.hub.calendar.status_message = Some(outcome.text);
            }
            Ok(_) => {
                self.hub.calendar.status_message = Some(format!("Launched '{}'", item.title));
                self.refresh_calendar();
            }
            Err(err) => {
                self.hub.calendar.status_message = Some(format!("Launch failed: {err}"));
            }
        }
    }

    pub(crate) fn calendar_cycle_milestone_status(&mut self) {
        let Some(ms) = self
            .hub
            .calendar
            .milestones
            .get(self.hub.calendar.milestone_selected)
            .cloned()
        else {
            return;
        };
        let next = match ms.status {
            MilestoneStatus::Planned => MilestoneStatus::Active,
            MilestoneStatus::Active => MilestoneStatus::Done,
            MilestoneStatus::Done => MilestoneStatus::Planned,
        };
        let update = CalendarMilestoneUpdate {
            status: Some(next.as_str()),
            ..Default::default()
        };
        match self.db.update_calendar_milestone(&ms.id, &update) {
            Ok(_) => {
                self.hub.calendar.status_message =
                    Some(format!("Milestone '{}' -> {}", ms.title, next));
                self.refresh_calendar();
            }
            Err(err) => {
                self.hub.calendar.status_message = Some(format!("Update failed: {err}"));
            }
        }
    }

    pub(crate) fn calendar_open_add_form(&mut self) {
        let mode = match self.hub.calendar.sub_view {
            CalendarSubView::Timeline => AddFormMode::Event,
            CalendarSubView::Milestones => AddFormMode::Milestone,
        };
        let default_at =
            (Utc::now() + Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let project_path = self.current_workdir().to_string_lossy().to_string();
        self.hub.calendar.add_form = CalendarAddForm {
            open: true,
            mode,
            field: 0,
            title: String::new(),
            at: if mode == AddFormMode::Event {
                default_at
            } else {
                String::new()
            },
            notes: String::new(),
            project_path,
        };
        self.hub.calendar.status_message = None;
    }

    pub(crate) fn calendar_cancel_add_form(&mut self) {
        self.hub.calendar.add_form.open = false;
    }

    pub(crate) fn calendar_submit_add_form(&mut self) {
        let form = self.hub.calendar.add_form.clone();
        let notes = (!form.notes.trim().is_empty()).then_some(form.notes.trim());
        let res = match form.mode {
            AddFormMode::Event => self
                .db
                .create_calendar_event(form.title.trim(), form.at.trim(), notes, None, None)
                .map(|ev| format!("Added event '{}'", ev.title)),
            AddFormMode::Milestone => {
                let due = (!form.at.trim().is_empty()).then_some(form.at.trim());
                self.db
                    .create_calendar_milestone(
                        form.project_path.trim(),
                        form.title.trim(),
                        notes,
                        due,
                        None,
                    )
                    .map(|ms| format!("Added milestone '{}'", ms.title))
            }
        };
        match res {
            Ok(msg) => {
                self.hub.calendar.add_form.open = false;
                self.hub.calendar.status_message = Some(msg);
                self.refresh_calendar();
            }
            Err(err) => {
                self.hub.calendar.status_message = Some(format!("Error: {err}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::graph_scheduled_runs::GraphScheduledRun;
    use crate::domain::ics::parse_ics;
    use tempfile::TempDir;

    fn temp_db() -> (TempDir, Database) {
        let dir = TempDir::new().unwrap();
        let db = Database::new(&dir.path().join("canopy.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn build_timeline_merges_all_five_sources_and_pins_missed_at_top() {
        use crate::domain::graphs::{Graph, GraphStatus};
        use crate::domain::models::Trigger;

        let (dir, db) = temp_db();
        let now = parse_rfc3339_utc("2026-11-27T10:00:00Z").unwrap();

        // 1. Personal calendar event
        db.create_calendar_event("Team Sync", "2026-11-27T14:00:00Z", None, None, None)
            .unwrap();

        // 2. ICS feed item
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:ics-1\r\nSUMMARY:Dentist\r\nDTSTART:20261127T120000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        db.upsert_calendar_feed_items("Personal", &parse_ics(ics))
            .unwrap();

        // 3. Cron graph
        let graph = Graph {
            id: "g-cron-1".to_string(),
            name: "nightly-cron".to_string(),
            description: None,
            workdir: dir.path().to_string_lossy().to_string(),
            status: GraphStatus::Draft,
            trigger: Some(Trigger::Cron {
                schedule_expr: "0 3 * * *".to_string(),
            }),
            trigger_prompt: None,
            created_at: now,
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            require_spec_type: false,
            infra_node_id: None,
            enabled: true,
            enable_at: None,
            expires_at: None,
        };
        db.insert_graph(&graph).unwrap();

        // 4. Missed & pending one-shot graph runs
        db.insert_graph_scheduled_run(&GraphScheduledRun {
            id: "run-missed".to_string(),
            graph_id: graph.id.clone(),
            fire_at: "2026-11-27T09:00:00Z".to_string(),
            idea: Some("Missed job".to_string()),
            queue_id: None,
            workdir: None,
            on_miss: "skip".to_string(),
            status: "missed".to_string(),
            created_at: "2026-11-27T08:00:00Z".to_string(),
            fired_at: None,
            error: None,
        })
        .unwrap();
        db.insert_graph_scheduled_run(&GraphScheduledRun {
            id: "run-pending".to_string(),
            graph_id: graph.id,
            fire_at: "2026-11-27T13:00:00Z".to_string(),
            idea: Some("Later job".to_string()),
            queue_id: None,
            workdir: None,
            on_miss: "skip".to_string(),
            status: "pending".to_string(),
            created_at: "2026-11-27T08:00:00Z".to_string(),
            fired_at: None,
            error: None,
        })
        .unwrap();

        // 5. Scheduled prompt
        db.insert_scheduled_prompt(
            "sp-1",
            "Check build status",
            "sess-1",
            None,
            parse_rfc3339_utc("2026-11-27T11:00:00Z").unwrap(),
            None,
            None,
        )
        .unwrap();

        let timeline = build_timeline(&db, now).unwrap();
        let tags: Vec<&str> = timeline.iter().map(|i| i.tag.as_str()).collect();
        assert_eq!(tags[0], "[missed]");
        assert!(tags.contains(&"[prompt]"));
        assert!(tags.contains(&"[ics:Personal]"));
        assert!(tags.contains(&"[graph-at]"));
        assert!(tags.contains(&"[event]"));
        assert!(tags.contains(&"[cron]"));

        // Non-pinned items are strictly chronological.
        let non_pinned: Vec<DateTime<Utc>> = timeline
            .iter()
            .filter(|i| !i.pinned)
            .map(|i| i.at)
            .collect();
        assert!(non_pinned.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn refresh_calendar_groups_milestones_by_project_path() {
        let (dir, db) = temp_db();
        db.create_calendar_milestone(
            "/tmp/proj-b",
            "B1",
            None,
            Some("2026-12-01T00:00:00Z"),
            None,
        )
        .unwrap();
        db.create_calendar_milestone(
            "/tmp/proj-a",
            "A1",
            None,
            Some("2026-12-02T00:00:00Z"),
            None,
        )
        .unwrap();
        db.create_calendar_milestone(
            "/tmp/proj-b",
            "B2",
            None,
            Some("2026-12-03T00:00:00Z"),
            None,
        )
        .unwrap();

        let mut app = App::new(
            std::sync::Arc::new(db),
            dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.refresh_calendar();
        let grouped: Vec<(&str, &str)> = app
            .hub
            .calendar
            .milestones
            .iter()
            .map(|m| (m.project_path.as_str(), m.title.as_str()))
            .collect();
        assert_eq!(
            grouped,
            vec![
                ("/tmp/proj-a", "A1"),
                ("/tmp/proj-b", "B1"),
                ("/tmp/proj-b", "B2"),
            ]
        );
    }
}
