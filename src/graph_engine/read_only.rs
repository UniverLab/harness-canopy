//! Read-only enforcement for agent nodes (and ensemble members): capture a
//! tree fingerprint at the start and end of a run and fail with
//! `read_only_violation` when the workdir changed.
//!
//! The fingerprint hashes `git status --porcelain`, `git diff HEAD --binary`,
//! and the path plus content hash of every untracked non-ignored file
//! (`git ls-files -o --exclude-standard`). Harness-owned `project_paths` for
//! the running platform are excluded from all three inputs before hashing.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::domain::graphs::{EnsembleDetails, GraphNode, GraphNodeKind, GraphRunStatus};

use super::dirty_commit::ensemble_owning_node;
use super::NodeExecution;

/// Per-path state captured in a fingerprint snapshot, used to report which
/// paths changed between start and end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PathChange {
    /// Path is present in the end snapshot but not the start snapshot.
    Added,
    /// Path is present in both snapshots with different state.
    Modified,
    /// Path is present in the start snapshot but not the end snapshot.
    Deleted,
}

fn classify_new_path(status: &str) -> PathChange {
    if status == "??" {
        PathChange::Added
    } else if status.contains('D') && !status.contains('A') {
        PathChange::Deleted
    } else if status.starts_with('A') || status.ends_with('A') {
        PathChange::Added
    } else {
        PathChange::Modified
    }
}

/// A point-in-time fingerprint of a git workdir's relevant state.
#[derive(Clone, Debug)]
pub(crate) struct TreeFingerprint {
    pub(crate) hash: [u8; 32],
    /// Path -> (porcelain status, state hash). A path present here is
    /// "interesting" (tracked dirty or untracked, after exclusions).
    paths: BTreeMap<String, (String, [u8; 32])>,
}

impl TreeFingerprint {
    /// Paths that differ between `self` (start) and `end`, classified as
    /// added/modified/deleted from the perspective of the run.
    pub(crate) fn diff_paths(&self, end: &Self) -> Vec<(String, PathChange)> {
        let mut out = Vec::new();
        for (path, (status, hash)) in &end.paths {
            match self.paths.get(path) {
                None => out.push((path.clone(), classify_new_path(status))),
                Some((_, before)) if before != hash => {
                    out.push((path.clone(), PathChange::Modified));
                }
                Some(_) => {}
            }
        }
        for (path, (status, _)) in &self.paths {
            if !end.paths.contains_key(path) {
                let change = if status == "??" {
                    PathChange::Deleted
                } else {
                    // Dirty tracked path became clean/restored: the tree changed
                    // relative to start.
                    PathChange::Modified
                };
                out.push((path.clone(), change));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// Watch a read-only run: compare start/end fingerprints.
pub(crate) struct ReadOnlyWatch {
    pub(crate) before: TreeFingerprint,
}

impl ReadOnlyWatch {
    /// Begin watching for a read-only agent run. Returns `Ok(None)` when the
    /// node is not read-only, when the workdir is not a git repository, or
    /// when fingerprint capture cannot be performed (treated as unenforced).
    /// `platform` names the harness running this node (for `project_paths`
    /// exclusions); pass `None` when unknown.
    pub(crate) async fn begin(
        enforced: bool,
        platform: Option<&str>,
        workdir: &str,
    ) -> Result<Option<Self>> {
        if !enforced {
            return Ok(None);
        }
        if !is_git_workdir(workdir).await {
            return Ok(None);
        }
        let exclusions = project_path_exclusions(platform);
        let before = capture_fingerprint(workdir, &exclusions).await?;
        Ok(Some(Self { before }))
    }

    /// `Some(paths)` when the tree changed, listing up to 20 touched paths as
    /// `(path, change)`. `None` when unchanged.
    pub(crate) async fn violation(
        &self,
        platform: Option<&str>,
        workdir: &str,
    ) -> Result<Option<Vec<(String, PathChange)>>> {
        let exclusions = project_path_exclusions(platform);
        let after = capture_fingerprint(workdir, &exclusions).await?;
        if self.before.hash == after.hash {
            return Ok(None);
        }
        let mut paths = self.before.diff_paths(&after);
        if paths.len() > 20 {
            paths.truncate(20);
        }
        Ok(Some(paths))
    }
}

/// Whether this node is read-only: its own `config.read_only == true`, or —
/// for a node owned by an ensemble — that ensemble's own `read_only` flag.
/// Mirrors [`super::dirty_commit::node_has_commit_rights`].
pub(crate) fn node_has_read_only(node: &GraphNode, ensembles: &[EnsembleDetails]) -> bool {
    if node.config.get("read_only").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    match ensemble_owning_node(&node.id, ensembles) {
        Some(ensemble_id) => ensembles
            .iter()
            .any(|d| d.ensemble.id == ensemble_id && d.ensemble.read_only),
        None => false,
    }
}

/// Resolve project_paths exclusions for a platform from the registry's cached
/// view (`~/.canopy/registry-baseline.toml`). Offline on purpose: the engine
/// must never fetch the registry. Missing platform/entry => no exclusions.
fn project_path_exclusions(platform: Option<&str>) -> Vec<String> {
    let Some(name) = platform.map(str::trim).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    crate::domain::registry_baseline::RegistryBaseline::load(&crate::domain::prompts::canopy_dir())
        .and_then(|baseline| baseline.get_platform(name).map(|p| p.project_paths.clone()))
        .unwrap_or_default()
}

async fn is_git_workdir(workdir: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workdir)
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Capture the workdir fingerprint, excluding `project_paths`.
pub(crate) async fn capture_fingerprint(
    workdir: &str,
    exclusions: &[String],
) -> Result<TreeFingerprint> {
    let status = git_output(workdir, &["status", "--porcelain", "-z"]).await?;
    let diff = git_output(workdir, &["diff", "HEAD", "--binary"]).await?;
    let untracked_list =
        git_output(workdir, &["ls-files", "-o", "--exclude-standard", "-z"]).await?;

    let status_entries = parse_porcelain_z(&status, exclusions);
    let filtered_status_bytes = serialize_porcelain_entries(&status_entries);
    let filtered_diff_bytes = filter_diff_by_exclusions(&diff, exclusions);

    let mut untracked_paths: Vec<String> = split_nul(&untracked_list)
        .into_iter()
        .filter(|p| !is_excluded(p, exclusions))
        .collect();
    untracked_paths.sort();
    untracked_paths.dedup();

    let mut hasher = Sha256::new();
    hasher.update(b"status\0");
    hasher.update(&filtered_status_bytes);
    hasher.update(b"\0diff\0");
    hasher.update(&filtered_diff_bytes);
    hasher.update(b"\0untracked\0");

    let mut paths: BTreeMap<String, (String, [u8; 32])> = BTreeMap::new();

    // Tracked/dirty paths from status (all non-?? entries, and also ?? appear
    // in porcelain but we handle untracked content via untracked list for
    // hashing; include ?? in path map based on file content hash below).
    for entry in &status_entries {
        if entry.status == "??" {
            continue;
        }
        // Per-path state for tracked dirty: hash of porcelain status + path-specific diff.
        let path_diff = diff_for_path(&filtered_diff_bytes, &entry.path);
        let mut ph = Sha256::new();
        ph.update(entry.status.as_bytes());
        ph.update(b"\0");
        ph.update(entry.path.as_bytes());
        ph.update(b"\0");
        ph.update(&path_diff);
        paths.insert(
            entry.path.clone(),
            (entry.status.clone(), ph.finalize().into()),
        );
    }

    // Untracked files: hash path + streamed content.
    for rel in &untracked_paths {
        let file_hash = hash_file_streaming(&Path::new(workdir).join(rel)).await?;
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(file_hash);
        hasher.update(b"\0");

        let mut ph = Sha256::new();
        ph.update(b"??\0");
        ph.update(rel.as_bytes());
        ph.update(b"\0");
        ph.update(file_hash);
        paths.insert(rel.clone(), ("??".to_string(), ph.finalize().into()));
    }

    let hash: [u8; 32] = hasher.finalize().into();
    Ok(TreeFingerprint { hash, paths })
}

#[derive(Clone, Debug)]
struct PorcelainEntry {
    status: String,
    path: String,
}

async fn git_output(workdir: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .await
        .map_err(|e| anyhow!("failed to run git {:?}: {e}", args))?;
    if !output.status.success() {
        // For repos with no HEAD (initial commit), `git diff HEAD` fails; treat as empty diff.
        if args.first().copied() == Some("diff") && args.get(1).copied() == Some("HEAD") {
            return Ok(Vec::new());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git {:?} failed: {}", args, stderr.trim()));
    }
    Ok(output.stdout)
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).to_string())
        .collect()
}

fn parse_porcelain_z(bytes: &[u8], exclusions: &[String]) -> Vec<PorcelainEntry> {
    // `git status --porcelain -z` emits entries separated by NUL. For renames/
    // copies there are two paths; we include both as relevant.
    let parts = split_nul(bytes);
    let mut entries = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        let raw = &parts[i];
        if raw.len() < 3 {
            i += 1;
            continue;
        }
        let status = raw[..2].to_string();
        let path = raw[3..].to_string();
        let is_rename = status.starts_with('R') || status.starts_with('C');
        if !is_excluded(&path, exclusions) {
            entries.push(PorcelainEntry {
                status: status.clone(),
                path: path.clone(),
            });
        }
        i += 1;
        if is_rename && i < parts.len() {
            // original path follows for renames/copies
            let orig = parts[i].clone();
            if !is_excluded(&orig, exclusions) {
                // record original too so deletions/renames are visible
                entries.push(PorcelainEntry {
                    status: status.clone(),
                    path: orig,
                });
            }
            i += 1;
        }
    }
    // stable order for hashing/diffing
    entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.status.cmp(&b.status)));
    entries.dedup_by(|a, b| a.path == b.path && a.status == b.status);
    entries
}

fn serialize_porcelain_entries(entries: &[PorcelainEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(e.status.as_bytes());
        out.push(b' ');
        out.extend_from_slice(e.path.as_bytes());
        out.push(0);
    }
    out
}

/// Exclude project_paths from diff output by dropping sections whose paths
/// match. Best-effort textual filtering on `diff --git` headers.
fn filter_diff_by_exclusions(diff: &[u8], exclusions: &[String]) -> Vec<u8> {
    if exclusions.is_empty() {
        return diff.to_vec();
    }
    let text = String::from_utf8_lossy(diff);
    let mut out = String::with_capacity(text.len());
    let mut skip = false;
    for chunk in split_diff_sections(&text) {
        if let Some(path) = diff_section_path(chunk) {
            skip = is_excluded(path, exclusions);
        }
        if !skip {
            out.push_str(chunk);
        }
    }
    out.into_bytes()
}

fn split_diff_sections(text: &str) -> Vec<&str> {
    let mut sections = Vec::new();
    let mut start = 0;
    let marker = "\ndiff --git ";
    let mut search = 0;
    // handle first section possibly starting at 0
    while let Some(pos_rel) = text[search..].find(marker) {
        let pos = search + pos_rel + 1; // include leading? keep '\n' boundary handling simple
        if pos > start {
            sections.push(&text[start..pos]);
        }
        start = pos;
        search = pos + 1;
        if search >= text.len() {
            break;
        }
    }
    if start < text.len() {
        sections.push(&text[start..]);
    }
    if sections.is_empty() && !text.is_empty() {
        sections.push(text);
    }
    sections
}

fn diff_section_path(section: &str) -> Option<&str> {
    // "diff --git a/PATH b/PATH"
    let line = section.lines().next()?;
    let line = line.strip_prefix("diff --git ")?;
    // prefer b/ path
    if let Some(idx) = line.rfind(" b/") {
        return Some(&line[idx + 3..]);
    }
    if let Some(rest) = line.strip_prefix("a/") {
        if let Some(idx) = rest.find(" b/") {
            return Some(&rest[..idx]);
        }
    }
    None
}

fn diff_for_path(filtered_diff: &[u8], path: &str) -> Vec<u8> {
    if filtered_diff.is_empty() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(filtered_diff);
    let mut out = String::new();
    for chunk in split_diff_sections(&text) {
        if diff_section_path(chunk) == Some(path) {
            out.push_str(chunk);
        }
    }
    out.into_bytes()
}

pub(crate) fn is_excluded(path: &str, exclusions: &[String]) -> bool {
    let normalized = path.trim_start_matches("./");
    exclusions.iter().any(|ex| {
        if ex.ends_with('/') {
            let dir = ex.trim_end_matches('/');
            normalized == dir
                || normalized.starts_with(ex)
                || normalized.starts_with(&format!("{dir}/"))
        } else {
            normalized == ex
        }
    })
}

async fn hash_file_streaming(path: &Path) -> Result<[u8; 32]> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| anyhow!("failed to open untracked file {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| anyhow!("failed to read untracked file {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

/// Build the `read_only_violation` failure result, mirroring
/// [`super::dirty_commit::commit_rights_failure`].
pub(crate) fn read_only_failure(
    label: &str,
    node_id: &str,
    touched: &[(String, PathChange)],
    node_output: Value,
) -> NodeExecution {
    let mut listed: Vec<String> = touched
        .iter()
        .map(|(path, change)| {
            let kind = match change {
                PathChange::Added => "added",
                PathChange::Modified => "modified",
                PathChange::Deleted => "deleted",
            };
            format!("{kind}: {path}")
        })
        .collect();
    if listed.len() > 20 {
        listed.truncate(20);
    }
    let mut message = format!(
        "{label}: read-only node changed the working tree while it ran. \
         A node configured with `read_only: true` must not modify tracked or untracked files. \
         The changes were left in place."
    );
    if !listed.is_empty() {
        message.push_str("\nTouched paths:");
        for line in &listed {
            message.push_str(&format!("\n  {line}"));
        }
    }
    let mut output = serde_json::Map::new();
    output.insert(
        "read_only_violation".to_string(),
        serde_json::json!({
            "node": label,
            "node_id": node_id,
            "paths": listed,
            "message": message,
        }),
    );
    output.insert("node_output".to_string(), node_output);
    NodeExecution {
        output: Value::Object(output),
        summary: message,
        status: GraphRunStatus::Fail,
    }
}

/// Output for an unenforced read-only run in a non-git workdir.
pub(crate) fn read_only_not_enforced_message() -> &'static str {
    "read_only not enforced: not a git workdir"
}

/// Whether `begin` would have no enforcement for a git-missing workdir while
/// the node is read-only.
pub(crate) async fn is_non_git_workdir(workdir: &str) -> bool {
    !is_git_workdir(workdir).await
}

pub(crate) fn node_platform_name(node: &GraphNode) -> Option<String> {
    node.config
        .get("platform")
        .or_else(|| node.config.get("cli"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Whether a member run is enforced read-only: the ensemble's own flag, or the
/// member node's own config.
pub(crate) fn member_is_read_only(details: &EnsembleDetails, node: &GraphNode) -> bool {
    details.ensemble.read_only
        || node.config.get("read_only").and_then(Value::as_bool) == Some(true)
}

/// Begin read-only enforcement for one plain agent node run. Only agent nodes
/// can be read-only; a node in a non-git workdir runs unenforced.
pub(crate) async fn begin_for_node(
    node: &GraphNode,
    ensembles: &[EnsembleDetails],
    workdir: &str,
) -> Result<(Option<ReadOnlyWatch>, bool)> {
    let enforced = node.kind == GraphNodeKind::Agent && node_has_read_only(node, ensembles);
    if !enforced {
        return Ok((None, false));
    }
    begin_for_run(true, node, workdir).await
}

/// Begin read-only enforcement for a single run (node or ensemble member).
/// Returns `(watch, unenforced_non_git)`.
pub(crate) async fn begin_for_run(
    enforced: bool,
    node: &GraphNode,
    workdir: &str,
) -> Result<(Option<ReadOnlyWatch>, bool)> {
    if !enforced {
        return Ok((None, false));
    }
    if is_non_git_workdir(workdir).await {
        return Ok((None, true));
    }
    let platform = node_platform_name(node);
    let watch = ReadOnlyWatch::begin(true, platform.as_deref(), workdir).await?;
    Ok((watch, false))
}

/// Apply read-only enforcement to a finished execution: on violation returns a
/// `read_only_violation` failure, on an unenforced non-git run annotates the
/// output, otherwise returns the execution unchanged.
pub(crate) async fn apply_to_execution(
    node: &GraphNode,
    label: &str,
    workdir: &str,
    watch: Option<&ReadOnlyWatch>,
    unenforced_non_git: bool,
    mut execution: NodeExecution,
) -> Result<NodeExecution> {
    if let Some(watch) = watch {
        let platform = node_platform_name(node);
        if let Some(touched) = watch.violation(platform.as_deref(), workdir).await? {
            tracing::warn!(
                node = %node.name,
                touched = ?touched.iter().map(|(p, _)| p).collect::<Vec<_>>(),
                "read-only node changed the working tree"
            );
            return Ok(read_only_failure(
                label,
                &node.id,
                &touched,
                execution.output,
            ));
        }
    }
    if unenforced_non_git {
        execution.output = attach_unenforced_note(execution.output);
    }
    Ok(execution)
}

/// Attach the unenforced non-git message to an execution output object.
pub(crate) fn attach_unenforced_note(mut output: Value) -> Value {
    let note = read_only_not_enforced_message().to_string();
    if let Value::Object(map) = &mut output {
        map.insert("read_only".to_string(), Value::String(note));
    } else {
        output = serde_json::json!({
            "node_output": output,
            "read_only": note,
        });
    }
    output
}
