//! FR7: no whole-word occurrence of a removed one-operation tool name may
//! survive anywhere in `src/` outside test code — every string the daemon
//! itself emits (prompts, hook text, error messages, docs comments an agent
//! may read) must name the new resource tool and action instead.
//!
//! The walk skips any path containing `test` and truncates each file at its
//! first `#[cfg(test)]`, so test modules (including this one) may still
//! reference the old names when asserting against them.

use std::fs;
use std::path::Path;

/// The tool names removed from the MCP surface when the graph family,
/// then the spec/queue/agent/harness/config/crew families, and finally the
/// node/intelligence/project/scheduled-send/skill/subagent/rag/identity
/// families moved onto the resource tools — plus the deleted
/// `loop_schedule_autorun` alias, the retired sync and blueprint tools, and
/// `spec_convert`.
/// (`spec_get` never existed as a tool, so there is no name to forbid.
/// `mutex` is covered by its own narrower test below: the bare word stays in
/// the engine, the CLI, and the node-config field.)
const REMOVED_NAMES: &[&str] = &[
    "graph_list",
    "graph_get",
    "graph_export",
    "graph_audit_node_configs",
    "graph_events",
    "graph_node_runs_list",
    "graph_node_run_get",
    "graph_create",
    "graph_update",
    "graph_archive",
    "graph_restore",
    "graph_import",
    "graph_validate",
    "graph_install",
    "graph_add_spec",
    "graph_remove_spec",
    "graph_update_spec",
    "graph_add_node",
    "graph_update_node",
    "graph_delete_node",
    "graph_copy_node",
    "graph_add_edge",
    "graph_update_edge",
    "graph_delete_edge",
    "graph_add_ensemble",
    "graph_update_ensemble",
    "graph_delete_ensemble",
    "graph_copy_ensemble",
    "graph_run",
    "graph_pause",
    "graph_continue",
    "graph_reset",
    "graph_preflight",
    "graph_schedule_continue",
    "graph_schedule_autorun",
    "loop_schedule_autorun",
    "spec_create",
    "spec_list",
    "spec_update",
    "spec_delete",
    "spec_set_status",
    "spec_section_get",
    "spec_notes_get",
    "spec_note_write",
    "queue_create",
    "queue_list",
    "queue_add_spec",
    "queue_remove_spec",
    "queue_reorder",
    "queue_set_group",
    "agent_add",
    "agent_watch",
    "agent_get",
    "agent_list",
    "agent_update",
    "agent_remove",
    "agent_enable",
    "agent_disable",
    "agent_schedule_enable",
    "agent_run",
    "agent_logs",
    "agent_status",
    "agent_models",
    "agent_probe",
    "agent_probe_recent",
    "registry_refresh",
    "roster_set",
    "roster_list",
    "roster_clear",
    "roster_check",
    "config_get",
    "config_list",
    "config_set",
    "config_reset",
    "crew_create",
    "crew_get",
    "crew_list",
    "crew_update",
    "crew_delete",
    "graph_crew",
    // Final wave: every name the eight last resource tools absorbed, plus
    // the retired sync, blueprint, and convert tools.
    "node_list",
    "node_install",
    "node_remove",
    "intelligence_delete_node",
    "intelligence_delete_relation",
    "intelligence_graph_walk",
    "intelligence_link_projects",
    "intelligence_list_projects",
    "project_register",
    "project_remap",
    "project_search",
    "project_update",
    // Retired names built with `concat!` so no retired literal survives
    // in the repo (a literal would fail the repo-wide retired-name grep).
    concat!("scheduled", "_send_list"),
    concat!("scheduled", "_send_cancel"),
    "skill_get",
    "skill_list",
    "subagent_collect",
    "subagent_spawn",
    "rag_compact",
    "rag_search",
    "get_identity",
    "evolve_identity",
    "list_seeds",
    "create_seed",
    "remove_seed",
    "sync_broadcast",
    "sync_declare_intent",
    "sync_get_context",
    "sync_report_status",
    "blueprint_list",
    "blueprint_create",
    "blueprint_delete",
    "spec_convert",
];

/// `graph_events` is also the name of the SQLite table the durable event log
/// lives in — a real schema entity the db code and its docs must be able to
/// name. The MCP tool of that name is gone from the surface; whole-word
/// `graph_events` identifiers in prose now always mean the table.
fn exempt_for(name: &str, _path: &str) -> bool {
    name == "graph_events"
}

fn is_test_path(path: &str) -> bool {
    path.contains("test")
}

/// Removed names that are also `Database` method (or `domain::seeds`
/// function) names, which keep their names: the read path the engine and
/// CLIs call directly. A method call (`.name(`), a `::`-qualified path, the
/// `fn name(` definition inside `src/db/`, or any use inside either
/// `seeds.rs` (the module that implements the seeds functions) is the
/// database, not the MCP surface — everything else (a `name = "…"` tool
/// attribute, a `call_tool("…")` caller, prompt or error prose) is still a
/// hit.
const DB_METHOD_NAMES: &[&str] = &[
    "spec_note_write",
    "spec_notes_get",
    "roster_list",
    "crew_create",
    "crew_get",
    "crew_list",
    "crew_update",
    "crew_delete",
    "graph_crew",
    "list_seeds",
    "remove_seed",
];

/// Removed names that survive as `harness` action names. The action table
/// (`name: "…"`), the dispatch match arm (`"…"=>`), a caller payload (the
/// line names `action`), new-surface prose (the line names `harness`), and
/// the dispatch file's own action descriptions are the new surface — a
/// `name = "…"` tool attribute or an old-style `call_tool("…")` caller is
/// still a hit.
const ROSTER_ACTION_NAMES: &[&str] = &["roster_set", "roster_list", "roster_clear", "roster_check"];

/// Removed names that survive as `identity` action names. Same shape as the
/// roster exemption: the action table (`name: "…"`), the dispatch match arm
/// (`"…"=>`), a caller payload (the line names `action`), new-surface prose
/// (the line names `identity`), and the dispatch file's own action
/// descriptions are the new surface — a `name = "…"` tool attribute or an
/// old-style `call_tool("…")` caller is still a hit.
const IDENTITY_ACTION_NAMES: &[&str] = &["list_seeds", "create_seed", "remove_seed"];

fn scan_file(path: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    // Inline `#[cfg(test)]` modules are test code; they typically open a
    // module that runs to end of file, so truncating at the marker is the
    // conservative cut.
    let text = text.split("#[cfg(test)]").next().unwrap_or(&text);
    let shown = path.to_string_lossy();
    let mut hits = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        for name in REMOVED_NAMES {
            if exempt_for(name, &shown) {
                continue;
            }
            // `graph_install`/`graph_validate` also name daemon modules the
            // CLI and node installer call directly; a `::`-qualified path
            // segment or a `mod` declaration is code, not a tool reference.
            // `graph_crew` also names the `db`/`params` modules (plus their
            // `use` re-export), and `node_install` the node-installer module;
            // same rationale.
            if (*name == "graph_install"
                || *name == "graph_validate"
                || *name == "graph_crew"
                || *name == "node_install")
                && (line.contains(&format!("::{name}"))
                    || line.contains(&format!("mod {name}"))
                    || line.contains(&format!("use {name}")))
            {
                continue;
            }
            if DB_METHOD_NAMES.contains(name)
                && (line.contains(&format!(".{name}("))
                    || line.contains(&format!("::{name}"))
                    || shown.ends_with("seeds.rs")
                    || (line.contains(&format!("fn {name}(")) && shown.contains("src/db/")))
            {
                continue;
            }
            if ROSTER_ACTION_NAMES.contains(name) {
                let words: Vec<&str> = line
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .collect();
                if words.contains(&"harness")
                    || words.contains(&"action")
                    || line.contains(&format!("name: \"{name}\""))
                    || line.contains(&format!("\"{name}\" =>"))
                    || shown.ends_with("src/daemon/handler_resource_tools.rs")
                {
                    continue;
                }
            }
            if IDENTITY_ACTION_NAMES.contains(name) {
                let words: Vec<&str> = line
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .collect();
                if words.contains(&"identity")
                    || words.contains(&"action")
                    || line.contains(&format!("name: \"{name}\""))
                    || line.contains(&format!("\"{name}\" =>"))
                    || shown.ends_with("src/daemon/handler_resource_tools_final.rs")
                {
                    continue;
                }
            }
            // The stale-name detector's own catalogue must keep the removed
            // alias as a detection token.
            if *name == "loop_schedule_autorun" && shown.ends_with("src/domain/stale_names.rs") {
                continue;
            }
            if line
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|word| word == *name)
            {
                hits.push(format!("{shown}:{}: contains `{name}`", idx + 1));
            }
        }
    }
    hits
}

fn walk(dir: &Path, hits: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let shown = path.to_string_lossy().to_string();
        if path.is_dir() {
            if !is_test_path(&shown) {
                walk(&path, hits);
            }
            continue;
        }
        if !shown.ends_with(".rs") || is_test_path(&shown) {
            continue;
        }
        hits.extend(scan_file(&path));
    }
}

#[test]
fn no_removed_tool_name_survives_in_src_outside_tests() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest_dir).join("src");
    let mut hits = Vec::new();
    walk(&src, &mut hits);
    assert!(
        hits.is_empty(),
        "removed tool names must be reworded to the new tool+action outside tests:\n{}",
        hits.join("\n")
    );
}

fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The `mutex` MCP tool is retired, but the bare word stays: the engine
/// (lease/waiter store), the `canopy mutex` CLI, and the node-config `mutex`
/// field all keep the name. So instead of a whole-word ban, forbid only the
/// MCP surface spellings — a `name = "mutex"` tool attribute or a
/// `call_tool("mutex", …)` caller — and assert the keepers still exist.
#[test]
fn no_mutex_mcp_tool_survives_while_engine_and_param_keep_the_word() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest_dir).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files);
    let mut hits = Vec::new();
    for path in &files {
        let shown = path.to_string_lossy().to_string();
        if is_test_path(&shown) {
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let text = text.split("#[cfg(test)]").next().unwrap_or(&text);
        for (idx, line) in text.lines().enumerate() {
            if line.contains("name = \"mutex\"")
                || (line.contains("call_tool(") && line.contains("\"mutex\""))
            {
                hits.push(format!("{shown}:{}: {line}", idx + 1));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "mutex MCP surface must be gone outside tests:\n{}",
        hits.join("\n")
    );
    let engine = fs::read_to_string(src.join("db/mutex_store.rs")).unwrap_or_default();
    assert!(
        engine.contains("mutex_leases"),
        "the mutex engine (mutex_leases store) must stay"
    );
    let params = fs::read_to_string(src.join("daemon/params/nodes.rs")).unwrap_or_default();
    assert!(
        params.contains("pub mutex:"),
        "the node-config `mutex` field must stay"
    );
    assert!(
        src.join("daemon/mutex_cli.rs").exists(),
        "the `canopy mutex` CLI must stay"
    );
}
