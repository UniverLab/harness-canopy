//! Stale 2.x reference detection — extracted from `validation.rs` to stay
//! below the rust-quality.sh file-size limit. Public API re-exported through
//! `crate::domain::validation` so callers are unaffected.

/// The 2.x `loop_<name>` MCP tool names removed in 3.0.0, each paired with
/// its current replacement. Kept as a literal list — not derived at runtime
/// from the tool router — so this pure `domain` module never depends on
/// `daemon`. CB68: used to warn about stale 2.x references in
/// preflight and `canopy doctor`; the replacements name the resource tools
/// (tool + action) that took over each operation.
pub const STALE_2X_REPLACEMENTS: &[(&str, &str)] = &[
    ("loop_create", "graph_edit create"),
    ("loop_update", "graph_edit update"),
    ("loop_add_spec", "graph_edit attach_spec"),
    ("loop_update_spec", "graph_edit update_spec"),
    ("loop_remove_spec", "graph_edit detach_spec"),
    ("loop_add_node", "graph_node add"),
    ("loop_update_node", "graph_node update"),
    ("loop_add_edge", "graph_edge add"),
    ("loop_update_edge", "graph_edge update"),
    ("loop_delete_edge", "graph_edge delete"),
    ("loop_delete_node", "graph_node delete"),
    ("loop_add_ensemble", "graph_ensemble add"),
    ("loop_copy_node", "graph_node copy"),
    ("loop_copy_ensemble", "graph_ensemble copy"),
    ("loop_update_ensemble", "graph_ensemble update"),
    ("loop_delete_ensemble", "graph_ensemble delete"),
    ("loop_get", "graph get"),
    ("loop_export", "graph export"),
    ("loop_import", "graph_edit import"),
    ("loop_audit_node_configs", "graph audit"),
    ("loop_list", "graph list"),
    ("loop_node_runs_list", "graph node_runs"),
    ("loop_node_run_get", "graph node_run"),
    ("loop_preflight", "graph_control preflight"),
    ("loop_run", "graph_control run"),
    ("loop_reset", "graph_control reset"),
    ("loop_schedule_autorun", "graph_control schedule_autorun"),
    ("loop_schedule_continue", "graph_control schedule_continue"),
    ("loop_pause", "graph_control pause"),
    ("loop_archive", "graph_edit archive"),
    ("loop_restore", "graph_edit restore"),
    ("loop_continue", "graph_control continue"),
    ("loop_complete_node", "graph_complete_node"),
    ("loop_report_blocker", "graph_report_blocker"),
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
/// from [`STALE_2X_REPLACEMENTS`]) or the literal `canopy loop` phrase are
/// reported — an unrelated `loop_`-prefixed word (e.g. a stale `loop_id`
/// mention in prose) is not a removed tool and is out of scope for this
/// detector (FR2 says "for any removed tool", not "any loop_ word").
pub fn find_stale_2x_names(text: &str) -> Vec<StaleNameHit> {
    let mut hits = Vec::new();
    for &(old, new) in STALE_2X_REPLACEMENTS {
        if contains_word(text, old) {
            hits.push(StaleNameHit {
                old: old.to_string(),
                new: new.to_string(),
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
