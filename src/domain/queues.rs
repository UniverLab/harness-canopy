use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::graphs::GraphSpec;

/// An ordered queue of existing specs — the work, decoupled from any one
/// graph (the team that will eventually run it). A spec can be a member of
/// any number of queues; queue membership never mutates the spec itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// A queue together with its members, resolved and ordered by position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueDetails {
    pub queue: Queue,
    pub members: Vec<GraphSpec>,
    /// RS3 context group label per member spec id. Absent/`None` for ungrouped
    /// members. Kept as a side map so `members` stays a plain `Vec<GraphSpec>`
    /// (queue membership never mutates the spec itself — the group lives on the
    /// membership row).
    pub member_groups: std::collections::HashMap<String, Option<String>>,
}
