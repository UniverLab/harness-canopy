//! Shared action dispatch for Canopy's resource tools (FR1).
//!
//! The MCP surface is moving from one tool per operation to a handful of
//! resource tools, each dispatching on an `action` parameter. This module
//! holds the value-based contract every such tool validates against before
//! its handler touches any logic: a [`ToolSpec`] declares its actions, each
//! with required and optional JSON parameter names. [`ToolSpec::validate`]
//! turns a bad call — unknown action, missing required parameter, or a
//! parameter not declared for that action — into ONE error that lists every
//! action of the tool with its required and optional parameters, so an
//! agent can correct the call from the refusal alone.
//!
//! The parameter *shapes* stay in the existing per-operation params structs
//! (FR: keep param names identical); the tables here only name the JSON
//! keys, split by required (non-`Option`, no serde default) vs optional.

use rmcp::ErrorData as McpError;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{Map, Value};

/// Free-form object parameter carrier for resource tools. The MCP spec
/// requires a root `type: object` input schema, which a bare
/// `serde_json::Value` does not provide; the real accepted shape is
/// validated against the tool's own action table by [`ToolSpec::validate`]
/// before anything else runs.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ActionObject(pub(crate) Map<String, Value>);

impl rmcp::schemars::JsonSchema for ActionObject {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ActionObject".into()
    }

    fn json_schema(_generator: &mut rmcp::schemars::SchemaGenerator) -> rmcp::schemars::Schema {
        serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": { "action": { "type": "string" } }
        }))
        .expect("static object schema")
    }
}

impl ActionObject {
    /// The raw JSON object the dispatch validates.
    pub(crate) fn into_value(self) -> Value {
        Value::Object(self.0)
    }
}

/// One action of a resource tool: its name plus the JSON parameter names it
/// accepts, split into required and optional.
pub(crate) struct ActionSpec {
    /// The action's dispatch value (the `action` parameter).
    pub(crate) name: &'static str,
    /// JSON parameter names the action requires (must be present, non-null).
    pub(crate) required: &'static [&'static str],
    /// JSON parameter names the action accepts besides `action` and the
    /// required ones. Anything else is refused.
    pub(crate) optional: &'static [&'static str],
}

/// One resource tool: its MCP name plus every action it dispatches.
pub(crate) struct ToolSpec {
    /// The tool's MCP name.
    pub(crate) name: &'static str,
    /// Every action the tool accepts, in listing order.
    pub(crate) actions: &'static [ActionSpec],
}

impl ToolSpec {
    /// The declared action with this name, if any.
    pub(crate) fn action(&self, name: &str) -> Option<&'static ActionSpec> {
        self.actions.iter().find(|action| action.name == name)
    }

    /// Validate one call against this tool's action table. Returns the
    /// validated action's name (borrowed from `params`) on success; on any
    /// refusal — `action` missing or not a string, unknown action, a
    /// required parameter missing or null, or a parameter not declared for
    /// the action — returns ONE error whose message lists every action with
    /// its required and optional parameters.
    pub(crate) fn validate<'v>(&self, params: &'v Value) -> Result<&'v str, McpError> {
        let Some(object) = params.as_object() else {
            return Err(self.refusal(&format!(
                "Parameters must be a JSON object with an `action` field; received {}.",
                json_kind(params)
            )));
        };
        let Some(action) = object.get("action").and_then(Value::as_str) else {
            return Err(self.refusal("Missing `action` (a string naming which action to run)."));
        };
        let Some(spec) = self.action(action) else {
            return Err(self.refusal(&format!("Unknown action '{action}'.")));
        };

        let missing: Vec<&str> = spec
            .required
            .iter()
            .copied()
            .filter(|name| !is_present(object.get(*name)))
            .collect();
        if !missing.is_empty() {
            return Err(self.refusal(&format!(
                "Action '{action}' is missing required parameter(s): {}.",
                missing.join(", ")
            )));
        }

        let undeclared: Vec<&str> = object
            .keys()
            .map(String::as_str)
            .filter(|key| *key != "action")
            .filter(|key| !spec.required.contains(key) && !spec.optional.contains(key))
            .collect();
        if !undeclared.is_empty() {
            return Err(self.refusal(&format!(
                "Action '{action}' does not declare parameter(s): {}.",
                undeclared.join(", ")
            )));
        }

        Ok(action)
    }

    /// The single refusal shape: the specific problem, then every action of
    /// the tool with its required and optional parameters.
    fn refusal(&self, problem: &str) -> McpError {
        let mut table = format!("{problem} Tool `{}` accepts:", self.name);
        for action in self.actions {
            table.push_str(&format!(
                "\n- `{}` — required: {}; optional: {}.",
                action.name,
                if action.required.is_empty() {
                    "(none)".to_string()
                } else {
                    action.required.join(", ")
                },
                if action.optional.is_empty() {
                    "(none)".to_string()
                } else {
                    action.optional.join(", ")
                },
            ));
        }
        McpError::invalid_params(table, None)
    }
}

/// Deserialize a validated call's parameters into the action's typed params
/// struct: `action` is the dispatch key and is removed before deserialization
/// (a struct field named `action` would otherwise always receive the
/// dispatch value — the reason `GraphContinueParams` renamed its own to
/// `mode`). Shapes and enrichment (field path, received value, minimal
/// example) come from the same `deserialize_params` every typed tool uses.
pub(crate) fn action_params<T: DeserializeOwned + rmcp::schemars::JsonSchema>(
    params: &Value,
) -> Result<T, McpError> {
    let mut map = match params {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    map.remove("action");
    crate::daemon::params_extract::deserialize_params(&Value::Object(map))
}

/// A short JSON type name for the refusal message.
fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// A required parameter counts as present when its key exists and is not
/// explicit `null`.
fn is_present(value: Option<&Value>) -> bool {
    value.is_some_and(|value| !value.is_null())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ErrorCode;
    use serde::Deserialize;
    use serde_json::json;

    const TOOL: ToolSpec = ToolSpec {
        name: "demo_tool",
        actions: &[
            ActionSpec {
                name: "list",
                required: &[],
                optional: &["workdir", "limit"],
            },
            ActionSpec {
                name: "get",
                required: &["id"],
                optional: &["compact"],
            },
            ActionSpec {
                name: "rename",
                required: &["id", "name"],
                optional: &[],
            },
        ],
    };

    fn refusal_message(params: &Value) -> String {
        match TOOL.validate(params) {
            Ok(action) => panic!("expected a refusal, got Ok({action})"),
            Err(err) => {
                assert_eq!(err.code, ErrorCode::INVALID_PARAMS, "wrong error code");
                err.message.to_string()
            }
        }
    }

    fn assert_lists_full_table(message: &str) {
        for action in TOOL.actions {
            assert!(
                message.contains(&format!("`{}`", action.name)),
                "refusal must list action `{}`: {message}",
                action.name
            );
            assert!(
                message.contains(action.required.join(", ").as_str()) || action.required.is_empty(),
                "refusal must list required params of `{}`: {message}",
                action.name
            );
            assert!(
                message.contains(action.optional.join(", ").as_str()) || action.optional.is_empty(),
                "refusal must list optional params of `{}`: {message}",
                action.name
            );
        }
    }

    #[test]
    fn unknown_action_refusal_lists_every_action_with_its_params() {
        let message = refusal_message(&json!({ "action": "frobnicate", "id": "x" }));
        assert!(message.contains("Unknown action 'frobnicate'"), "{message}");
        assert_lists_full_table(&message);
    }

    #[test]
    fn missing_required_param_refusal_lists_every_action_with_its_params() {
        // `rename` requires `id` and `name`; only `id` is given.
        let message = refusal_message(&json!({ "action": "rename", "id": "x" }));
        assert!(
            message.contains("missing required parameter(s): name"),
            "{message}"
        );
        assert_lists_full_table(&message);
    }

    #[test]
    fn undeclared_param_refusal_lists_every_action_with_its_params() {
        // `get` declares `id` and `compact`, not `frobnicate`.
        let message = refusal_message(&json!({ "action": "get", "id": "x", "frobnicate": 1 }));
        assert!(
            message.contains("does not declare parameter(s): frobnicate"),
            "{message}"
        );
        assert_lists_full_table(&message);
    }

    #[test]
    fn missing_action_key_is_also_a_table_refusal() {
        let message = refusal_message(&json!({ "id": "x" }));
        assert!(message.contains("Missing `action`"), "{message}");
        assert_lists_full_table(&message);
    }

    #[test]
    fn non_object_parameters_are_refused_with_the_table() {
        let message = refusal_message(&json!("list"));
        assert!(
            message.contains("Parameters must be a JSON object"),
            "{message}"
        );
        assert_lists_full_table(&message);
    }

    #[test]
    fn explicit_null_required_param_counts_as_missing() {
        let message = refusal_message(&json!({ "action": "get", "id": null }));
        assert!(
            message.contains("missing required parameter(s): id"),
            "{message}"
        );
    }

    #[test]
    fn valid_calls_validate_and_return_the_action() {
        assert_eq!(
            TOOL.validate(&json!({ "action": "list" }))
                .expect("list is valid"),
            "list"
        );
        assert_eq!(
            TOOL.validate(&json!({ "action": "get", "id": "x", "compact": true }))
                .expect("get is valid"),
            "get"
        );
    }

    #[test]
    fn action_params_strips_the_dispatch_key_before_deserializing() {
        #[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
        struct Demo {
            id: String,
        }
        let params = json!({ "action": "get", "id": "x" });
        let parsed: Demo = action_params(&params).expect("deserializes");
        assert_eq!(parsed.id, "x");
    }
}
