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
    /// Named model entry this member tracks (`crew_models.name`), or `None`
    /// for a literal member. A member with a `model_ref` always stores the
    /// entry's current `platform`/`model` in its own columns, so the engine
    /// and every reader keep working unchanged.
    pub model_ref: Option<String>,
    pub timeout_minutes: Option<i64>,
    pub infra_retry_limit: Option<i64>,
    pub infra_crash_max_seconds: Option<i64>,
    pub infra_backoff_seconds: Option<i64>,
}

/// A member as submitted: exactly one of `platform` (literal pair) or
/// `model_ref` (track a named entry) must be present. Resolution against
/// the entry table happens inside the member-write transaction; this type
/// carries the unresolved form across that boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedCrewMember {
    pub platform: Option<String>,
    pub model: Option<String>,
    pub model_ref: Option<String>,
    pub timeout_minutes: Option<i64>,
    pub infra_retry_limit: Option<i64>,
    pub infra_crash_max_seconds: Option<i64>,
    pub infra_backoff_seconds: Option<i64>,
}

/// Reads one model entry by (trimmed) name: the `platform` plus optional
/// `model` a tracked member materializes.
pub type CrewModelLookup<'a> = &'a dyn Fn(&str) -> Result<Option<(String, Option<String>)>, String>;

impl UnresolvedCrewMember {
    /// Materialize one member: a literal keeps its own pair, a reference
    /// takes the entry's current pair. `lookup` reads the entry table.
    /// Errors name the member's index.
    pub fn resolve(&self, index: usize, lookup: CrewModelLookup<'_>) -> Result<CrewMember, String> {
        match (self.platform.as_ref(), self.model_ref.as_ref()) {
            (Some(_), Some(_)) | (None, None) => Err(format!(
                "member {index}: exactly one of platform/model_ref must be given"
            )),
            (Some(platform), None) => Ok(CrewMember {
                position: index as i64,
                platform: platform.clone(),
                model: self.model.clone(),
                model_ref: None,
                timeout_minutes: self.timeout_minutes,
                infra_retry_limit: self.infra_retry_limit,
                infra_crash_max_seconds: self.infra_crash_max_seconds,
                infra_backoff_seconds: self.infra_backoff_seconds,
            }),
            (None, Some(model_ref)) => {
                if self.model.is_some() {
                    return Err(format!(
                        "member {index}: model cannot be combined with model_ref"
                    ));
                }
                let name = model_ref.trim();
                let (platform, model) =
                    lookup(name)?.ok_or_else(|| format!("unknown model_ref '{name}'"))?;
                Ok(CrewMember {
                    position: index as i64,
                    platform,
                    model,
                    model_ref: Some(name.to_string()),
                    timeout_minutes: self.timeout_minutes,
                    infra_retry_limit: self.infra_retry_limit,
                    infra_crash_max_seconds: self.infra_crash_max_seconds,
                    infra_backoff_seconds: self.infra_backoff_seconds,
                })
            }
        }
    }
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

/// Validate a member list: 1..=`max` entries, each platform non-empty after
/// trim, each optional numeric non-negative. `max` is
/// `graphs.ensemble_max_members_sequential` (a crew is kind-agnostic, so it
/// takes the sequential cap; kind-specific bounds are enforced at bind time).
pub fn validate_crew_members(members: &[CrewMember], max: usize) -> Result<(), String> {
    if members.len() < CREW_MIN_MEMBERS {
        return Err(format!(
            "a crew needs at least {CREW_MIN_MEMBERS} member(s)"
        ));
    }
    if members.len() > max {
        return Err(format!(
            "a crew holds at most {max} members, got {}",
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
            model_ref: None,
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
        let default_max =
            crate::domain::settings::default_int("graphs.ensemble_max_members_sequential") as usize;
        assert!(validate_crew_members(&[member("opencode")], default_max).is_ok());
        assert!(validate_crew_members(&[], default_max).is_err());
        let many: Vec<CrewMember> = (0..default_max + 1)
            .map(|i| CrewMember {
                position: i as i64,
                ..member("opencode")
            })
            .collect();
        assert!(validate_crew_members(&many, default_max).is_err());
        let blank = CrewMember {
            platform: "   ".to_string(),
            ..member("opencode")
        };
        assert!(validate_crew_members(&[blank], default_max).is_err());
        let tracked = CrewMember {
            model_ref: Some("agy-main".to_string()),
            ..member("antigravity")
        };
        assert!(validate_crew_members(&[tracked], default_max).is_ok());
        let negative = CrewMember {
            timeout_minutes: Some(-1),
            ..member("opencode")
        };
        assert!(validate_crew_members(&[negative], default_max).is_err());
    }

    #[test]
    fn unresolved_member_resolve_rules() {
        let table = |name: &str| -> Result<Option<(String, Option<String>)>, String> {
            match name {
                "agy-main" => Ok(Some((
                    "antigravity".to_string(),
                    Some("gemini-4-hydrogen-medium".to_string()),
                ))),
                _ => Ok(None),
            }
        };
        let literal = UnresolvedCrewMember {
            platform: Some("opencode".to_string()),
            model: Some("m".to_string()),
            model_ref: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        let member = literal.resolve(0, &table).unwrap();
        assert_eq!(member.platform, "opencode");
        assert_eq!(member.model_ref, None);
        let tracked = UnresolvedCrewMember {
            platform: None,
            model: None,
            model_ref: Some("  agy-main ".to_string()),
            timeout_minutes: Some(5),
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        let member = tracked.resolve(2, &table).unwrap();
        assert_eq!(member.position, 2);
        assert_eq!(member.platform, "antigravity");
        assert_eq!(member.model.as_deref(), Some("gemini-4-hydrogen-medium"));
        assert_eq!(member.model_ref.as_deref(), Some("agy-main"));
        assert_eq!(member.timeout_minutes, Some(5));
        let both = UnresolvedCrewMember {
            platform: Some("x".to_string()),
            model: None,
            model_ref: Some("agy-main".to_string()),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        assert!(both.resolve(1, &table).unwrap_err().contains("member 1"));
        let neither = UnresolvedCrewMember {
            platform: None,
            model: None,
            model_ref: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        assert!(neither.resolve(3, &table).unwrap_err().contains("member 3"));
        let unknown = UnresolvedCrewMember {
            platform: None,
            model: None,
            model_ref: Some("nope".to_string()),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        let err = unknown.resolve(0, &table).unwrap_err();
        assert!(err.contains("unknown model_ref 'nope'"), "{err}");
    }

    #[test]
    fn member_validation_reads_the_cap() {
        let m: Vec<CrewMember> = (0..3)
            .map(|i| CrewMember {
                position: i as i64,
                ..member("opencode")
            })
            .collect();
        assert!(validate_crew_members(&m, 3).is_ok());
        assert!(validate_crew_members(&m, 2).is_err());
    }
}
