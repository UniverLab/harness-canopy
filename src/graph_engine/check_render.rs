//! Check command rendering and environment variable setup.

use super::*;
use crate::domain::graphs::{GraphNode, GraphNodeKind, GraphSpec};
use crate::domain::spec_type::SpecType;
use anyhow::Result;

/// The `{{...}}` markers a check command expands through POSIX shell
/// quoting (CM38 FR1) — their values (graph/spec names, a directory) may
/// contain spaces or quotes, so they must never be spliced in raw. Shared
/// with `graph_preflight` via [`CHECK_COMMAND_MARKERS`]; the split matters
/// because the pre-quoted-marker stripping below applies only to these.
pub(crate) const CHECK_COMMAND_QUOTED_MARKERS: &[&str] = &[
    "graph_name",
    "workdir",
    "spec_id",
    "spec_name",
    // SPECCODE: the change-type fields + the prebuilt commit message. All
    // quoted: names and scopes may carry spaces/quotes; the empty-scope
    // case renders empty just like CANOPY_SPEC_* env vars do.
    "spec_code",
    "spec_type",
    "spec_scope",
    "spec_commit_message",
];

/// The `{{...}}` markers a check command expands RAW (CM38 FR2) — a git SHA
/// or a dirty-count integer, exactly as before CM38. Stripping a
/// `'{{marker}}'` wrapper here would change `test -n '{{spec_start_head}}'`
/// from false to true on a non-git workdir, so they are never normalized.
pub const CHECK_COMMAND_RAW_MARKERS: &[&str] =
    &["spec_start_head", "spec_start_dirty", "spec_committed_head"];

/// Every marker a check command can bind (CM38) — the one vocabulary both
/// [`render_check_command`] and `graph_preflight` enforce, so the engine and
/// the audit can never drift. Built from the quoted and raw vocabularies so
/// the three can never drift from each other either.
pub(crate) const CHECK_COMMAND_MARKERS: &[&str] = &[
    CHECK_COMMAND_QUOTED_MARKERS[0],
    CHECK_COMMAND_QUOTED_MARKERS[1],
    CHECK_COMMAND_QUOTED_MARKERS[2],
    CHECK_COMMAND_QUOTED_MARKERS[3],
    CHECK_COMMAND_QUOTED_MARKERS[4],
    CHECK_COMMAND_QUOTED_MARKERS[5],
    CHECK_COMMAND_QUOTED_MARKERS[6],
    CHECK_COMMAND_QUOTED_MARKERS[7],
    CHECK_COMMAND_RAW_MARKERS[0],
    CHECK_COMMAND_RAW_MARKERS[1],
    CHECK_COMMAND_RAW_MARKERS[2],
];

/// Everything a check command's markers and environment are rendered from.
/// `workdir` is the directory the command actually runs in (the graph's
/// workdir, or the run's sandbox worktree / `workdir_override` when one
/// applies) — never `lp.workdir` read at a distance.
pub(crate) struct CheckCommandCtx<'a> {
    pub(crate) graph_id: &'a str,
    pub(crate) graph_name: &'a str,
    pub(crate) workdir: &'a str,
    pub(crate) spec_id: &'a str,
    pub(crate) spec_name: &'a str,
    /// SPECCODE: the spec's change type (`feat`, `fix`, ...) — `None` on a
    /// legacy/untyped spec, where the three SPECCODE markers/env vars are
    /// empty just like the legacy ones they stand beside.
    pub(crate) spec_type: Option<SpecType>,
    pub(crate) spec_scope: Option<&'a str>,
    /// SPECCODE: engine-generated quick reference (`FIX-7`) or `None`.
    pub(crate) spec_code: Option<&'a str>,
    /// SPECCODE: prebuilt `<type>(<scope>): <name>` (see
    /// `domain::spec_type::commit_message`), precomputed by the caller from
    /// the spec's own fields.
    pub(crate) spec_commit_message: Option<&'a str>,
    pub(crate) spec_start_head: Option<&'a str>,
    pub(crate) spec_start_dirty: Option<i64>,
    pub(crate) spec_committed_head: Option<&'a str>,
}

/// Render a check node's command (CM38). Same contract as the CB48 hook
/// command renderer: refuse any `{{...}}` marker the vocabulary cannot bind
/// (first unknown marker wins, message is exactly
/// `unknown marker {{x}} in check command`), strip a single-quote wrapper a
/// template may already put around one of the QUOTED markers
/// (`'{{spec_name}}'` → `{{spec_name}}`) so the shell quoting is never
/// nested, then expand. Quoted markers go through `shell_quote`; the
/// head/dirty markers expand raw. A `\{{x}}` escape stays literal text.
pub(crate) fn render_check_command(template: &str, ctx: &CheckCommandCtx<'_>) -> Result<String> {
    if let Some(name) =
        crate::domain::prompts::unbindable_placeholders(template, CHECK_COMMAND_MARKERS).first()
    {
        bail!("unknown marker {{{{{name}}}}} in check command");
    }
    let normalized =
        CHECK_COMMAND_QUOTED_MARKERS
            .iter()
            .fold(template.to_string(), |template, marker| {
                template.replace(&format!("'{{{{{marker}}}}}'"), &format!("{{{{{marker}}}}}"))
            });
    Ok(crate::domain::prompts::render_template(
        &normalized,
        |raw| match raw {
            "graph_name" => Some(shell_quote(ctx.graph_name)),
            "workdir" => Some(shell_quote(ctx.workdir)),
            "spec_id" => Some(shell_quote(ctx.spec_id)),
            "spec_name" => Some(shell_quote(ctx.spec_name)),
            "spec_code" => Some(shell_quote(ctx.spec_code.unwrap_or(""))),
            "spec_type" => Some(shell_quote(ctx.spec_type.map(|t| t.as_str()).unwrap_or(""))),
            "spec_scope" => Some(shell_quote(
                ctx.spec_scope.filter(|s| !s.is_empty()).unwrap_or(""),
            )),
            "spec_commit_message" => Some(shell_quote(ctx.spec_commit_message.unwrap_or(""))),
            "spec_start_head" => Some(ctx.spec_start_head.unwrap_or("").to_string()),
            "spec_start_dirty" => Some(
                ctx.spec_start_dirty
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            "spec_committed_head" => Some(ctx.spec_committed_head.unwrap_or("").to_string()),
            _ => None,
        },
    ))
}

/// The `unknown marker {{x}} in check command` error for one check node, or
/// `None` when every marker binds. Scans a whole `GraphDetails` — the
/// graph's own nodes and every spec's nodes — so `graph_preflight` can
/// report the defect before spending any probe quota (CM38 FR4).
pub fn check_command_marker_error(details: &crate::domain::graphs::GraphDetails) -> Option<String> {
    let all_nodes = details
        .graph_nodes
        .iter()
        .chain(details.specs.iter().flat_map(|s| s.nodes.iter()));
    for node in all_nodes {
        if node.kind != GraphNodeKind::Check {
            continue;
        }
        let Some(command) = node.config.get("command").and_then(Value::as_str) else {
            continue;
        };
        if let Some(name) =
            crate::domain::prompts::unbindable_placeholders(command, CHECK_COMMAND_MARKERS).first()
        {
            return Some(format!(
                "unknown marker {{{{{name}}}}} in check command (check node '{}').",
                node.name
            ));
        }
    }
    None
}

/// The environment every check-node command runs with (CM38 FR6). The
/// command's working directory already IS the workdir, so there is no
/// `CANOPY_WORKDIR`.
pub(crate) fn check_command_env_vars(
    ctx: &CheckCommandCtx<'_>,
    spec_file: &str,
    spec_notes_file: &str,
) -> Vec<(String, String)> {
    [
        ("CANOPY_SPEC_ID", ctx.spec_id),
        ("CANOPY_SPEC_NAME", ctx.spec_name),
        ("CANOPY_GRAPH_ID", ctx.graph_id),
        ("CANOPY_GRAPH_NAME", ctx.graph_name),
        // SPECCODE: empty when the spec has no type — same convention as
        // CANOPY_SPEC_START_HEAD on a non-git workdir. The commit message
        // is prebuilt by the caller so check nodes can use it verbatim.
        ("CANOPY_SPEC_CODE", ctx.spec_code.unwrap_or("")),
        (
            "CANOPY_SPEC_TYPE",
            ctx.spec_type.map(|t| t.as_str()).unwrap_or(""),
        ),
        (
            "CANOPY_SPEC_SCOPE",
            ctx.spec_scope.filter(|s| !s.is_empty()).unwrap_or(""),
        ),
        (
            "CANOPY_SPEC_COMMIT_MESSAGE",
            ctx.spec_commit_message.unwrap_or(""),
        ),
        ("CANOPY_SPEC_START_HEAD", ctx.spec_start_head.unwrap_or("")),
        ("CANOPY_SPEC_FILE", spec_file),
        ("CANOPY_SPEC_NOTES_FILE", spec_notes_file),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect()
}

/// Write the spec text (CM38 FR6) to `spec.md` inside a fresh scratch dir
/// for this node execution, and return the dir handle with the file's
/// absolute path. The caller holds the `TempDir` until the node finishes,
/// which removes the file afterwards on every path. The content is the
/// spec's full `description` — which on an idea run IS the idea text; a
/// spec with no description gets an empty file so the variable is always
/// set.
pub(crate) fn write_check_spec_file(
    node: &GraphNode,
    spec: &GraphSpec,
) -> Result<(tempfile::TempDir, String)> {
    let scratch = tempfile::tempdir().with_context(|| {
        format!(
            "Failed to create scratch dir for check node '{}'.",
            node.name
        )
    })?;
    let path = scratch.path().join("spec.md");
    std::fs::write(&path, spec.description.as_deref().unwrap_or("")).with_context(|| {
        format!(
            "Failed to write the spec file for check node '{}'.",
            node.name
        )
    })?;
    Ok((scratch, path.display().to_string()))
}

/// Write the spec's notes (CM47) to `spec-notes.md` inside a fresh scratch
/// dir for this node execution, and return the dir handle with the file's
/// absolute path. One `## <owner_name>` section per note, newest first. An
/// empty file when there are no notes, so `CANOPY_SPEC_NOTES_FILE` is always
/// set and a `grep -q '^## Designer'` check correctly fails when there is no
/// Designer note. Mirrors [`write_check_spec_file`]'s lifetime contract.
pub(crate) fn write_check_notes_file(
    db: &Database,
    node: &GraphNode,
    spec: &GraphSpec,
) -> Result<(tempfile::TempDir, String)> {
    let scratch = tempfile::tempdir().with_context(|| {
        format!(
            "Failed to create scratch dir for check node '{}'.",
            node.name
        )
    })?;
    let path = scratch.path().join("spec-notes.md");
    let notes = db.spec_notes_get(&spec.id).unwrap_or_default();
    let mut out = String::new();
    for note in &notes {
        out.push_str(&format!("## {}\n{}\n\n", note.owner_name, note.body));
    }
    std::fs::write(&path, out).with_context(|| {
        format!(
            "Failed to write the notes file for check node '{}'.",
            node.name
        )
    })?;
    Ok((scratch, path.display().to_string()))
}
