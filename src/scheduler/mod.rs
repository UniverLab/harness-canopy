//! Schedule parsing, variable substitution, and internal cron scheduling.
//!
//! Provides:
//! - Cron expression validation
//! - Prompt template variable substitution
//! - Internal cron scheduler (tokio-based, replaces OS crontab/launchd)

pub mod cron_scheduler;

use chrono::Utc;

/// Substitute template variables in a background_agent prompt.
///
/// Supported variables:
/// - `{{TIMESTAMP}}` — current ISO 8601 timestamp
/// - `{{TASK_ID}}` — the background_agent's ID
/// - `{{LOG_PATH}}` — the background_agent's log file path
/// - `{{FILE_PATH}}` — the watched file path (watchers only)
/// - `{{EVENT_TYPE}}` — the event type (watchers only)
pub fn substitute_variables(
    prompt: &str,
    background_agent_id: &str,
    log_path: &str,
    file_path: Option<&str>,
    event_type: Option<&str>,
) -> String {
    let timestamp = Utc::now().to_rfc3339();
    let mut result = prompt.to_string();

    result = result.replace("{{TIMESTAMP}}", &timestamp);
    result = result.replace("{{TASK_ID}}", background_agent_id);
    result = result.replace("{{LOG_PATH}}", log_path);

    if let Some(path) = file_path {
        result = result.replace("{{FILE_PATH}}", path);
    }
    if let Some(event) = event_type {
        result = result.replace("{{EVENT_TYPE}}", event);
    }

    result
}

/// Validate that a string is a valid 5-field cron expression.
///
/// The model is responsible for converting natural language to cron.
/// This function only validates the format.
pub fn validate_cron(expr: &str) -> bool {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return false;
    }
    for part in parts {
        if part.is_empty() {
            return false;
        }
        // Allow: digits, /, -, comma, *
        if part != "*"
            && !part
                .chars()
                .all(|c| c.is_ascii_digit() || c == '/' || c == '-' || c == ',' || c == '*')
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests;
