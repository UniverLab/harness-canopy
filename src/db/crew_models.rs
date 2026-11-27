//! Named model entries that crew members track (`crew_models`).
//!
//! A model is named once — `agy-main` is `antigravity` plus
//! `gemini-4-hydrogen-medium` — and crew members reference it, so changing
//! one model is one edit. Members keep their materialized `platform`/`model`
//! columns equal to the entry (written at create/update time and rewritten
//! by [`Database::crew_model_apply`]), so the engine and every reader stay
//! unchanged; `model_ref` only records *which* entry a member tracks.

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::db::Database;
use crate::domain::crews::{
    validate_crew_members, validate_crew_name, Crew, CrewMember, UnresolvedCrewMember,
};

/// One named model entry: the `platform` plus optional `model` that every
/// crew member with this `model_ref` materializes into its own columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewModel {
    pub name: String,
    pub platform: String,
    pub model: Option<String>,
}

/// Outcome of [`Database::crew_model_apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrewModelApplyOutcome {
    pub model: CrewModel,
    /// Whether the stored pair changed (a no-op reports `crews_updated`
    /// empty and writes nothing).
    pub changed: bool,
    /// Names of the crews whose members were rewritten, one entry per crew
    /// however many members it holds.
    pub crews_updated: Vec<String>,
}

fn read_crew_model_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CrewModel> {
    Ok(CrewModel {
        name: row.get(0)?,
        platform: row.get(1)?,
        model: row.get(2)?,
    })
}

/// Validate an entry name: trimmed, 1 to 64 characters, `[A-Za-z0-9._-]`.
pub fn validate_model_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("model name must not be empty".to_string());
    }
    if trimmed.len() > 64 {
        return Err(format!(
            "model name must be at most 64 characters, got {}",
            trimmed.len()
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
    {
        return Err("model name may only contain letters, digits, '_', '.' and '-'".to_string());
    }
    Ok(())
}

fn resolve_in_tx(
    tx: &rusqlite::Transaction<'_>,
    members: &[UnresolvedCrewMember],
) -> Result<Vec<CrewMember>> {
    members
        .iter()
        .enumerate()
        .map(|(index, member)| {
            member
                .resolve(index, &|name| {
                    tx.query_row(
                        "SELECT platform, model FROM crew_models WHERE name = ?1",
                        params![name],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(|e| e.to_string())
                })
                .map_err(|e| anyhow!(e))
        })
        .collect()
}

impl Database {
    /// Create or replace the entry `name` with `platform`/`model`, then —
    /// in the same transaction — rewrite `platform`/`model` on every
    /// `crew_members` row whose `model_ref` equals the name and bump each
    /// distinct touched crew's `version` by exactly one. When the entry
    /// already holds the pair, nothing is written.
    pub fn crew_model_apply(
        &self,
        name: &str,
        platform: &str,
        model: Option<&str>,
    ) -> Result<CrewModelApplyOutcome> {
        validate_model_name(name).map_err(|e| anyhow!(e))?;
        let name = name.trim();
        let platform = platform.trim();
        if platform.is_empty() {
            anyhow::bail!("model '{name}': platform must not be empty");
        }
        let model = model
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string);
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let existing: Option<CrewModel> = tx
            .query_row(
                "SELECT name, platform, model FROM crew_models WHERE name = ?1",
                params![name],
                read_crew_model_row,
            )
            .optional()?;
        let unchanged = existing
            .as_ref()
            .is_some_and(|entry| entry.platform == platform && entry.model == model);
        if unchanged {
            return Ok(CrewModelApplyOutcome {
                model: existing.expect("checked Some above"),
                changed: false,
                crews_updated: Vec::new(),
            });
        }
        tx.execute(
            "INSERT INTO crew_models (name, platform, model, updated_at)
             VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(name) DO UPDATE SET platform = excluded.platform,
                model = excluded.model,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            params![name, platform, &model],
        )?;
        let touched: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT c.id, c.name FROM crews c
                 JOIN crew_members m ON m.crew_id = c.id
                 WHERE m.model_ref = ?1 ORDER BY c.name ASC",
            )?;
            let rows = stmt.query_map(params![name], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !touched.is_empty() {
            tx.execute(
                "UPDATE crew_members SET platform = ?1, model = ?2 WHERE model_ref = ?3",
                params![platform, &model, name],
            )?;
            let ids: Vec<&str> = touched.iter().map(|(id, _)| id.as_str()).collect();
            let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            tx.execute(
                &format!(
                    "UPDATE crews SET version = version + 1,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     WHERE id IN ({placeholders})"
                ),
                rusqlite::params_from_iter(ids),
            )?;
        }
        tx.commit()?;
        Ok(CrewModelApplyOutcome {
            model: CrewModel {
                name: name.to_string(),
                platform: platform.to_string(),
                model,
            },
            changed: true,
            crews_updated: touched.into_iter().map(|(_, name)| name).collect(),
        })
    }

    /// Every entry with the names of the crews referencing it.
    pub fn crew_model_list(&self) -> Result<Vec<(CrewModel, Vec<String>)>> {
        let models: Vec<CrewModel> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt =
                conn.prepare("SELECT name, platform, model FROM crew_models ORDER BY name ASC")?;
            let rows = stmt.query_map([], read_crew_model_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        models
            .into_iter()
            .map(|model| {
                let crews = self.crew_model_referencing(&model.name)?;
                Ok((model, crews))
            })
            .collect()
    }

    /// Names of the crews with at least one member tracking `name`.
    pub fn crew_model_referencing(&self, name: &str) -> Result<Vec<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT c.name FROM crews c
             JOIN crew_members m ON m.crew_id = c.id
             WHERE m.model_ref = ?1 ORDER BY c.name ASC",
        )?;
        let rows = stmt.query_map(params![name.trim()], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Delete an entry. Refused while any member references it, naming every
    /// crew — so a `crew_members.model_ref` can never dangle.
    pub fn crew_model_delete(&self, name: &str) -> Result<()> {
        let name = name.trim();
        let referencing = self.crew_model_referencing(name)?;
        if !referencing.is_empty() {
            anyhow::bail!(
                "model '{name}' is still referenced by crews: {}",
                referencing.join(", ")
            );
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute("DELETE FROM crew_models WHERE name = ?1", params![name])?;
        if rows == 0 {
            anyhow::bail!("model '{name}' not found.");
        }
        Ok(())
    }

    /// Create a crew whose members may track named entries. Resolution
    /// happens inside the member-write transaction.
    pub fn crew_create_resolved(
        &self,
        name: &str,
        description: Option<&str>,
        members: &[UnresolvedCrewMember],
    ) -> Result<Crew> {
        validate_crew_name(name).map_err(|e| anyhow!(e))?;
        let name = name.trim();
        let id = uuid::Uuid::new_v4().to_string();
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let resolved = resolve_in_tx(&tx, members)?;
        validate_crew_members(&resolved, super::crews::crew_member_cap())
            .map_err(|e| anyhow!(e))?;
        tx.execute(
            "INSERT INTO crews (id, name, description, version) VALUES (?1, ?2, ?3, 1)",
            params![&id, name, description],
        )
        .map_err(|e| anyhow!("could not create crew '{name}': {e}"))?;
        for (index, member) in resolved.iter().enumerate() {
            super::crews::insert_crew_member(&tx, &id, index as i64, member)?;
        }
        tx.commit()?;
        Ok(Crew {
            id,
            name: name.to_string(),
            description: description.map(str::to_string),
            version: 1,
        })
    }

    /// Update a crew whose replacement members may track named entries.
    /// Resolution happens inside the member-write transaction; a
    /// member-list write bumps `version` exactly as [`Database::crew_update`]
    /// does.
    pub fn crew_update_resolved(
        &self,
        name: &str,
        members: Option<&[UnresolvedCrewMember]>,
        description: Option<&str>,
        rename: Option<&str>,
    ) -> Result<Crew> {
        let name = name.trim();
        if let Some(rename) = rename {
            validate_crew_name(rename).map_err(|e| anyhow!(e))?;
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let resolved = members
            .map(|members| resolve_in_tx(&tx, members))
            .transpose()?;
        if let Some(resolved) = resolved.as_deref() {
            validate_crew_members(resolved, super::crews::crew_member_cap())
                .map_err(|e| anyhow!(e))?;
        }
        let (crew_id, current_version): (String, i64) = tx
            .query_row(
                "SELECT id, version FROM crews WHERE name = ?1",
                params![name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow!("crew '{name}' not found."))?;
        let new_name = rename.map(str::trim).unwrap_or(name);
        if rename.is_some() {
            let taken: bool = tx
                .query_row(
                    "SELECT 1 FROM crews WHERE name = ?1 AND id != ?2",
                    params![new_name, &crew_id],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if taken {
                anyhow::bail!("crew '{new_name}' already exists.");
            }
        }
        tx.execute(
            "UPDATE crews SET name = ?1, description = COALESCE(?2, description), updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?3",
            params![new_name, description, &crew_id],
        )?;
        tx.execute(
            "UPDATE ensembles SET crew_name = ?1 WHERE crew_id = ?2",
            params![new_name, &crew_id],
        )?;
        let new_version = if let Some(resolved) = resolved {
            tx.execute(
                "DELETE FROM crew_members WHERE crew_id = ?1",
                params![&crew_id],
            )?;
            for (index, member) in resolved.iter().enumerate() {
                super::crews::insert_crew_member(&tx, &crew_id, index as i64, member)?;
            }
            tx.execute(
                "UPDATE crews SET version = version + 1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
                params![&crew_id],
            )?;
            current_version + 1
        } else {
            current_version
        };
        let description: Option<String> = tx.query_row(
            "SELECT description FROM crews WHERE id = ?1",
            params![&crew_id],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(Crew {
            id: crew_id,
            name: new_name.to_string(),
            description,
            version: new_version,
        })
    }
}

impl Database {
    /// Named model entries (`crew_models`) plus the `model_ref` column on
    /// `crew_members`. Guarded `CREATE TABLE IF NOT EXISTS` + a
    /// pragma-guarded `ALTER TABLE` — a no-op on a fresh database (already
    /// in `BASE_SCHEMA_BATCHES`) and on an already-migrated one, and the
    /// thing that adds them to a database created before they existed.
    /// Pre-existing members keep `model_ref` NULL (literal members).
    pub(in crate::db) fn migrate_crew_models(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS crew_models (
                name TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                model TEXT,
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );",
        )
        .map_err(|e| anyhow!("Migration failed: {e}"))?;
        let has_model_ref: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('crew_members') WHERE name = 'model_ref'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_model_ref {
            conn.execute("ALTER TABLE crew_members ADD COLUMN model_ref TEXT", [])
                .map_err(|e| anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn literal(platform: &str, model: Option<&str>) -> UnresolvedCrewMember {
        UnresolvedCrewMember {
            platform: Some(platform.to_string()),
            model: model.map(str::to_string),
            model_ref: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }
    }

    fn tracked(name: &str) -> UnresolvedCrewMember {
        UnresolvedCrewMember {
            platform: None,
            model: None,
            model_ref: Some(name.to_string()),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }
    }

    #[test]
    fn set_replace_list_delete_round_trip() {
        let db = test_db();
        let outcome = db
            .crew_model_apply("agy-main", "antigravity", Some("gen-medium"))
            .unwrap();
        assert!(outcome.changed);
        assert!(outcome.crews_updated.is_empty());
        let replaced = db
            .crew_model_apply("agy-main", "antigravity", Some("gen-high"))
            .unwrap();
        assert!(replaced.changed);
        assert_eq!(replaced.model.model.as_deref(), Some("gen-high"));
        let entries = db.crew_model_list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0.name, "agy-main");
        assert_eq!(entries[0].0.platform, "antigravity");
        assert_eq!(entries[0].1, Vec::<String>::new());
        db.crew_model_delete("agy-main").unwrap();
        assert!(db.crew_model_list().unwrap().is_empty());
    }

    #[test]
    fn model_name_validation() {
        let db = test_db();
        let long = "x".repeat(65);
        for bad in ["", "   ", "has space", "semi;colon", long.as_str()] {
            assert!(db.crew_model_apply(bad, "p", None).is_err(), "{bad:?}");
        }
        assert!(db.crew_model_apply("ok._-9", "p", None).is_ok());
        let padded = db.crew_model_apply("  padded  ", " p ", None).unwrap();
        assert_eq!(padded.model.name, "padded");
        assert_eq!(padded.model.platform, "p");
    }

    #[test]
    fn create_with_ref_stores_the_entry_pair() {
        let db = test_db();
        db.crew_model_apply("agy-main", "antigravity", Some("gen-medium"))
            .unwrap();
        db.crew_create_resolved(
            "team",
            None,
            &[tracked("agy-main"), literal("opencode", Some("m1"))],
        )
        .unwrap();
        let (_, members) = db.crew_get("team").unwrap().unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].platform, "antigravity");
        assert_eq!(members[0].model.as_deref(), Some("gen-medium"));
        assert_eq!(members[0].model_ref.as_deref(), Some("agy-main"));
        assert_eq!(members[1].platform, "opencode");
        assert_eq!(members[1].model_ref, None);
        let entries = db.crew_model_list().unwrap();
        assert_eq!(entries[0].1, vec!["team".to_string()]);
    }

    #[test]
    fn set_rewrites_every_referencing_member_and_bumps_each_crew_once() {
        let db = test_db();
        db.crew_model_apply("agy-main", "antigravity", Some("gen-medium"))
            .unwrap();
        db.crew_create_resolved("one", None, &[tracked("agy-main"), tracked("agy-main")])
            .unwrap();
        db.crew_create_resolved("two", None, &[tracked("agy-main")])
            .unwrap();
        db.crew_create_resolved("plain", None, &[literal("opencode", None)])
            .unwrap();
        let before: Vec<(String, i64)> = ["one", "two", "plain"]
            .iter()
            .map(|name| {
                let (crew, _) = db.crew_get(name).unwrap().unwrap();
                (crew.name, crew.version)
            })
            .collect();
        assert!(before.iter().all(|(_, version)| *version == 1));
        let outcome = db
            .crew_model_apply("agy-main", "antigravity", Some("gen-high"))
            .unwrap();
        assert!(outcome.changed);
        assert_eq!(
            outcome.crews_updated,
            vec!["one".to_string(), "two".to_string()]
        );
        for name in ["one", "two"] {
            let (crew, members) = db.crew_get(name).unwrap().unwrap();
            assert_eq!(crew.version, 2, "{name} bumps exactly once");
            assert!(members.iter().all(|member| {
                member.platform == "antigravity"
                    && member.model.as_deref() == Some("gen-high")
                    && member.model_ref.as_deref() == Some("agy-main")
            }));
        }
        let (crew, members) = db.crew_get("plain").unwrap().unwrap();
        assert_eq!(crew.version, 1, "an untracked crew is untouched");
        assert_eq!(members[0].platform, "opencode");
    }

    #[test]
    fn setting_the_same_pair_is_a_no_op() {
        let db = test_db();
        db.crew_model_apply("agy-main", "antigravity", Some("gen-medium"))
            .unwrap();
        db.crew_create_resolved("one", None, &[tracked("agy-main")])
            .unwrap();
        let outcome = db
            .crew_model_apply("  agy-main  ", "antigravity", Some("gen-medium"))
            .unwrap();
        assert!(!outcome.changed);
        assert!(outcome.crews_updated.is_empty());
        let (crew, _) = db.crew_get("one").unwrap().unwrap();
        assert_eq!(crew.version, 1);
    }

    #[test]
    fn bad_member_refs_are_refused() {
        let db = test_db();
        db.crew_model_apply("agy-main", "antigravity", None)
            .unwrap();
        let unknown = db
            .crew_create_resolved("bad", None, &[tracked("missing")])
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("unknown model_ref 'missing'"), "{unknown}");
        let unresolved = |platform: Option<&str>, model_ref: Option<&str>| UnresolvedCrewMember {
            platform: platform.map(str::to_string),
            model: None,
            model_ref: model_ref.map(str::to_string),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        for members in [
            unresolved(Some("p"), Some("agy-main")),
            unresolved(None, None),
        ] {
            let err = db
                .crew_create_resolved("bad", None, &[members])
                .unwrap_err()
                .to_string();
            assert!(err.contains("member 0"), "{err}");
            assert!(err.contains("exactly one of platform/model_ref"), "{err}");
        }
        let with_model = UnresolvedCrewMember {
            platform: None,
            model: Some("m".to_string()),
            model_ref: Some("agy-main".to_string()),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        let err = db
            .crew_create_resolved("bad", None, &[with_model])
            .unwrap_err()
            .to_string();
        assert!(err.contains("member 0"), "{err}");
        assert!(
            db.crew_get("bad").unwrap().is_none(),
            "refused writes nothing"
        );
    }

    #[test]
    fn delete_is_refused_while_referenced_then_succeeds() {
        let db = test_db();
        db.crew_model_apply("agy-main", "antigravity", None)
            .unwrap();
        db.crew_create_resolved("one", None, &[tracked("agy-main")])
            .unwrap();
        db.crew_create_resolved("two", None, &[tracked("agy-main")])
            .unwrap();
        let err = db.crew_model_delete("agy-main").unwrap_err().to_string();
        assert!(err.contains("one"), "{err}");
        assert!(err.contains("two"), "{err}");
        db.crew_update_resolved("one", Some(&[literal("p", None)]), None, None)
            .unwrap();
        let err = db.crew_model_delete("agy-main").unwrap_err().to_string();
        assert!(err.contains("two"), "{err}");
        assert!(!err.contains("one"), "{err}");
        db.crew_update_resolved("two", Some(&[literal("p", None)]), None, None)
            .unwrap();
        db.crew_model_delete("agy-main").unwrap();
        assert!(db
            .crew_model_delete("agy-main")
            .unwrap_err()
            .to_string()
            .contains("not found"));
    }

    #[test]
    fn migration_adds_table_and_column_to_an_old_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE crews (
                    id TEXT PRIMARY KEY,
                    name TEXT UNIQUE NOT NULL,
                    description TEXT,
                    version INTEGER NOT NULL DEFAULT 1,
                    created_at TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL DEFAULT ''
                );
                CREATE TABLE crew_members (
                    crew_id TEXT NOT NULL REFERENCES crews(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    platform TEXT NOT NULL,
                    model TEXT,
                    timeout_minutes INTEGER,
                    infra_retry_limit INTEGER,
                    infra_crash_max_seconds INTEGER,
                    infra_backoff_seconds INTEGER,
                    PRIMARY KEY (crew_id, position)
                );
                INSERT INTO crews (id, name, version) VALUES ('c1', 'legacy', 1);
                INSERT INTO crew_members (crew_id, position, platform, model)
                    VALUES ('c1', 0, 'opencode', 'm1');",
            )
            .unwrap();
            Database::migrate_crew_models(&conn).unwrap();
            let has_table: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'crew_models'",
                    [],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap();
            assert!(has_table);
            let has_column: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('crew_members') WHERE name = 'model_ref'",
                    [],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap();
            assert!(has_column);
        }
        let db = Database::new(&path).unwrap();
        let (_, members) = db.crew_get("legacy").unwrap().unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].platform, "opencode");
        assert_eq!(members[0].model_ref, None);
    }
}
