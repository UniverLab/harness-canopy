//! Bundling custom `prompt_preset` files into a graph export document, and
//! installing them back on import (v6): a shared recipe must carry the full
//! text of every custom preset its agent nodes reference, so installing it
//! on another machine produces working nodes instead of name-only
//! references that fail `reject_unknown_prompt_presets`.
//!
//! Safety rules, shared by every function here: nothing outside the prompts
//! dir is ever bundled or installed, and an existing preset file is never
//! overwritten — a user's edited preset always wins, by refusal rather than
//! by clobbering.

use std::collections::BTreeSet;

use super::*;

/// The full content of every custom `prompt_preset` referenced by `nodes`
/// (agent nodes only — an ensemble member carries no preset field), for
/// bundling into an export document. Builtins are skipped: they are seeded
/// on every daemon start and need no file. A referenced preset whose file
/// cannot be read (missing locally, e.g. the node names a preset that does
/// not exist) or is empty is skipped rather than failing the whole export —
/// export stays "the design as it is", and the import-side refusal is the
/// safety net for a reference that travelled without content. Skipping an
/// empty file also keeps the document importable: import refuses an empty
/// bundled preset, so export never emits one. Names are compared verbatim,
/// exactly as the spawn path joins them; a name that could point outside the
/// prompts dir (path separators, `.`, `..`) is skipped, never bundled. A
/// `BTreeMap` underneath: sorted by name and deduplicated when several nodes
/// share one preset.
pub fn collect(
    nodes: &[GraphExportNode],
    prompts_dir: &std::path::Path,
) -> Vec<GraphExportPromptPreset> {
    let builtins = builtin_preset_names();
    let mut by_name: BTreeMap<String, String> = BTreeMap::new();
    for node in nodes {
        if node.kind != GraphNodeKind::Agent {
            continue;
        }
        let Some(name) = node
            .config
            .get("prompt_preset")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
        else {
            continue;
        };
        if builtins.contains(&name) || !is_safe_name(name) {
            continue;
        }
        if by_name.contains_key(name) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(prompts_dir.join(format!("{name}.md"))) else {
            continue;
        };
        if content.is_empty() {
            continue;
        }
        by_name.insert(name.to_string(), content);
    }
    by_name
        .into_iter()
        .map(|(name, content)| GraphExportPromptPreset { name, content })
        .collect()
}

/// The machine-independent half of [`validate`]: every rule about the entry
/// itself — a plain non-blank name that stays inside the prompts dir, not a
/// builtin, not a repeat of an earlier entry, non-empty content. Import runs
/// it through [`validate`] (followed by the target-machine overwrite checks);
/// `graph` runs it alone, since a share-check has no installer's
/// prompts dir to read — so a document that validates on one machine is
/// installable on every machine. `seen` carries the names already accepted,
/// so a duplicate refuses with the same message in both paths.
pub fn validate_entry<'a>(
    preset: &'a GraphExportPromptPreset,
    seen: &mut BTreeSet<&'a str>,
) -> Result<(), String> {
    let name = preset.name.as_str();
    if name.trim() != name || name.is_empty() {
        return Err(format!(
            "Bundled prompt preset '{name}' must be a plain non-blank name (no surrounding whitespace)."
        ));
    }
    if !is_safe_name(name) {
        return Err(format!(
            "Bundled prompt preset '{name}' must be a plain file name: no path separators, not '.' or '..'."
        ));
    }
    let builtins = builtin_preset_names();
    if builtins.contains(&name) {
        return Err(format!(
            "Bundled prompt preset '{name}' is a builtin ({}); builtins are never bundled or installed from a document.",
            builtins.join(", ")
        ));
    }
    if !seen.insert(name) {
        return Err(format!(
            "Bundled prompt preset '{name}' appears more than once in the document."
        ));
    }
    if preset.content.is_empty() {
        return Err(format!("Bundled prompt preset '{name}' has empty content."));
    }
    Ok(())
}

/// Validate the bundled presets of a document about to be imported, before
/// any of them is installed: every entry must be installable into the
/// prompts dir (a safe, non-builtin, non-blank name and non-empty content)
/// and must not overwrite anything. A name that exists locally with
/// identical content is a no-op; one that exists with DIFFERENT content —
/// or whose file cannot be read — is a refusal naming the preset and its
/// path, never a silent overwrite of a user's edited preset file.
pub fn validate(
    presets: &[GraphExportPromptPreset],
    prompts_dir: &std::path::Path,
) -> Result<(), String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for preset in presets {
        validate_entry(preset, &mut seen)?;
        let name = preset.name.as_str();
        let path = prompts_dir.join(format!("{name}.md"));
        match std::fs::read_to_string(&path) {
            Ok(existing) if existing == preset.content => {}
            Ok(_) => {
                return Err(format!(
                    "Prompt preset '{name}' already exists at {} with different content; refusing to overwrite it.",
                    path.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Prompt preset '{name}' exists at {} but could not be read ({error}); refusing to overwrite it.",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

/// Install the bundled presets into the prompts dir, after the whole import
/// validated (all-or-nothing). Every entry is re-validated first, so this
/// function is safe to call on its own: an unsafe name, a builtin, or an
/// entry that would overwrite a differing local file is a refusal, never a
/// write outside the prompts dir or over a user's edited preset file. An
/// entry whose file already exists validated identical, so it is skipped;
/// a racing writer between the existence check and the create is also
/// skipped (`create_new` reports `AlreadyExists`).
pub fn install(
    presets: &[GraphExportPromptPreset],
    prompts_dir: &std::path::Path,
) -> Result<(), String> {
    if presets.is_empty() {
        return Ok(());
    }
    validate(presets, prompts_dir)?;
    std::fs::create_dir_all(prompts_dir).map_err(|error| {
        format!(
            "Could not create prompts dir {}: {error}.",
            prompts_dir.display()
        )
    })?;
    for preset in presets {
        let path = prompts_dir.join(format!("{}.md", preset.name));
        if path.exists() {
            continue;
        }
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match created {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(preset.content.as_bytes()).map_err(|error| {
                    format!(
                        "Could not write bundled prompt preset '{}' to {}: {error}.",
                        preset.name,
                        path.display()
                    )
                })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "Could not create bundled prompt preset '{}' at {}: {error}.",
                    preset.name,
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

/// Validate the bundled presets of a catalogue install, before anything is
/// written: the same entry rules as [`validate`], plus the ownership rule.
/// A name that exists locally with identical content is a no-op; one that
/// exists with DIFFERENT content is updated in place when the recorded
/// owner is the same source — a reinstall — and refused otherwise, naming
/// the recorded owner (or that the file is local) and the incoming source,
/// unless `overwrite` (`--overwrite-presets`/`--replace`) is set. A preset
/// name owned by another source is never overwritten silently.
pub fn validate_for_install(
    presets: &[GraphExportPromptPreset],
    prompts_dir: &std::path::Path,
    source_ref: &str,
    overwrite: bool,
) -> Result<(), String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for preset in presets {
        validate_entry(preset, &mut seen)?;
        let name = preset.name.as_str();
        let path = prompts_dir.join(format!("{name}.md"));
        match std::fs::read_to_string(&path) {
            Ok(existing) if existing == preset.content => {}
            Ok(_) => {
                if overwrite || owner_of(prompts_dir, name).as_deref() == Some(source_ref) {
                    continue;
                }
                return Err(owned_preset_refusal(&path, name, prompts_dir, source_ref));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Prompt preset '{name}' exists at {} but could not be read ({error}); refusing to overwrite it.",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

/// Install the bundled presets of a catalogue install, after the whole
/// install validated (all-or-nothing): like [`install`], but a differing
/// local file owned by the same source is updated in place, and every
/// preset actually written is stamped with its source in `.sources.json`.
/// Entry rules still apply; `overwrite`
/// (`--overwrite-presets`/`--replace`) replaces a differing file regardless
/// of owner. A file with identical content is skipped as a no-op and keeps
/// its recorded owner — a matching local file is never adopted.
pub fn install_for_install(
    presets: &[GraphExportPromptPreset],
    prompts_dir: &std::path::Path,
    source_ref: &str,
    overwrite: bool,
) -> Result<(), String> {
    if presets.is_empty() {
        return Ok(());
    }
    validate_for_install(presets, prompts_dir, source_ref, overwrite)?;
    std::fs::create_dir_all(prompts_dir).map_err(|error| {
        format!(
            "Could not create prompts dir {}: {error}.",
            prompts_dir.display()
        )
    })?;
    let mut written: Vec<GraphExportPromptPreset> = Vec::new();
    for preset in presets {
        let path = prompts_dir.join(format!("{}.md", preset.name));
        match std::fs::read_to_string(&path) {
            Ok(existing) if existing == preset.content => {}
            _ => {
                std::fs::write(&path, &preset.content).map_err(|error| {
                    format!(
                        "Could not write bundled prompt preset '{}' to {}: {error}.",
                        preset.name,
                        path.display()
                    )
                })?;
                written.push(preset.clone());
            }
        }
    }
    stamp_owners(prompts_dir, &written, source_ref)
}

/// The refusal for a bundled preset whose local file differs and belongs to
/// another source (or to no recorded source — a local file): it names the
/// preset, its path, both sides, and the way out. It keeps the `different
/// content` phrase the import-path refusal uses.
fn owned_preset_refusal(
    path: &std::path::Path,
    name: &str,
    prompts_dir: &std::path::Path,
    source_ref: &str,
) -> String {
    let existing = match owner_of(prompts_dir, name) {
        Some(owner) => format!("installed from '{owner}'"),
        None => "a local file, not installed from the catalogue".to_string(),
    };
    format!(
        "Prompt preset '{name}' already exists at {} with different content ({existing}); \
         '{source_ref}' also ships it. Install with --as to rename it, or pass --overwrite-presets to replace it.",
        path.display()
    )
}

/// The recorded owner of one preset: the source identity stamped when it
/// was last written by a catalogue install, or `None` for a local file.
fn owner_of(prompts_dir: &std::path::Path, name: &str) -> Option<String> {
    read_owners(prompts_dir).get(name).cloned()
}

/// The sidecar recording which catalogue source owns each installed preset:
/// `<prompts_dir>/.sources.json`, a `{name: source}` object. Presets are
/// read by direct `<name>.md` joins, never by globbing the dir, so the
/// sidecar disturbs no reader.
const SIDECAR_FILENAME: &str = ".sources.json";

/// Every recorded preset owner. A missing sidecar — or one that does not
/// parse as a `{name: source}` object — is an empty map: local files and
/// installs from before sources were recorded simply have no owner.
pub fn read_owners(prompts_dir: &std::path::Path) -> BTreeMap<String, String> {
    let Ok(raw) = std::fs::read_to_string(prompts_dir.join(SIDECAR_FILENAME)) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<BTreeMap<String, String>>(&raw).unwrap_or_default()
}

/// Record `source_ref` as the owner of every preset in `presets`, keeping
/// the owners of presets not listed. An empty list still succeeds (it
/// rewrites the sidecar unchanged, creating it when missing).
pub fn stamp_owners(
    prompts_dir: &std::path::Path,
    presets: &[GraphExportPromptPreset],
    source_ref: &str,
) -> Result<(), String> {
    let mut owners = read_owners(prompts_dir);
    for preset in presets {
        owners.insert(preset.name.clone(), source_ref.to_string());
    }
    let body = serde_json::to_string_pretty(&owners)
        .map_err(|error| format!("Could not serialize preset owners: {error}."))?;
    std::fs::write(prompts_dir.join(SIDECAR_FILENAME), format!("{body}\n")).map_err(|error| {
        format!(
            "Could not write {}: {error}.",
            prompts_dir.join(SIDECAR_FILENAME).display()
        )
    })?;
    Ok(())
}

/// The builtin preset names (`implementer`, `reviewer`, `resilience` —
/// seeded on daemon start, never bundled or installed from a document).
fn builtin_preset_names() -> Vec<&'static str> {
    crate::domain::prompts::builtin_prompt_preset_specs()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// A preset name that stays inside the prompts dir when joined as
/// `<dir>/<name>.md`: no path separators, not `.` or `..`. The join is
/// verbatim (as the spawn path joins it), so this is the one place that
/// decides a name is a plain file name.
fn is_safe_name(name: &str) -> bool {
    !name.contains('/') && !name.contains('\\') && name != "." && name != ".."
}
