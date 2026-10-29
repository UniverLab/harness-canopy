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
//!
//! Each resource tool also publishes a *typed* input schema, generated with
//! schemars from the same per-operation params structs: the
//! [`resource_action_object!`] macro declares one newtype per tool whose
//! `JsonSchema` merges every action's struct properties into one map
//! (`action` is a string enum of the tool's actions; `required` is
//! `["action"]` only, because which parameters an action requires is
//! action-dependent and cannot be expressed at the root — the tool
//! description carries the per-action required/optional table). A
//! parameter shared by several actions with different shapes is declared
//! once as the `anyOf` union of its shapes.
//!
//! Tolerance for clients that stringify: some MCP clients send every
//! argument as a JSON string. Dispatch sites therefore run the newtype's
//! `coerce` right before [`ToolSpec::validate`] — when a parameter declared
//! object, array, integer, number or boolean arrives as a JSON string that
//! parses to that same type, the parsed value is substituted; anything else
//! (unparseable strings, kind mismatches, undeclared or string-typed
//! parameters) is left untouched so the existing validation and
//! deserialization errors still apply.

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

// ── Typed per-tool input schemas (FR1) ───────────────────────────────

/// The non-string JSON types a stringified parameter may be parsed back
/// into (FR3). `string` and `null` are deliberately absent: a string is
/// already legal JSON on the wire, so there is nothing to coerce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoercibleKind {
    Object,
    Array,
    Integer,
    Number,
    Boolean,
}

/// Merges one params struct's `properties` into the tool's per-name shape
/// list: identical shapes dedupe to one entry, different shapes for the
/// same name are kept side by side (published as an `anyOf` union).
pub(crate) fn merge_params_schema<T: rmcp::schemars::JsonSchema>(
    generator: &mut rmcp::schemars::SchemaGenerator,
    properties: &mut std::collections::BTreeMap<String, Vec<Value>>,
) {
    let schema = generator.root_schema_for::<T>().to_value();
    let Some(incoming) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    for (name, shape) in incoming {
        let shapes = properties.entry(name.clone()).or_default();
        if !shapes.contains(shape) {
            shapes.push(shape.clone());
        }
    }
}

/// The published schema of one merged parameter: a single shape stays
/// as-is; several shapes become their deduped `anyOf` union.
pub(crate) fn union_shape(shapes: &[Value]) -> Value {
    if let [only] = shapes {
        only.clone()
    } else {
        serde_json::json!({ "anyOf": shapes })
    }
}

/// Coerce stringified non-string parameters (FR3, module doc): a string
/// value for a parameter whose declared schema is object, array, integer,
/// number or boolean is parsed back into that type when the parsed JSON
/// matches the declared kind; every other value — including strings that
/// do not parse or parse to another kind — is left untouched so the
/// existing validation and deserialization errors still apply.
pub(crate) fn coerce_stringified_params(
    params: &mut Value,
    shapes: &std::collections::BTreeMap<String, Vec<Value>>,
) {
    let Some(object) = params.as_object_mut() else {
        return;
    };
    for (name, value) in object.iter_mut() {
        if name == "action" || !value.is_string() {
            continue;
        }
        let Some(kind) = shapes.get(name).and_then(coercible_kind) else {
            continue;
        };
        let Some(text) = value.as_str() else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        if kind_matches(kind, &parsed) {
            *value = parsed;
        }
    }
}

/// The one coercible kind every shape of a parameter agrees on, if any: a
/// union of different kinds means the plain string may be the intended
/// value, so such a parameter is never coerced.
fn coercible_kind<'a>(shapes: impl IntoIterator<Item = &'a Value>) -> Option<CoercibleKind> {
    let mut kinds = shapes.into_iter().map(shape_kind);
    let first = kinds.next()?;
    first.filter(|kind| kinds.all(|other| other == Some(*kind)))
}

/// The coercible kind one schema declares, if any. Handles a direct
/// `"type"`, a nullable `["integer", "null"]` type list, structural
/// (`properties`/`items`) object/array schemas, and `anyOf`/`oneOf`
/// unions whose non-null variants all agree on one kind.
fn shape_kind(shape: &Value) -> Option<CoercibleKind> {
    match shape.get("type") {
        Some(Value::String(single)) => kind_from_name(single),
        Some(Value::Array(candidates)) => {
            let mut found: Option<CoercibleKind> = None;
            for candidate in candidates.iter().filter_map(Value::as_str) {
                if candidate == "null" {
                    continue;
                }
                let kind = kind_from_name(candidate)?;
                if found.is_some_and(|previous| previous != kind) {
                    return None;
                }
                found = Some(kind);
            }
            found
        }
        Some(_) => None,
        None => {
            if shape.get("properties").is_some() {
                return Some(CoercibleKind::Object);
            }
            if shape.get("items").is_some() {
                return Some(CoercibleKind::Array);
            }
            for combinator in ["anyOf", "oneOf"] {
                if let Some(variants) = shape.get(combinator).and_then(Value::as_array) {
                    return coercible_kind(variants.iter().filter(|variant| {
                        variant.get("type").and_then(Value::as_str) != Some("null")
                    }));
                }
            }
            None
        }
    }
}

fn kind_from_name(name: &str) -> Option<CoercibleKind> {
    match name {
        "object" => Some(CoercibleKind::Object),
        "array" => Some(CoercibleKind::Array),
        "integer" => Some(CoercibleKind::Integer),
        "number" => Some(CoercibleKind::Number),
        "boolean" => Some(CoercibleKind::Boolean),
        _ => None,
    }
}

fn kind_matches(kind: CoercibleKind, value: &Value) -> bool {
    match kind {
        CoercibleKind::Object => value.is_object(),
        CoercibleKind::Array => value.is_array(),
        CoercibleKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        CoercibleKind::Number => value.is_number(),
        CoercibleKind::Boolean => value.is_boolean(),
    }
}

/// Declares the typed input-parameter newtype of one resource tool.
///
/// Invocation: `resource_action_object!(SpecObject, SPEC, [("list",
/// SpecListParams), …])` — one pair per action arm that calls
/// [`action_params`], naming that arm's params struct (armless actions,
/// e.g. `harness refresh_registry`, add nothing). `JsonSchema` is
/// per-type while [`ActionObject`] is shared, hence one newtype per tool.
/// The newtype wraps [`ActionObject`] (deserialization is unchanged) and
/// adds the merged typed schema plus [`Self::coerce`] — the
/// stringified-client tolerance run right before [`ToolSpec::validate`]
/// (module doc). The merged shapes are built once (one schemars
/// generator, draft 2020-12 with `inline_subschemas` so nested `$ref`s
/// vanish and properties merge directly) and cached for both consumers.
macro_rules! resource_action_object {
    ($newtype:ident, $tool:ident, [$(($action:literal, $params:ty)),+ $(,)?]) => {
        /// Typed input parameters for a resource tool: [`ActionObject`]
        /// plus the tool's merged, per-action typed JSON schema.
        #[derive(Debug, Clone, serde::Deserialize)]
        pub(crate) struct $newtype(pub(crate) crate::daemon::actions::ActionObject);

        impl $newtype {
            /// Every action params struct's `properties` merged per name
            /// (identical shapes deduped; disagreements kept as a list,
            /// published as `anyOf`), generated once per tool.
            fn parameter_shapes() -> &'static std::collections::BTreeMap<
                String,
                Vec<serde_json::Value>,
            > {
                static SHAPES: std::sync::OnceLock<
                    std::collections::BTreeMap<String, Vec<serde_json::Value>>,
                > = std::sync::OnceLock::new();
                SHAPES.get_or_init(|| {
                    let mut generator = rmcp::schemars::generate::SchemaSettings::draft2020_12()
                        .with(|settings| settings.inline_subschemas = true)
                        .into_generator();
                    let mut properties = std::collections::BTreeMap::new();
                    $(
                        crate::daemon::actions::merge_params_schema::<$params>(
                            &mut generator,
                            &mut properties,
                        );
                    )+
                    properties
                })
            }

            /// The stringified-client tolerance (module doc): rewrite in
            /// place any string value that parses to the JSON kind its
            /// merged schema declares; leave everything else untouched.
            pub(crate) fn coerce(params: &mut serde_json::Value) {
                crate::daemon::actions::coerce_stringified_params(params, Self::parameter_shapes());
            }

            /// The raw JSON object the dispatch validates.
            pub(crate) fn into_value(self) -> serde_json::Value {
                self.0.into_value()
            }
        }

        impl rmcp::schemars::JsonSchema for $newtype {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($newtype))
            }

            fn json_schema(
                _generator: &mut rmcp::schemars::SchemaGenerator,
            ) -> rmcp::schemars::Schema {
                let mut properties = serde_json::Map::new();
                properties.insert(
                    "action".to_string(),
                    serde_json::json!({
                        "type": "string",
                        "enum": $tool.actions.iter().map(|action| action.name).collect::<Vec<_>>(),
                    }),
                );
                for (name, shapes) in Self::parameter_shapes() {
                    properties.insert(name.clone(), crate::daemon::actions::union_shape(shapes));
                }
                serde_json::from_value(serde_json::json!({
                    "type": "object",
                    "properties": properties,
                    "required": ["action"],
                }))
                .expect("static resource tool schema")
            }
        }
    };
}
pub(crate) use resource_action_object;

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

    // ── stringified-client coercion (FR3) ─────────────────────────

    fn demo_shapes() -> std::collections::BTreeMap<String, Vec<Value>> {
        [
            (
                "limit".to_string(),
                vec![json!({"type": ["integer", "null"]})],
            ),
            (
                "config".to_string(),
                vec![json!({"type": ["object", "null"]})],
            ),
            (
                "members".to_string(),
                vec![json!({"type": ["array", "null"], "items": {"type": "string"}})],
            ),
            (
                "enabled".to_string(),
                vec![json!({"type": ["boolean", "null"]})],
            ),
            (
                "workdir".to_string(),
                vec![json!({"type": ["string", "null"]})],
            ),
            (
                "mixed".to_string(),
                vec![
                    json!({"anyOf": [{"type": "integer"}, {"type": "string"}]}),
                    json!({"type": "integer"}),
                ],
            ),
        ]
        .into_iter()
        .collect()
    }

    fn coerced(params: &Value) -> Value {
        let mut params = params.clone();
        coerce_stringified_params(&mut params, &demo_shapes());
        params
    }

    #[test]
    fn coerce_parses_stringified_integer() {
        let out = coerced(&json!({ "action": "list", "limit": "5" }));
        assert_eq!(out["limit"], json!(5));
    }

    #[test]
    fn coerce_parses_stringified_object_array_and_boolean() {
        let out = coerced(&json!({
            "config": "{\"command\":\"true\"}",
            "members": "[\"a\",\"b\"]",
            "enabled": "true",
        }));
        assert_eq!(out["config"], json!({"command": "true"}));
        assert_eq!(out["members"], json!(["a", "b"]));
        assert_eq!(out["enabled"], json!(true));
    }

    #[test]
    fn coerce_leaves_unparseable_or_kind_mismatched_strings_untouched() {
        let out = coerced(&json!({ "limit": "abc", "config": "not json", "enabled": "5" }));
        assert_eq!(out["limit"], json!("abc"));
        assert_eq!(out["config"], json!("not json"));
        assert_eq!(out["enabled"], json!("5"));
    }

    #[test]
    fn coerce_never_touches_strings_declared_string_or_union_kinds_or_action() {
        let out = coerced(&json!({
            "action": "5",
            "workdir": "\"5\"",
            "mixed": "5",
        }));
        assert_eq!(out["action"], json!("5"));
        assert_eq!(out["workdir"], json!("\"5\""));
        assert_eq!(out["mixed"], json!("5"));
    }

    #[test]
    fn coerce_leaves_non_string_values_untouched() {
        let out = coerced(&json!({ "limit": 7, "config": {"a": 1}, "workdir": 3 }));
        assert_eq!(out["limit"], json!(7));
        assert_eq!(out["config"], json!({"a": 1}));
        assert_eq!(out["workdir"], json!(3));
    }

    // ── resource_action_object! typed schema (FR1) ────────────────

    #[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
    struct DemoListParams {
        workdir: Option<String>,
        limit: Option<u32>,
    }

    #[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
    struct DemoGetParams {
        id: String,
        compact: Option<bool>,
    }

    #[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
    struct DemoRenameParams {
        /// Recipient id, renamed per action.
        id: Option<String>,
        name: String,
    }

    resource_action_object!(
        DemoObject,
        TOOL,
        [
            ("list", DemoListParams),
            ("get", DemoGetParams),
            ("rename", DemoRenameParams),
        ]
    );

    fn demo_object_schema() -> Value {
        use rmcp::schemars::JsonSchema;
        DemoObject::json_schema(&mut rmcp::schemars::SchemaGenerator::default()).to_value()
    }

    #[test]
    fn macro_schema_declares_action_enum_and_root_required() {
        let schema = demo_object_schema();
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["required"], json!(["action"]));
        assert_eq!(schema["properties"]["action"]["type"], json!("string"));
        assert_eq!(
            schema["properties"]["action"]["enum"],
            json!(["list", "get", "rename"]),
            "action enum must list every action of the tool"
        );
    }

    #[test]
    fn macro_schema_merges_params_properties_with_types() {
        let props = &demo_object_schema()["properties"];
        assert!(props["workdir"].to_string().contains("string"), "{props}");
        assert!(
            props["limit"].to_string().contains("integer"),
            "limit must declare integer: {props}"
        );
        assert!(
            props["compact"].to_string().contains("boolean"),
            "compact must declare boolean: {props}"
        );
        assert!(props["name"].to_string().contains("string"), "{props}");
    }

    #[test]
    fn macro_schema_unions_same_name_different_shape_once() {
        // `id` is a required string in `get` and optional in `rename`:
        // declared once, as the anyOf union of both shapes.
        let props = &demo_object_schema()["properties"];
        let id = &props["id"];
        let variants = id
            .get("anyOf")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("id must be an anyOf union: {id}"));
        assert_eq!(variants.len(), 2, "both shapes, deduped: {id}");
        assert!(id.get("type").is_none(), "one declaration only: {id}");
    }

    #[test]
    fn macro_newtype_deserializes_into_action_params_structs() {
        // Reading every demo field here keeps the fixtures honest and pins
        // the newtype still dispatches into the same typed structs.
        let list: DemoListParams =
            action_params(&json!({ "action": "list", "workdir": "/w", "limit": 3 }))
                .expect("list params");
        assert_eq!(list.workdir.as_deref(), Some("/w"));
        assert_eq!(list.limit, Some(3));
        let get: DemoGetParams =
            action_params(&json!({ "action": "get", "id": "x" })).expect("get params");
        assert_eq!(get.id, "x");
        assert!(!get.compact.unwrap_or(false));
        let rename: DemoRenameParams =
            action_params(&json!({ "action": "rename", "id": "y", "name": "z" }))
                .expect("rename params");
        assert_eq!(rename.id.as_deref(), Some("y"));
        assert_eq!(rename.name, "z");
    }

    #[test]
    fn macro_coerce_rewrites_stringified_declared_params() {
        let mut params = json!({ "action": "list", "limit": "5" });
        DemoObject::coerce(&mut params);
        assert_eq!(params["limit"], json!(5));
        assert_eq!(params["action"], json!("list"));
        let parsed = DemoObject::into_value(DemoObject(ActionObject(
            params.as_object().cloned().expect("object"),
        )));
        assert_eq!(parsed["limit"], json!(5));
    }
}
