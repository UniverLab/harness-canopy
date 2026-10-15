use rmcp::schemars;
use serde::{Deserialize, Serialize};

mod agents;
pub use agents::*;
mod sync_intel;
pub use sync_intel::*;
mod rag;
pub use rag::*;
mod misc;
pub use misc::*;
mod graph_core;
pub use graph_core::*;
mod graph_access;
pub use graph_access::*;
mod graph_crew;
pub use graph_crew::*;
mod roster;
pub use roster::*;
mod crew;
pub use crew::*;
mod settings;
pub use settings::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for T22: `agent_update` (MCP tool `agent_update`)
    /// previously appeared to fail on cron schedules containing `*`
    /// (e.g. "30 * * * *") with "JSON Parse error: Unexpected EOF". This
    /// pins that `TaskUpdateParams` deserializes such a schedule string
    /// correctly — `*` is an ordinary JSON string character and requires
    /// no special handling in serde.
    #[test]
    fn task_update_params_deserializes_cron_schedule_with_asterisks() {
        let value = serde_json::json!({
            "id": "x",
            "schedule": "30 * * * *"
        });

        let params: TaskUpdateParams = serde_json::from_value(value).expect("should deserialize");

        assert_eq!(params.id, "x");
        assert_eq!(params.schedule, Some("30 * * * *".to_string()));
    }

    #[test]
    fn task_update_params_deserializes_cron_schedule_with_asterisks_from_str() {
        let raw = r#"{"id":"x","schedule":"*/5 * * * *"}"#;

        let params: TaskUpdateParams = serde_json::from_str(raw).expect("should deserialize");

        assert_eq!(params.schedule, Some("*/5 * * * *".to_string()));
    }

    /// Regression test for bug D: the JSON Schema for `config` used to omit
    /// "type", so MCP clients would serialize it as a JSON-encoded string
    /// instead of an object. Pin that the generated schema now declares
    /// `config` as an object.
    #[test]
    fn graph_add_node_params_schema_declares_config_as_object() {
        let schema = schemars::schema_for!(GraphAddNodeParams);
        let value = serde_json::to_value(&schema).expect("schema should serialize");
        let config_schema = &value["properties"]["config"];
        // `config` is now optional (an alternative to `blueprint`), so the
        // "object" type may appear directly or nested under a $ref/anyOf
        // produced for `Option<..>`.
        let type_str = config_schema["type"].as_str();
        assert!(
            type_str == Some("object") || config_schema.to_string().contains("\"object\""),
            "expected config schema to declare an object type, got {config_schema}"
        );
    }

    #[test]
    fn graph_update_node_params_schema_declares_config_as_object() {
        let schema = schemars::schema_for!(GraphUpdateNodeParams);
        let value = serde_json::to_value(&schema).expect("schema should serialize");
        let config_schema = &value["properties"]["config"];
        // Optional fields are wrapped, so the "object" type may appear either
        // directly or nested under a $ref/anyOf produced for `Option<..>`.
        let type_str = config_schema["type"].as_str();
        assert!(
            type_str == Some("object") || config_schema.to_string().contains("\"object\""),
            "expected config schema to declare an object type, got {config_schema}"
        );
    }

    #[test]
    fn graph_add_node_params_rejects_string_config() {
        let value = serde_json::json!({
            "spec_id": "spec-1",
            "name": "n",
            "kind": "agent",
            "config": "{\"platform\": \"claude\"}"
        });

        let error = serde_json::from_value::<GraphAddNodeParams>(value).unwrap_err();
        assert!(
            error.to_string().contains("invalid type"),
            "expected a type error, got: {error}"
        );
    }

    #[test]
    fn graph_add_node_params_accepts_object_config() {
        let value = serde_json::json!({
            "spec_id": "spec-1",
            "name": "n",
            "kind": "agent",
            "config": { "platform": "claude" }
        });

        let params: GraphAddNodeParams =
            serde_json::from_value(value).expect("object config should deserialize");
        assert_eq!(
            params
                .config
                .as_ref()
                .and_then(|c| c.get("platform"))
                .and_then(|v| v.as_str()),
            Some("claude")
        );
    }

    #[test]
    fn graph_update_node_params_rejects_string_config() {
        let value = serde_json::json!({
            "node_id": "node-1",
            "config": "{\"platform\": \"claude\"}"
        });

        let error = serde_json::from_value::<GraphUpdateNodeParams>(value).unwrap_err();
        assert!(
            error.to_string().contains("invalid type"),
            "expected a type error, got: {error}"
        );
    }

    /// (CB25) `GraphPreflightParams` deserializes identically with and
    /// without the optional `reviewer` field — omitting it must leave
    /// `reviewer` as `None` (probe behaviour unchanged, never an
    /// implicit review), and a platform-only reviewer defaults its
    /// model to `None` (platform default).
    #[test]
    fn graph_preflight_params_deserializes_without_reviewer() {
        let bare: GraphPreflightParams = serde_json::from_value(serde_json::json!({
            "graph_id": "x",
        }))
        .expect("should deserialize without reviewer");
        assert_eq!(bare.graph_id, "x");
        assert!(
            bare.reviewer.is_none(),
            "omitted reviewer must deserialize to None"
        );

        let with_reviewer: GraphPreflightParams = serde_json::from_value(serde_json::json!({
            "graph_id": "x",
            "reviewer": { "platform": "claude" },
        }))
        .expect("should deserialize with reviewer");
        let reviewer = with_reviewer.reviewer.expect("reviewer must be present");
        assert_eq!(reviewer.platform, "claude");
        assert!(
            reviewer.model.is_none(),
            "omitted model must default to None"
        );
    }

    /// CM18: async stays the default (FR2). Omitting `blocking` must
    /// deserialize to `None` (falsy → today's id-and-poll path), and an
    /// explicit `blocking: true` opts into the inline-result path.
    #[test]
    fn subagent_spawn_params_blocking_defaults_to_async() {
        let bare: SubagentSpawnParams =
            serde_json::from_value(serde_json::json!({ "prompt": "hi" }))
                .expect("should deserialize without blocking");
        assert_eq!(bare.prompt, "hi");
        assert!(
            bare.blocking.is_none(),
            "omitted blocking must deserialize to None"
        );
        assert!(
            !bare.blocking.unwrap_or(false),
            "omitted blocking must take the async path"
        );

        let explicit: SubagentSpawnParams = serde_json::from_value(serde_json::json!({
            "prompt": "hi",
            "blocking": true,
        }))
        .expect("should deserialize with blocking");
        assert_eq!(explicit.blocking, Some(true));
    }
}
