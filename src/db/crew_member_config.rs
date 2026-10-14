//! CM49: the member agent-node `config` builder used when a crew's members are
//! materialized into an ensemble's rows. Split out of `db::crews` to keep that
//! file within the size gate.

/// Build a member agent node's `config`, mirroring
/// `daemon::handler_ensemble_core::member_node_config` (kept local so the db
/// layer does not depend on the daemon layer).
pub(crate) fn member_node_config(
    platform: &str,
    model: Option<&str>,
    effective_prompt: &str,
    timeout_minutes: i64,
    infra_retry_limit: Option<i64>,
    infra_crash_max_seconds: Option<i64>,
    infra_backoff_seconds: Option<i64>,
) -> serde_json::Value {
    let mut config = serde_json::json!({
        "platform": platform,
        "model": model,
        "prompt_template": effective_prompt,
        "timeout_minutes": timeout_minutes,
    });
    if let serde_json::Value::Object(ref mut map) = config {
        if let Some(v) = infra_retry_limit {
            map.insert("infra_retry_limit".to_string(), serde_json::json!(v));
        }
        if let Some(v) = infra_crash_max_seconds {
            map.insert("infra_crash_max_seconds".to_string(), serde_json::json!(v));
        }
        if let Some(v) = infra_backoff_seconds {
            map.insert("infra_backoff_seconds".to_string(), serde_json::json!(v));
        }
    }
    config
}
