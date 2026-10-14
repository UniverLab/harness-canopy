//! CM49: crews — named, ordered member lists kept in one place.
//!
//! A crew is a named list of `(platform, model)` members that ensembles,
//! agent nodes and agent hooks can bind to instead of carrying literal
//! pairs. Editing a crew reaches every bound ensemble at its next dispatch
//! (see `Database::sync_ensemble_with_crew`). Pure validation and types —
//! no IO; all SQL lives in `crate::db::crews`.
//!
//! Member order is meaningful: `cascade` tries members in position order,
//! `round_robin` rotates through them in position order, and a bound agent
//! node or hook takes the first member in position order that the roster
//! (CM48 `effective_state`) reports available.

/// A crew holds at least one member. (Ensembles need two — that bound is
/// enforced at bind time, because a crew is kind-agnostic.)
pub const CREW_MIN_MEMBERS: usize = 1;

/// The upper bound is the largest ensemble cap: `max(member_bounds(Parallel).1,
/// member_bounds(Cascade).1)` = 16. A crew is kind-agnostic so it takes the
/// max; kind-specific bounds are enforced at bind time. If `member_bounds`
/// ever changes, change this constant with it.
pub const CREW_MAX_MEMBERS: usize = 16;

/// One crew: the stored header row. Members live in `crew_members`, keyed by
/// `(crew_id, position)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crew {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// Bumped by one in the same transaction as every write to the member
    /// list. Bound ensembles store the version they last applied
    /// (`ensembles.crew_version_applied`) so the lazy sync can tell there is
    /// work to do.
    pub version: i64,
}

/// One member of a crew. Deliberately has NO `prompt_override`: a bound
/// ensemble renders its shared `prompt_template` for every member, so
/// per-member review angles stay literal ensembles (spec req 4).
///
/// `position` is the meaningful order (0-based): the order `cascade` tries,
/// `round_robin` rotates through, and a bound agent node walks for the first
/// available member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewMember {
    pub position: i64,
    pub platform: String,
    pub model: Option<String>,
    pub timeout_minutes: Option<i64>,
    pub infra_retry_limit: Option<i64>,
    pub infra_crash_max_seconds: Option<i64>,
    pub infra_backoff_seconds: Option<i64>,
}

/// Validate a crew name: trimmed, non-empty, at most 64 characters.
pub fn validate_crew_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("crew name must not be empty".to_string());
    }
    if trimmed.len() > 64 {
        return Err(format!(
            "crew name must be at most 64 characters, got {}",
            trimmed.len()
        ));
    }
    Ok(())
}

/// Validate a member list: 1..=[`CREW_MAX_MEMBERS`] entries, each platform
/// non-empty after trim, each optional numeric non-negative.
pub fn validate_crew_members(members: &[CrewMember]) -> Result<(), String> {
    if members.len() < CREW_MIN_MEMBERS {
        return Err(format!(
            "a crew needs at least {CREW_MIN_MEMBERS} member(s)"
        ));
    }
    if members.len() > CREW_MAX_MEMBERS {
        return Err(format!(
            "a crew holds at most {CREW_MAX_MEMBERS} members, got {}",
            members.len()
        ));
    }
    for (index, member) in members.iter().enumerate() {
        if member.platform.trim().is_empty() {
            return Err(format!("member {index}: platform must not be empty"));
        }
        for (label, value) in [
            ("timeout_minutes", member.timeout_minutes),
            ("infra_retry_limit", member.infra_retry_limit),
            ("infra_crash_max_seconds", member.infra_crash_max_seconds),
            ("infra_backoff_seconds", member.infra_backoff_seconds),
        ] {
            if value.is_some_and(|v| v < 0) {
                return Err(format!(
                    "member {index}: {label} must be a non-negative integer"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(platform: &str) -> CrewMember {
        CrewMember {
            position: 0,
            platform: platform.to_string(),
            model: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }
    }

    #[test]
    fn name_validation() {
        assert!(validate_crew_name("implementer").is_ok());
        assert!(validate_crew_name("  ").is_err());
        assert!(validate_crew_name(&"x".repeat(65)).is_err());
        assert!(validate_crew_name(&"x".repeat(64)).is_ok());
    }

    #[test]
    fn member_validation_bounds_and_values() {
        assert!(validate_crew_members(&[member("opencode")]).is_ok());
        assert!(validate_crew_members(&[]).is_err());
        let many: Vec<CrewMember> = (0..CREW_MAX_MEMBERS + 1)
            .map(|i| CrewMember {
                position: i as i64,
                ..member("opencode")
            })
            .collect();
        assert!(validate_crew_members(&many).is_err());
        let blank = CrewMember {
            platform: "   ".to_string(),
            ..member("opencode")
        };
        assert!(validate_crew_members(&[blank]).is_err());
        let negative = CrewMember {
            timeout_minutes: Some(-1),
            ..member("opencode")
        };
        assert!(validate_crew_members(&[negative]).is_err());
    }
}
