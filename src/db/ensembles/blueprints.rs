use super::*;

impl Database {
    pub fn insert_ensemble_blueprint(&self, blueprint: &EnsembleBlueprint) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO ensemble_blueprints (id, name, prompt_template, members, min_pass, quorum_grace_minutes, builtin, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &blueprint.id,
                &blueprint.name,
                &blueprint.prompt_template,
                serde_json::to_string(&blueprint.members)?,
                blueprint.min_pass,
                blueprint.quorum_grace_minutes,
                blueprint.builtin,
                blueprint.created_at.timestamp(),
            ],
        )?;
        Ok(())
    }

    pub fn get_ensemble_blueprint_by_name(&self, name: &str) -> Result<Option<EnsembleBlueprint>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, prompt_template, members, min_pass, quorum_grace_minutes, builtin, created_at
             FROM ensemble_blueprints WHERE name = ?1",
        )?;
        stmt.query_row(params![name], map_ensemble_blueprint_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn list_ensemble_blueprints(&self) -> Result<Vec<EnsembleBlueprint>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, prompt_template, members, min_pass, quorum_grace_minutes, builtin, created_at
             FROM ensemble_blueprints ORDER BY builtin DESC, name ASC",
        )?;
        let rows = stmt.query_map([], map_ensemble_blueprint_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Delete an ensemble blueprint by name. Mirrors
    /// [`Self::delete_blueprint_by_name`]: callers own any builtin guard —
    /// this performs none itself.
    pub fn delete_ensemble_blueprint_by_name(&self, name: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "DELETE FROM ensemble_blueprints WHERE name = ?1",
            params![name],
        )?;
        Ok(rows > 0)
    }

    /// Overwrite a builtin ensemble blueprint row's `prompt_template`/
    /// `members`/`min_pass` in place — the ensemble mirror of
    /// [`Self::update_builtin_blueprint`], reconciling a stale builtin shape
    /// (e.g. a hardcoded model identity from before this field was dropped)
    /// with the current spec. Only ever called from
    /// [`Self::seed_builtin_ensemble_blueprints`].
    pub(super) fn update_builtin_ensemble_blueprint(
        &self,
        id: &str,
        prompt_template: &str,
        members: &[EnsembleMemberSpec],
        min_pass: Option<i64>,
        quorum_grace_minutes: Option<i64>,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE ensemble_blueprints SET prompt_template = ?1, members = ?2, min_pass = ?3, quorum_grace_minutes = ?4 WHERE id = ?5",
            params![
                prompt_template,
                serde_json::to_string(members)?,
                min_pass,
                quorum_grace_minutes,
                id,
            ],
        )?;
        Ok(())
    }

    /// Seed the builtin ensemble blueprints (currently just
    /// "ensemble-proposers") and keep already-seeded builtin rows in sync
    /// with the current `builtin_ensemble_blueprint_specs()` shape —
    /// idempotent and safe on every daemon startup, mirroring
    /// [`Self::seed_builtin_blueprints`]'s stale-name-delete /
    /// drifted-config-overwrite / leave-custom-rows-alone behavior exactly.
    pub fn seed_builtin_ensemble_blueprints(&self) -> Result<()> {
        let specs = builtin_ensemble_blueprint_specs();
        let current_names: std::collections::HashSet<&str> =
            specs.iter().map(|(name, ..)| *name).collect();

        for existing in self.list_ensemble_blueprints()? {
            if existing.builtin && !current_names.contains(existing.name.as_str()) {
                self.delete_ensemble_blueprint_by_name(&existing.name)?;
            }
        }

        for (name, prompt_template, members, min_pass) in specs {
            let members: Vec<EnsembleMemberSpec> = members
                .into_iter()
                .map(|(platform, model, prompt_override)| {
                    (
                        platform.to_string(),
                        model.map(str::to_string),
                        prompt_override.map(str::to_string),
                        None,
                        None,
                        None,
                        None,
                    )
                })
                .collect();

            match self.get_ensemble_blueprint_by_name(name)? {
                Some(existing) if existing.builtin => {
                    if existing.prompt_template != prompt_template
                        || existing.members != members
                        || existing.min_pass != min_pass
                        || existing.quorum_grace_minutes.is_some()
                    {
                        self.update_builtin_ensemble_blueprint(
                            &existing.id,
                            prompt_template,
                            &members,
                            min_pass,
                            None,
                        )?;
                    }
                }
                // Name already claimed by a custom ensemble blueprint — leave it be.
                Some(_) => {}
                None => {
                    self.insert_ensemble_blueprint(&EnsembleBlueprint {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: name.to_string(),
                        prompt_template: prompt_template.to_string(),
                        members,
                        min_pass,
                        quorum_grace_minutes: None,
                        builtin: true,
                        created_at: Utc::now(),
                    })?;
                }
            }
        }
        Ok(())
    }
}

/// Parse a blueprint row's stored `members` JSON, tolerating the
/// pre-prompt-override 2-element `[platform, model]` shape, the
/// pre-member-timeout 3-element `[platform, model, prompt_override]` shape
/// (rows seeded before this field existed — including a builtin blueprint
/// from an older daemon), the pre-CM30 4-element
/// `[platform, model, prompt_override, timeout_minutes]` shape, and the
/// current 7-element `[platform, model, prompt_override, timeout_minutes,
/// infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds]`
/// shape, so an upgrade never needs a data migration for blueprints
/// already in the DB.
pub(super) fn parse_ensemble_blueprint_members(
    raw: &str,
) -> Result<Vec<EnsembleMemberSpec>, serde_json::Error> {
    let rows: Vec<serde_json::Value> = serde_json::from_str(raw)?;
    Ok(rows
        .into_iter()
        .map(|entry| {
            let platform = entry
                .get(0)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let model = entry
                .get(1)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let prompt_override = entry
                .get(2)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let timeout_minutes = entry.get(3).and_then(serde_json::Value::as_i64);
            let infra_retry_limit = entry.get(4).and_then(serde_json::Value::as_i64);
            let infra_crash_max_seconds = entry.get(5).and_then(serde_json::Value::as_i64);
            let infra_backoff_seconds = entry.get(6).and_then(serde_json::Value::as_i64);
            (
                platform,
                model,
                prompt_override,
                timeout_minutes,
                infra_retry_limit,
                infra_crash_max_seconds,
                infra_backoff_seconds,
            )
        })
        .collect())
}

pub(super) fn map_ensemble_blueprint_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<EnsembleBlueprint> {
    let members_raw: String = row.get(3)?;
    let members = parse_ensemble_blueprint_members(&members_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(EnsembleBlueprint {
        id: row.get(0)?,
        name: row.get(1)?,
        prompt_template: row.get(2)?,
        members,
        min_pass: row.get(4)?,
        quorum_grace_minutes: row.get(5)?,
        builtin: row.get(6)?,
        created_at: from_timestamp(row.get(7)?)?,
    })
}
