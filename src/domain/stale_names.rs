//! Stale 2.x reference detection — extracted from `validation.rs` to stay
//! below the rust-quality.sh file-size limit. Public API re-exported through
//! `crate::domain::validation` so callers are unaffected.

/// Every `graph_*` MCP tool name registered in `src/daemon/handler.rs` as of
/// 3.x (34 tools, verified 2026-09-23 by grepping every `name = "graph_..."`
/// in that file). Kept as a literal list — not derived at runtime from the
/// tool router — so this pure `domain` module never depends on `daemon`.
/// CB68: used to build the set of removed 2.x `loop_<name>` tool names that
/// `graph_preflight` and `canopy doctor` warn about.
pub const GRAPH_TOOL_NAMES: &[&str] = &[
    "graph_create",
    "graph_update",
    "graph_add_spec",
    "graph_update_spec",
    "graph_remove_spec",
    "graph_add_node",
    "graph_update_node",
    "graph_add_edge",
    "graph_update_edge",
    "graph_delete_edge",
    "graph_delete_node",
    "graph_add_ensemble",
    "graph_copy_node",
    "graph_copy_ensemble",
    "graph_update_ensemble",
    "graph_delete_ensemble",
    "graph_get",
    "graph_export",
    "graph_import",
    "graph_audit_node_configs",
    "graph_list",
    "graph_node_runs_list",
    "graph_node_run_get",
    "graph_preflight",
    "graph_run",
    "graph_reset",
    "graph_schedule_autorun",
    "graph_schedule_continue",
    "graph_pause",
    "graph_archive",
    "graph_restore",
    "graph_continue",
    "graph_complete_node",
    "graph_report_blocker",
];

/// One stale 2.x reference found in stored prompt/hook/command text: the
/// exact stale token and its 3.x replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleNameHit {
    pub old: String,
    pub new: String,
}

/// True if `needle` occurs in `haystack` as a whole word/phrase — not as a
/// substring of a longer identifier (so scanning for `loop_run` does not
/// also match `loop_running_something`, and `canopy loop` does not match
/// inside `canopy loopback`).
pub(crate) fn contains_word(haystack: &str, needle: &str) -> bool {
    fn is_ident_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }
    let bytes = haystack.as_bytes();
    let nlen = needle.len();
    if nlen == 0 {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let idx = start + pos;
        let before_ok = idx == 0 || !is_ident_byte(bytes[idx - 1]);
        let after_idx = idx + nlen;
        let after_ok = after_idx >= bytes.len() || !is_ident_byte(bytes[after_idx]);
        if before_ok && after_ok {
            return true;
        }
        start = idx + 1;
    }
    false
}

/// CB68 FR2/FR3: scan `text` (a node/ensemble/hook prompt, a hook command,
/// or a scheduled agent's prompt) for a removed 2.x `loop_<name>` MCP tool
/// name or the gone `canopy loop` CLI subcommand. Returns one hit per
/// distinct stale token found, each paired with its exact 3.x replacement.
/// Deliberately narrow: only mentions of a *known former tool name* (built
/// from [`GRAPH_TOOL_NAMES`]) or the literal `canopy loop` phrase are
/// reported — an unrelated `loop_`-prefixed word (e.g. a stale `loop_id`
/// mention in prose) is not a removed tool and is out of scope for this
/// detector (FR2 says "for any removed tool", not "any loop_ word").
pub fn find_stale_2x_names(text: &str) -> Vec<StaleNameHit> {
    let mut hits = Vec::new();
    for &name in GRAPH_TOOL_NAMES {
        let old = format!("loop_{}", &name["graph_".len()..]);
        if contains_word(text, &old) {
            hits.push(StaleNameHit {
                old,
                new: name.to_string(),
            });
        }
    }
    if contains_word(text, "canopy loop") {
        hits.push(StaleNameHit {
            old: "canopy loop".to_string(),
            new: "canopy graph".to_string(),
        });
    }
    hits
}
