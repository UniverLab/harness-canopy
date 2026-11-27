//! The `calendar` resource tool: personal calendar events and per-project milestones.

use crate::daemon::actions::{action_params, resource_action_object, ActionSpec, ToolSpec};
use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::calendar::{CalendarEventUpdate, CalendarMilestoneUpdate};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

pub(crate) static CALENDAR: ToolSpec = ToolSpec {
    name: "calendar",
    actions: &[
        ActionSpec {
            name: "list_events",
            required: &[],
            optional: &["from", "to", "include_dismissed"],
        },
        ActionSpec {
            name: "create_event",
            required: &["title", "at"],
            optional: &["notes", "recurrence", "lead_minutes"],
        },
        ActionSpec {
            name: "update_event",
            required: &["id"],
            optional: &[
                "title",
                "notes",
                "at",
                "recurrence",
                "lead_minutes",
                "snoozed_until",
                "dismissed_at",
            ],
        },
        ActionSpec {
            name: "delete_event",
            required: &["id"],
            optional: &[],
        },
        ActionSpec {
            name: "list_milestones",
            required: &[],
            optional: &["project_path", "status"],
        },
        ActionSpec {
            name: "create_milestone",
            required: &["project_path", "title"],
            optional: &["notes", "due_at", "status"],
        },
        ActionSpec {
            name: "update_milestone",
            required: &["id"],
            optional: &["title", "notes", "due_at", "status"],
        },
        ActionSpec {
            name: "delete_milestone",
            required: &["id"],
            optional: &[],
        },
    ],
};

resource_action_object!(
    CalendarObject,
    CALENDAR,
    [
        ("list_events", CalendarListEventsParams),
        ("create_event", CalendarCreateEventParams),
        ("update_event", CalendarUpdateEventParams),
        ("delete_event", CalendarIdParams),
        ("list_milestones", CalendarListMilestonesParams),
        ("create_milestone", CalendarCreateMilestoneParams),
        ("update_milestone", CalendarUpdateMilestoneParams),
        ("delete_milestone", CalendarIdParams),
    ]
);

#[tool_router(router = tool_router_calendar, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "calendar",
        description = "Manage personal calendar events (meetings/reminders with recurrence and lead-time alerts) and per-project roadmap milestones. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `list_events` (required: none; optional: from, to, include_dismissed) — list personal events ordered by `at`. \
        `create_event` (required: title, at; optional: notes, recurrence, lead_minutes) — create a personal event (`at` is RFC 3339, `recurrence` is none|daily|weekly|monthly). \
        `update_event` (required: id; optional: title, notes, at, recurrence, lead_minutes, snoozed_until, dismissed_at) — update an existing event. \
        `delete_event` (required: id; optional: none) — delete an event by id. \
        `list_milestones` (required: none; optional: project_path, status) — list per-project milestones (`status` is planned|active|done). \
        `create_milestone` (required: project_path, title; optional: notes, due_at, status) — create a project milestone. \
        `update_milestone` (required: id; optional: title, notes, due_at, status) — update an existing milestone. \
        `delete_milestone` (required: id; optional: none) — delete a milestone by id."
    )]
    pub(crate) async fn calendar(
        &self,
        Parameters(params): Parameters<CalendarObject>,
    ) -> Result<CallToolResult, McpError> {
        let mut params = params.into_value();
        CalendarObject::coerce(&mut params);
        let action = CALENDAR.validate(&params)?;
        let spec = CALENDAR.action(action).expect("validated action");
        match action {
            "list_events" => self.calendar_list_events(&action_params(&params, spec)?),
            "create_event" => self.calendar_create_event(&action_params(&params, spec)?),
            "update_event" => self.calendar_update_event(&action_params(&params, spec)?),
            "delete_event" => self.calendar_delete_event(&action_params(&params, spec)?),
            "list_milestones" => self.calendar_list_milestones(&action_params(&params, spec)?),
            "create_milestone" => self.calendar_create_milestone(&action_params(&params, spec)?),
            "update_milestone" => self.calendar_update_milestone(&action_params(&params, spec)?),
            "delete_milestone" => self.calendar_delete_milestone(&action_params(&params, spec)?),
            _ => unreachable!("validated action"),
        }
    }
}

impl TaskTriggerHandler {
    fn calendar_list_events(
        &self,
        params: &CalendarListEventsParams,
    ) -> Result<CallToolResult, McpError> {
        let events = match self.db.list_calendar_events(
            params.from.as_deref(),
            params.to.as_deref(),
            params.include_dismissed.unwrap_or(false),
        ) {
            Ok(events) => events,
            Err(e) => return Ok(error_result(&e.to_string())),
        };
        json_result(&serde_json::json!({ "events": events }))
    }

    fn calendar_create_event(
        &self,
        params: &CalendarCreateEventParams,
    ) -> Result<CallToolResult, McpError> {
        let event = match self.db.create_calendar_event(
            &params.title,
            &params.at,
            params.notes.as_deref(),
            params.recurrence.as_deref(),
            params.lead_minutes,
        ) {
            Ok(event) => event,
            Err(e) => return Ok(error_result(&e.to_string())),
        };
        json_result(&event)
    }

    fn calendar_update_event(
        &self,
        params: &CalendarUpdateEventParams,
    ) -> Result<CallToolResult, McpError> {
        let update = CalendarEventUpdate {
            title: params.title.as_deref(),
            notes: params.notes.as_deref(),
            at: params.at.as_deref(),
            recurrence: params.recurrence.as_deref(),
            lead_minutes: params.lead_minutes,
            snoozed_until: params.snoozed_until.as_deref(),
            dismissed_at: params.dismissed_at.as_deref(),
        };
        let event = match self.db.update_calendar_event(&params.id, &update) {
            Ok(event) => event,
            Err(e) => return Ok(error_result(&e.to_string())),
        };
        json_result(&event)
    }

    fn calendar_delete_event(&self, params: &CalendarIdParams) -> Result<CallToolResult, McpError> {
        match self.db.delete_calendar_event(&params.id) {
            Ok(true) => json_result(&serde_json::json!({ "deleted": true, "id": params.id })),
            Ok(false) => Ok(error_result(&format!(
                "Calendar event '{}' not found",
                params.id
            ))),
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }

    fn calendar_list_milestones(
        &self,
        params: &CalendarListMilestonesParams,
    ) -> Result<CallToolResult, McpError> {
        let milestones = match self
            .db
            .list_calendar_milestones(params.project_path.as_deref(), params.status.as_deref())
        {
            Ok(milestones) => milestones,
            Err(e) => return Ok(error_result(&e.to_string())),
        };
        json_result(&serde_json::json!({ "milestones": milestones }))
    }

    fn calendar_create_milestone(
        &self,
        params: &CalendarCreateMilestoneParams,
    ) -> Result<CallToolResult, McpError> {
        let milestone = match self.db.create_calendar_milestone(
            &params.project_path,
            &params.title,
            params.notes.as_deref(),
            params.due_at.as_deref(),
            params.status.as_deref(),
        ) {
            Ok(milestone) => milestone,
            Err(e) => return Ok(error_result(&e.to_string())),
        };
        json_result(&milestone)
    }

    fn calendar_update_milestone(
        &self,
        params: &CalendarUpdateMilestoneParams,
    ) -> Result<CallToolResult, McpError> {
        let update = CalendarMilestoneUpdate {
            title: params.title.as_deref(),
            notes: params.notes.as_deref(),
            due_at: params.due_at.as_deref(),
            status: params.status.as_deref(),
        };
        let milestone = match self.db.update_calendar_milestone(&params.id, &update) {
            Ok(milestone) => milestone,
            Err(e) => return Ok(error_result(&e.to_string())),
        };
        json_result(&milestone)
    }

    fn calendar_delete_milestone(
        &self,
        params: &CalendarIdParams,
    ) -> Result<CallToolResult, McpError> {
        match self.db.delete_calendar_milestone(&params.id) {
            Ok(true) => json_result(&serde_json::json!({ "deleted": true, "id": params.id })),
            Ok(false) => Ok(error_result(&format!(
                "Calendar milestone '{}' not found",
                params.id
            ))),
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }
}

fn json_result<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|e| McpError::internal_error(format!("serializing result: {e}"), None))?;
    Ok(success_result(&body))
}
