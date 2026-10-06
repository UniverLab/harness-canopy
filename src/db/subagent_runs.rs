use anyhow::Result;
use rusqlite::OptionalExtension;

use super::{Database, NewSubagentRun, SubagentRunRecord};

impl Database {
    pub fn insert_subagent_run(&self, run: NewSubagentRun<'_>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO subagent_runs (id, platform, model, prompt, workdir, status, started_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7)",
            rusqlite::params![
                run.id,
                run.platform,
                run.model,
                run.prompt,
                run.workdir,
                run.started_at,
                run.expires_at
            ],
        )?;
        Ok(())
    }

    pub fn set_subagent_run_pid(&self, id: &str, pid: i64, boot_id: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE subagent_runs SET pid = ?1, boot_id = ?2 WHERE id = ?3",
            rusqlite::params![pid, boot_id, id],
        )?;
        Ok(())
    }

    pub fn complete_subagent_run(
        &self,
        id: &str,
        exit_code: i32,
        stdout: &str,
        stderr: &str,
        mcp_surface: Option<&str>,
        finished_at: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE subagent_runs SET status = 'finished', exit_code = ?1, stdout = ?2, stderr = ?3, mcp_surface = ?4, finished_at = ?5 WHERE id = ?6",
            rusqlite::params![exit_code, stdout, stderr, mcp_surface, finished_at, id],
        )?;
        Ok(())
    }

    pub fn fail_subagent_run(
        &self,
        id: &str,
        stderr: &str,
        mcp_surface: Option<&str>,
        finished_at: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE subagent_runs SET status = 'failed', stderr = ?1, mcp_surface = ?2, finished_at = ?3 WHERE id = ?4",
            rusqlite::params![stderr, mcp_surface, finished_at, id],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_subagent_run(&self, id: &str) -> Result<Option<SubagentRunRecord>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, platform, model, prompt, workdir, mcp_surface, status, exit_code, stdout, stderr, started_at, finished_at, collected_at, expires_at
             FROM subagent_runs WHERE id = ?1",
            [id],
            |row| {
                Ok(SubagentRunRecord {
                    id: row.get(0)?,
                    platform: row.get(1)?,
                    model: row.get(2)?,
                    prompt: row.get(3)?,
                    workdir: row.get(4)?,
                    mcp_surface: row.get(5)?,
                    status: row.get(6)?,
                    exit_code: row.get(7)?,
                    stdout: row.get(8)?,
                    stderr: row.get(9)?,
                    started_at: row.get(10)?,
                    finished_at: row.get(11)?,
                    collected_at: row.get(12)?,
                    expires_at: row.get(13)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn collect_subagent_run(&self, id: &str) -> Result<Option<SubagentRunRecord>> {
        let conn = self.conn.lock().unwrap();
        let record = conn
            .query_row(
                "SELECT id, platform, model, prompt, workdir, mcp_surface, status, exit_code, stdout, stderr, started_at, finished_at, collected_at, expires_at
                 FROM subagent_runs WHERE id = ?1",
                [id],
                |row| {
                    Ok(SubagentRunRecord {
                        id: row.get(0)?,
                        platform: row.get(1)?,
                        model: row.get(2)?,
                        prompt: row.get(3)?,
                        workdir: row.get(4)?,
                        mcp_surface: row.get(5)?,
                        status: row.get(6)?,
                        exit_code: row.get(7)?,
                        stdout: row.get(8)?,
                        stderr: row.get(9)?,
                        started_at: row.get(10)?,
                        finished_at: row.get(11)?,
                        collected_at: row.get(12)?,
                        expires_at: row.get(13)?,
                    })
                },
            )
            .optional()?;
        // Discard the row only once it holds a real result. A collect that
        // races the still-`running` subagent returns the "running" status but
        // leaves the row so a later collect can retrieve the actual output;
        // an abandoned running row is caught by `expire_subagent_runs`.
        if record
            .as_ref()
            .is_some_and(|r| r.status == "finished" || r.status == "failed")
        {
            conn.execute("DELETE FROM subagent_runs WHERE id = ?1", [id])?;
        }
        Ok(record)
    }

    pub fn expire_subagent_runs(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let count = conn.execute(
            "DELETE FROM subagent_runs WHERE collected_at IS NOT NULL OR expires_at < ?1",
            [&now],
        )?;
        // CM18: tombstones expire on the same schedule as the rows they
        // stand in for, using the original `expires_at` copied at delivery.
        let tombstones = conn.execute(
            "DELETE FROM subagent_delivered_tombstones WHERE expires_at < ?1",
            [&now],
        )?;
        Ok((count + tombstones) as u64)
    }

    /// CM18: record that a blocking spawn delivered `id` inline. The
    /// `subagent_runs` row is already deleted; this tombstone lets a later
    /// `subagent_collect` answer "already delivered" instead of "not found".
    /// `expires_at` must be the original run's expiry so natural expiry
    /// cleans the tombstone on the same schedule.
    pub fn insert_delivered_tombstone(
        &self,
        id: &str,
        delivered_at: &str,
        expires_at: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO subagent_delivered_tombstones (id, delivered_at, expires_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![id, delivered_at, expires_at],
        )?;
        Ok(())
    }

    /// CM18: true if `id` was delivered by a blocking spawn (and its
    /// tombstone has not yet expired).
    pub fn is_delivered_tombstone(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM subagent_delivered_tombstones WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    #[allow(dead_code)]
    pub fn delete_delivered_tombstone(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM subagent_delivered_tombstones WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }
}
