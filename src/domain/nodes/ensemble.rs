//! The config shape of an `ensemble` node document.
//!
//! An ensemble node document travels without harnesses: its `members` carry
//! per-member prompt overrides and timeouts, never a `platform` or a
//! `model` — which harness runs each member is the installer's decision,
//! supplied to `graph_ensemble`'s own `members` parameter. Parsing is
//! strict (`deny_unknown_fields`), so a document that pins a harness is
//! refused instead of silently carrying a binding the caller cannot see.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::graphs::{member_bounds, member_count_message, EnsembleKind, MemberCaps};

/// One ensemble member as it travels in a document: no platform, no model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnsembleNodeMember {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infra_retry_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infra_crash_max_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infra_backoff_seconds: Option<i64>,
}

/// An `ensemble` node document's `config`: the shared prompt, the member
/// templates, and every shape field `graph_ensemble` accepts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnsembleNodeConfig {
    pub prompt_template: String,
    /// Execution strategy: `parallel` (default), `cascade`, `round_robin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub members: Vec<EnsembleNodeMember>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_pass: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub straggler_timeout_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quorum_grace_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infra_retry_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infra_crash_max_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infra_backoff_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_rights: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutex: Option<Vec<String>>,
}

impl EnsembleNodeConfig {
    /// Parse a document's `config` object, naming the document in any
    /// refusal so the caller knows which file is at fault.
    pub fn from_value(document_name: &str, config: &Value) -> Result<Self, String> {
        serde_json::from_value(config.clone()).map_err(|error| {
            format!(
                "Ensemble node document '{document_name}' has an invalid config: {error}. Its config accepts prompt_template, kind, members (each without platform/model), min_pass, straggler_timeout_minutes, quorum_grace_minutes, timeout_minutes, infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds, commit_rights, and mutex."
            )
        })
    }

    /// The strategy the document asks for, defaulting to `parallel`.
    pub fn strategy(&self) -> Result<EnsembleKind, String> {
        match self.kind.as_deref().map(str::trim) {
            None | Some("") => Ok(EnsembleKind::Parallel),
            Some(raw) => EnsembleKind::from_str(raw).ok_or_else(|| {
                format!(
                    "Ensemble node document kind '{raw}' is invalid; must be one of: parallel, cascade, round_robin."
                )
            }),
        }
    }

    /// The shape rules every ensemble node document must meet before it can
    /// be installed or instantiated: a non-empty shared prompt, a member
    /// count inside the strategy's bounds, a valid `min_pass`, and
    /// non-negative timeouts.
    pub fn validate(&self, document_name: &str) -> Result<(), String> {
        if self.prompt_template.trim().is_empty() {
            return Err(format!(
                "Ensemble node document '{document_name}' must include a non-empty 'prompt_template'."
            ));
        }
        let kind = self.strategy()?;
        let caps = MemberCaps::default();
        let (min, max) = member_bounds(kind, caps);
        if self.members.len() < min || self.members.len() > max {
            return Err(format!(
                "Ensemble node document '{document_name}': {}",
                member_count_message(kind, caps, self.members.len())
            ));
        }
        let min_pass = self.min_pass.unwrap_or(self.members.len() as i64);
        crate::domain::validation::validate_ensemble_min_pass(
            document_name,
            min_pass,
            self.members.len(),
        )?;
        for member in &self.members {
            for (field, value) in [
                ("timeout_minutes", member.timeout_minutes),
                ("infra_retry_limit", member.infra_retry_limit),
                ("infra_crash_max_seconds", member.infra_crash_max_seconds),
                ("infra_backoff_seconds", member.infra_backoff_seconds),
            ] {
                if value.is_some_and(|value| value < 0) {
                    return Err(format!(
                        "Ensemble node document '{document_name}' member '{field}' must not be negative."
                    ));
                }
            }
        }
        for (field, value) in [
            ("straggler_timeout_minutes", self.straggler_timeout_minutes),
            ("quorum_grace_minutes", self.quorum_grace_minutes),
            ("timeout_minutes", self.timeout_minutes),
            ("infra_retry_limit", self.infra_retry_limit),
            ("infra_crash_max_seconds", self.infra_crash_max_seconds),
            ("infra_backoff_seconds", self.infra_backoff_seconds),
        ] {
            if value.is_some_and(|value| value < 0) {
                return Err(format!(
                    "Ensemble node document '{document_name}' field '{field}' must not be negative."
                ));
            }
        }
        if let Some(mutex) = &self.mutex {
            crate::domain::validation::validate_mutex_list(
                &format!("Ensemble node document '{document_name}'"),
                Some(mutex),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn proposers() -> Value {
        json!({
            "prompt_template": "draft it {{spec_content}}",
            "kind": "parallel",
            "members": [{}, {}, {}],
        })
    }

    #[test]
    fn config_parses_without_any_harness() {
        let config = EnsembleNodeConfig::from_value("proposers", &proposers()).unwrap();
        assert_eq!(config.strategy().unwrap(), EnsembleKind::Parallel);
        assert_eq!(config.members.len(), 3);
        config.validate("proposers").unwrap();
    }

    #[test]
    fn a_member_carrying_a_platform_is_refused() {
        let mut doc = proposers();
        doc["members"][0] = json!({"platform": "claude"});
        let error = EnsembleNodeConfig::from_value("proposers", &doc).unwrap_err();
        assert!(error.contains("proposers"), "{error}");
        assert!(error.contains("platform"), "{error}");
    }

    #[test]
    fn a_member_carrying_a_model_is_refused() {
        let mut doc = proposers();
        doc["members"][1] = json!({"model": "opus"});
        let error = EnsembleNodeConfig::from_value("proposers", &doc).unwrap_err();
        assert!(error.contains("model"), "{error}");
    }

    #[test]
    fn empty_prompt_template_is_refused() {
        let mut doc = proposers();
        doc["prompt_template"] = json!("  ");
        let config = EnsembleNodeConfig::from_value("proposers", &doc).unwrap();
        let error = config.validate("proposers").unwrap_err();
        assert!(error.contains("prompt_template"), "{error}");
    }

    #[test]
    fn too_few_members_is_refused_naming_the_document() {
        let mut doc = proposers();
        doc["members"] = json!([{}]);
        let config = EnsembleNodeConfig::from_value("proposers", &doc).unwrap();
        let error = config.validate("proposers").unwrap_err();
        assert!(error.contains("proposers"), "{error}");
        assert!(error.contains("2-8 members"), "{error}");
    }

    #[test]
    fn an_unknown_strategy_is_refused() {
        let mut doc = proposers();
        doc["kind"] = json!("race");
        let config = EnsembleNodeConfig::from_value("proposers", &doc).unwrap();
        let error = config.validate("proposers").unwrap_err();
        assert!(error.contains("race"), "{error}");
        assert!(error.contains("round_robin"), "{error}");
    }

    #[test]
    fn min_pass_above_the_member_count_is_refused() {
        let mut doc = proposers();
        doc["min_pass"] = json!(9);
        let config = EnsembleNodeConfig::from_value("proposers", &doc).unwrap();
        assert!(config.validate("proposers").is_err());
    }

    #[test]
    fn a_negative_timeout_is_refused() {
        let mut doc = proposers();
        doc["members"][0] = json!({"timeout_minutes": -1});
        let config = EnsembleNodeConfig::from_value("proposers", &doc).unwrap();
        let error = config.validate("proposers").unwrap_err();
        assert!(error.contains("timeout_minutes"), "{error}");
    }

    #[test]
    fn an_unknown_config_key_is_refused() {
        let mut doc = proposers();
        doc["platform"] = json!("claude");
        let error = EnsembleNodeConfig::from_value("proposers", &doc).unwrap_err();
        assert!(error.contains("invalid config"), "{error}");
    }
}
