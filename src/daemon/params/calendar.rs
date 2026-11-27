use super::*;

/// `calendar` `list_events` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CalendarListEventsParams {
    /// Optional lower bound (RFC 3339 timestamp).
    pub from: Option<String>,
    /// Optional upper bound (RFC 3339 timestamp).
    pub to: Option<String>,
    /// Include dismissed events (default false).
    pub include_dismissed: Option<bool>,
}

/// `calendar` `create_event` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CalendarCreateEventParams {
    /// Event title.
    pub title: String,
    /// Event timestamp (RFC 3339).
    pub at: String,
    /// Optional notes.
    pub notes: Option<String>,
    /// Recurrence cadence: none, daily, weekly, or monthly (default none).
    pub recurrence: Option<String>,
    /// Lead-time alert in minutes before `at` (default 10, must be >= 0).
    pub lead_minutes: Option<i64>,
}

/// `calendar` `update_event` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CalendarUpdateEventParams {
    /// Event id.
    pub id: String,
    /// New title.
    pub title: Option<String>,
    /// New notes.
    pub notes: Option<String>,
    /// New timestamp (RFC 3339).
    pub at: Option<String>,
    /// New recurrence cadence: none, daily, weekly, or monthly.
    pub recurrence: Option<String>,
    /// New lead-time alert in minutes (>= 0).
    pub lead_minutes: Option<i64>,
    /// Snooze alert until timestamp (RFC 3339).
    pub snoozed_until: Option<String>,
    /// Mark dismissed at timestamp (RFC 3339).
    pub dismissed_at: Option<String>,
}

/// `calendar` `delete_event` / `delete_milestone` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CalendarIdParams {
    /// Target record id.
    pub id: String,
}

/// `calendar` `list_milestones` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CalendarListMilestonesParams {
    /// Optional project path filter.
    pub project_path: Option<String>,
    /// Optional status filter: planned, active, or done.
    pub status: Option<String>,
}

/// `calendar` `create_milestone` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CalendarCreateMilestoneParams {
    /// Project directory path.
    pub project_path: String,
    /// Milestone title.
    pub title: String,
    /// Optional notes.
    pub notes: Option<String>,
    /// Optional due timestamp (RFC 3339).
    pub due_at: Option<String>,
    /// Initial status: planned, active, or done (default planned).
    pub status: Option<String>,
}

/// `calendar` `update_milestone` parameters.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CalendarUpdateMilestoneParams {
    /// Milestone id.
    pub id: String,
    /// New title.
    pub title: Option<String>,
    /// New notes.
    pub notes: Option<String>,
    /// New due timestamp (RFC 3339).
    pub due_at: Option<String>,
    /// New status: planned, active, or done.
    pub status: Option<String>,
}
