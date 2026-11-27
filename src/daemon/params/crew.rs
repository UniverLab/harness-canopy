use super::*;

// ── CM49 crew tool parameter types ─────────────────────────────────

/// One member in a `crew create`/`crew update` request. `position` is the
/// array index. Exactly one of `platform` (literal pair) or `model_ref`
/// (track a named model entry) must be given.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct CrewMemberJson {
    /// Platform (harness) for this member (e.g. `opencode-go`). Pass a
    /// `model_ref` instead to track a named model entry.
    pub platform: Option<String>,
    /// Model id, or omitted for the platform default. Never combined with
    /// `model_ref` — the entry owns the pair.
    pub model: Option<String>,
    /// Named model entry this member tracks. Exactly one of `platform` or
    /// `model_ref` must be given.
    pub model_ref: Option<String>,
    pub timeout_minutes: Option<i64>,
    pub infra_retry_limit: Option<i64>,
    pub infra_crash_max_seconds: Option<i64>,
    pub infra_backoff_seconds: Option<i64>,
}

impl CrewMemberJson {
    pub fn into_unresolved(self) -> crate::domain::crews::UnresolvedCrewMember {
        crate::domain::crews::UnresolvedCrewMember {
            platform: self.platform,
            model: self.model,
            model_ref: self.model_ref,
            timeout_minutes: self.timeout_minutes,
            infra_retry_limit: self.infra_retry_limit,
            infra_crash_max_seconds: self.infra_crash_max_seconds,
            infra_backoff_seconds: self.infra_backoff_seconds,
        }
    }
}

pub fn unresolved_from_json(
    members: Vec<CrewMemberJson>,
) -> Vec<crate::domain::crews::UnresolvedCrewMember> {
    members
        .into_iter()
        .map(|member| member.into_unresolved())
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
    /// first available member. Each member passes either `platform` or
    /// `model_ref`, never both.
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
    /// The crew name.
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct CrewModelSetParams {
    /// The entry name (trimmed, 1 to 64 chars of `[A-Za-z0-9._-]`).
    pub name: String,
    /// Platform (harness) for the entry (e.g. `antigravity`).
    pub platform: String,
    /// Model id, or omitted for the platform default.
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct CrewModelDeleteParams {
    /// The entry name.
    pub name: String,
}
