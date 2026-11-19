//! Hub spawn helpers: build the `spawn` frame for an agent or terminal
//! launch and resolve the spawned id back to its metadata row.
//!
//! Split out of `agent::mod` to keep every production file within the
//! file-size gate; the behaviour is the TUI spawn path's.

use anyhow::Result;

use crate::sessions::client::{new_req, with_active};
use crate::sessions::protocol::{
    list_frame, spawn_frame, spawn_frame_full, HostFrame, SessionItem, SpawnExt, SpawnFrameArgs,
};

/// Launch fields for one hub spawn; a struct so the spawn helper stays
/// under the argument-count lint without silencing it.
pub(super) struct HubSpawnArgs<'a> {
    pub(super) kind: &'a str,
    pub(super) cli: &'a str,
    pub(super) model: Option<&'a str>,
    pub(super) workdir: &'a str,
    pub(super) name: &'a str,
    pub(super) interactive_args: Option<&'a str>,
    pub(super) fallback_args: Option<&'a str>,
    pub(super) model_flag: Option<&'a str>,
    pub(super) id: Option<&'a str>,
    pub(super) seed_id: Option<&'a str>,
    pub(super) accent: Option<&'a str>,
    pub(super) harness_conversation_id: Option<&'a str>,
    pub(super) preserve_args: bool,
}

/// Spawn a hub session and return its metadata row: `spawn` answers the id,
/// then `list` finds the row the hub announced for it. A child that dies
/// before the follow-up `list` lands is already gone from `list` but its
/// verdict is on the database row: synthesize the row from the stored exit
/// code and failure detail (see [`SessionHub::db`]) instead of erroring, so
/// the caller can attach the error card the failing launch earned.
pub(super) fn spawn_on_hub(args: &HubSpawnArgs<'_>) -> Result<SessionItem> {
    let ext = SpawnExt {
        interactive_args: args.interactive_args.map(str::to_owned),
        fallback_args: args.fallback_args.map(str::to_owned),
        model_flag: args.model_flag.map(str::to_owned),
        id: args.id.map(str::to_owned),
        seed_id: args.seed_id.map(str::to_owned),
        accent: args.accent.map(str::to_owned),
        harness_conversation_id: args.harness_conversation_id.map(str::to_owned),
        yolo: false,
        preserve_args: args.preserve_args,
    };
    let req = new_req();
    let frame_args = SpawnFrameArgs {
        req: &req,
        kind: args.kind,
        cli: args.cli,
        model: args.model,
        workdir: args.workdir,
        name: Some(args.name),
        prompt: None,
        ext: &ext,
    };
    let spawned = with_active(|active| active.client.request(&req, spawn_frame_full(&frame_args)))
        .ok_or_else(|| anyhow::anyhow!("not connected to the session hub"))??;
    let HostFrame::Spawned { id, .. } = spawned else {
        anyhow::bail!("hub answered spawn with an unexpected frame");
    };
    let req = new_req();
    let listed = with_active(|active| active.client.request(&req, list_frame(&req)))
        .ok_or_else(|| anyhow::anyhow!("not connected to the session hub"))??;
    let HostFrame::Sessions { items, .. } = listed else {
        anyhow::bail!("hub answered list with an unexpected frame");
    };
    items
        .into_iter()
        .find(|item| item.id == id)
        .or_else(|| failed_spawn_item(args, &id))
        .ok_or_else(|| anyhow::anyhow!("hub has no session '{id}' it just spawned"))
}

/// Terminal spawn through the short frame: terminals carry no agent launch
/// fields, so the 7-arg builder stays the non-test caller of `spawn_frame`.
pub(super) fn spawn_on_hub_terminal(
    kind: &str,
    cli: &str,
    workdir: &str,
    name: &str,
) -> Result<SessionItem> {
    let req = new_req();
    let spawned = with_active(|active| {
        active.client.request(
            &req,
            spawn_frame(&req, kind, cli, None, workdir, Some(name), None),
        )
    })
    .ok_or_else(|| anyhow::anyhow!("not connected to the session hub"))??;
    let HostFrame::Spawned { id, .. } = spawned else {
        anyhow::bail!("hub answered spawn with an unexpected frame");
    };
    let req = new_req();
    let listed = with_active(|active| active.client.request(&req, list_frame(&req)))
        .ok_or_else(|| anyhow::anyhow!("not connected to the session hub"))??;
    let HostFrame::Sessions { items, .. } = listed else {
        anyhow::bail!("hub answered list with an unexpected frame");
    };
    items
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| anyhow::anyhow!("hub has no session '{id}' it just spawned"))
}

/// A child that died before the post-spawn `list` is already gone from
/// `list`, but the daemon recorded its verdict on the database row first:
/// rebuild the metadata row from the stored exit code and failure detail so
/// the caller can attach the error card the failing launch earned. `None`
/// when the row carries no stored nonzero verdict — the session really is
/// missing and the caller still errors.
fn failed_spawn_item(args: &HubSpawnArgs<'_>, id: &str) -> Option<SessionItem> {
    let db = with_active(|active| active.db.clone())?;
    failed_verdict(&db, args, id)
}

/// The stored nonzero verdict for `id`, rebuilt as a metadata row: `None`
/// when the row carries none — the session really is missing.
fn failed_verdict(
    db: &crate::db::Database,
    args: &HubSpawnArgs<'_>,
    id: &str,
) -> Option<SessionItem> {
    let row = db.get_interactive_session(id).ok()??;
    let code = db.get_session_exit_code(id).ok()??;
    if row.status != "error" || code == 0 {
        return None;
    }
    Some(SessionItem {
        id: row.id,
        name: row.name,
        kind: args.kind.to_string(),
        cli: row.cli,
        model: None,
        workdir: row.working_dir,
        started_at: row.started_at.clone(),
        pid: None,
        cols: 80,
        rows: 24,
        last_output_at: row.started_at,
        status: "exited".to_string(),
        exit_code: Some(code),
    })
}

/// Test bridge to the Resume-step spawn path: spawn `cli` as an agent and
/// wait until its verdict lands on the database row, then return the metadata
/// row `spawn_on_hub` resolves for it — the live row, or the synthesized
/// failed row when the child already exited before the follow-up `list`.
#[cfg(test)]
pub(crate) mod hub_spawn_for_test {
    use super::{spawn_on_hub, HubSpawnArgs};
    use crate::sessions::protocol::SessionItem;

    /// A child that exits nonzero at once (`sh -c 'exit 3'`): unless the
    /// scheduler stalls, `list` lands after the reap and the failed row is
    /// what the Resume step sees.
    pub(crate) fn spawn_fast_failing_agent(workdir: &str, name: &str) -> SessionItem {
        let item = spawn_on_hub(&HubSpawnArgs {
            kind: "agent",
            cli: "sh",
            model: None,
            workdir,
            name,
            interactive_args: Some("-c 'exit 3'"),
            fallback_args: None,
            model_flag: None,
            id: None,
            seed_id: None,
            accent: None,
            harness_conversation_id: None,
            preserve_args: false,
        })
        .expect("a fast failure still resolves its row");
        poll_error_row(&item.id);
        item
    }

    /// Wait until the row reads `error`: the verdict the card is built from.
    fn poll_error_row(id: &str) {
        use crate::sessions::client::test_client;
        let db = test_client().hub.db();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if db
                .get_interactive_session_status(id)
                .unwrap_or(None)
                .as_deref()
                == Some("error")
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the fast failure never landed on the row"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
