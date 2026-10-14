//! CM51: the one writer of `config.toml`.
//!
//! Every write goes through `toml_edit`, which changes exactly the touched
//! item and leaves comments, key order and unknown keys untouched. The file
//! is replaced atomically. A file we cannot parse is never overwritten.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::domain::cli_config::CliConfig;

const CONFIG_FILE: &str = "config.toml";
const TEMP_FILE: &str = "config.toml.tmp";

/// Read the document, treating a missing file as empty. A file that does not
/// parse is an error: callers must not overwrite something they cannot read.
pub(crate) fn load_document(canopy_dir: &Path) -> Result<DocumentMut> {
    let path = canopy_dir.join(CONFIG_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(e.into()),
    }
}

/// The value at a dotted key, if present. Intermediate tables must exist.
pub(crate) fn read_dotted(doc: &DocumentMut, key: &str) -> Option<Value> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = doc.as_item();
    for (index, part) in parts.iter().enumerate() {
        let item = current.get(part)?;
        if index + 1 == parts.len() {
            return item.as_value().cloned();
        }
        current = item;
    }
    None
}

/// Set (`Some`) or remove (`None`) a dotted key, creating `[table]` headers
/// on the way down.
pub(crate) fn set_dotted(doc: &mut DocumentMut, key: &str, value: Option<Value>) {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current: &mut Table = doc.as_table_mut();
    for part in &parts[..parts.len() - 1] {
        if !current.contains_key(part) {
            current.insert(part, Item::Table(Table::new()));
        }
        let item = current
            .get_mut(part)
            .expect("just inserted or already present");
        if item.as_table().is_none() {
            *item = Item::Table(Table::new());
        }
        current = item.as_table_mut().expect("a table now");
    }
    let last = parts[parts.len() - 1];
    match value {
        Some(value) => set_item(current, last, value),
        None => {
            current.remove(last);
        }
    }
}

/// Write one leaf, keeping the trivia (the comment above it, the spacing)
/// the existing value carried so the edited line is the only one that moves.
fn set_item(table: &mut Table, key: &str, value: Value) {
    let Some(existing) = table.get_mut(key).and_then(Item::as_value_mut) else {
        table.insert(key, Item::Value(value));
        return;
    };
    let decor = existing.decor().clone();
    *existing = value;
    *existing.decor_mut() = decor;
}

/// Apply `edits` to `config.toml` and write it back atomically.
pub fn write_keys(canopy_dir: &Path, edits: &[(&str, Option<Value>)]) -> Result<()> {
    let mut doc = load_document(canopy_dir)?;
    for (key, value) in edits {
        set_dotted(&mut doc, key, value.clone());
    }
    write_document(canopy_dir, &doc)
}

/// Replace only the `clis` item, keeping every comment around it.
pub fn write_clis(canopy_dir: &Path, clis: &[CliConfig]) -> Result<()> {
    #[derive(Serialize)]
    struct Clis<'a> {
        clis: &'a [CliConfig],
    }

    let mut doc = load_document(canopy_dir)?;
    let text = toml::to_string(&Clis { clis })?;
    let rendered: DocumentMut = text.parse()?;
    match rendered.get("clis").cloned() {
        Some(item) => {
            doc.as_table_mut().insert("clis", item);
        }
        None => {
            doc.as_table_mut().remove("clis");
        }
    }
    write_document(canopy_dir, &doc)
}

fn write_document(canopy_dir: &Path, doc: &DocumentMut) -> Result<()> {
    std::fs::create_dir_all(canopy_dir)?;
    let temp = canopy_dir.join(TEMP_FILE);
    std::fs::write(&temp, doc.to_string())?;
    std::fs::rename(&temp, canopy_dir.join(CONFIG_FILE))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn written(dir: &TempDir) -> String {
        std::fs::read_to_string(dir.path().join(CONFIG_FILE)).unwrap()
    }

    #[test]
    fn config_set_round_trip_preserves_comments_and_unknown_keys() {
        let dir = TempDir::new().unwrap();
        let original = "# top comment\n\
                        zzz = 1\n\
                        projects_root = \"/x\"\n\
                        # the theme\n\
                        theme = \"classic\"\n\
                        [clean]\n\
                        retention_days = 7 # keep\n";
        std::fs::write(dir.path().join(CONFIG_FILE), original).unwrap();

        let value: Value = "\"modern\"".parse().unwrap();
        write_keys(dir.path(), &[("theme", Some(value))]).unwrap();

        let after = written(&dir);
        let before_lines: Vec<&str> = original.lines().collect();
        let after_lines: Vec<&str> = after.lines().collect();
        assert_eq!(before_lines.len(), after_lines.len(), "{after}");
        for (before, now) in before_lines.iter().zip(&after_lines) {
            if before.starts_with("theme") {
                assert_eq!(*now, "theme = \"modern\"");
                continue;
            }
            assert_eq!(before, now, "line changed unexpectedly:\n{after}");
        }
        assert!(after.contains("zzz = 1"));
        assert!(after.contains("projects_root = \"/x\""));
        assert!(after.contains("# the theme"));
        assert!(after.contains("# keep"));
    }

    #[test]
    fn write_keys_creates_missing_tables_and_removes_on_none() {
        let dir = TempDir::new().unwrap();
        let value: Value = "30".parse().unwrap();
        write_keys(dir.path(), &[("scheduler.retry_max", Some(value))]).unwrap();
        assert!(written(&dir).contains("[scheduler]"));
        assert!(written(&dir).contains("retry_max = 30"));

        write_keys(dir.path(), &[("scheduler.retry_max", None)]).unwrap();
        assert!(!written(&dir).contains("retry_max"));
    }

    #[test]
    fn write_clis_replaces_the_array_of_tables_and_keeps_comments() {
        let dir = TempDir::new().unwrap();
        // The comment is outside the `[[clis]]` tables (it belongs to the
        // `theme` line above them), which is what the writer must preserve.
        let original = "# keep me\ntheme = \"classic\"\n\n[[clis]]\nname = \"opencode\"\n\n[[clis]]\nname = \"kiro\"\n";
        std::fs::write(dir.path().join(CONFIG_FILE), original).unwrap();

        write_clis(dir.path(), &[]).unwrap();
        let after = written(&dir);
        assert!(!after.contains("[[clis]]"), "{after}");
        assert!(!after.contains("opencode"), "{after}");
        assert!(after.contains("# keep me"), "{after}");
        assert!(after.contains("theme = \"classic\""), "{after}");

        let one = CliConfig {
            name: "kiro".to_string(),
            binary: "kiro".to_string(),
            ..Default::default()
        };
        write_clis(dir.path(), &[one]).unwrap();
        let after = written(&dir);
        assert_eq!(after.matches("[[clis]]").count(), 1, "{after}");
        assert!(after.contains("name = \"kiro\""), "{after}");
        assert!(!after.contains("opencode"), "{after}");
        assert!(after.contains("# keep me"), "{after}");
    }

    #[test]
    fn a_file_that_does_not_parse_is_never_overwritten() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), "this is not = = toml").unwrap();
        let value: Value = "\"modern\"".parse().unwrap();
        assert!(write_keys(dir.path(), &[("theme", Some(value))]).is_err());
        assert_eq!(written(&dir), "this is not = = toml");
    }
}
