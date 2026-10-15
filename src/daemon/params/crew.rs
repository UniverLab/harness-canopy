use super::*;

// ── CM49 crew tool parameter types ─────────────────────────────────

/// One member in a `crew_create`/`crew_update` request. `position` is the
/// array index.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct CrewMemberJson {
    /// Platform (harness) for this member (e.g. `opencode-go`).
    pub platform: String,
    /// Model id, or omitted for the platform default.
    pub model: Option<String>,
    pub timeout_minutes: Option<i64>,
    pub infra_retry_limit: Option<i64>,
    pub infra_crash_max_seconds: Option<i64>,
    pub infra_backoff_seconds: Option<i64>,
}

impl CrewMemberJson {
    pub fn into_member(self, position: i64) -> crate::domain::crews::CrewMember {
        crate::domain::crews::CrewMember {
            position,
            platform: self.platform,
            model: self.model,
            timeout_minutes: self.timeout_minutes,
            infra_retry_limit: self.infra_retry_limit,
            infra_crash_max_seconds: self.infra_crash_max_seconds,
            infra_backoff_seconds: self.infra_backoff_seconds,
        }
    }
}

pub fn members_from_json(members: Vec<CrewMemberJson>) -> Vec<crate::domain::crews::CrewMember> {
    members
        .into_iter()
        .enumerate()
        .map(|(index, member)| member.into_member(index as i64))
        .collect()
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct CrewGetParams {
    /// The crew name.
    pub name: String,
}

/// `members` (a literal ordered list) or `from_ensemble` (seed from an
/// existing ensemble's current members) — exactly one.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct CrewCreateParams {
    pub name: String,
    pub description: Option<String>,
    /// Ordered member list. Order is meaningful: `cascade` tries it in
    /// order, `round_robin` rotates through it, a bound agent node takes the
    /// first available member.
    pub members: Option<Vec<CrewMemberJson>>,
    /// Seed the members from this ensemble's current members.
    pub from_ensemble: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct CrewUpdateParams {
    pub name: String,
    /// Replaces the whole ordered member list when given.
    pub members: Option<Vec<CrewMemberJson>>,
    pub description: Option<String>,
    /// New name for the crew.
    pub rename: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct CrewDeleteParams {
    pub name: String,
}
