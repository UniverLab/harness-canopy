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
/// then `list` finds the row the hub announced for it.
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
