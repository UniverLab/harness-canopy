---
title: Agents
description: Interactive PTY agents, background cron/watcher agents, terminal sessions and seed identities.
order: 5
---

# Agents

Canopy manages three kinds of agents.

## Interactive agents

Each interactive agent runs your chosen AI CLI in a dedicated
pseudo-terminal with full vt100 emulation, 24-bit color and cursor
positioning — anything that works in a terminal works inside canopy.

The daemon hosts those pseudo-terminals, so an interactive session outlives
the TUI that started it: quitting the TUI (`F10` or closing the window) keeps
every running session alive, and the next TUI attaches to it with its screen
and scrollback intact. Restarting the daemon does end its sessions — they
are its children — but as soon as the new daemon starts it brings back every
session the database still lists as active, under the same id and name; a
session that ended while no TUI was open is not reopened. Closing a session
in the TUI ends it.

## Background agents

Background agents run unattended, triggered by:

- **Cron schedules** — an event-driven Tokio scheduler computes precise
  wake-up times and sleeps until needed; near-zero CPU when idle.
- **File events** — create/modify/delete/move events via the `notify`
  crate, with configurable debouncing and recursive directory monitoring.

They support configurable timeouts, automatic retries, execution logging
(5 MB rotation) and per-run status tracking. Prompts support template
variables: `{{TIMESTAMP}}`, `{{TASK_ID}}`, `{{LOG_PATH}}`,
`{{FILE_PATH}}`, `{{EVENT_TYPE}}`.

Background agents are managed from the TUI or by other agents through the
[agent MCP tools](mcp-tools.md) (`agent_edit` `add`, `agent_edit` `watch`,
`agent_edit` `run`, `agent` `status`, …).

### Send to a session

A background agent can deliver its prompt into a *live interactive session*
instead of running a CLI headlessly. Set `delivery_action` to `send_to_session` (`action` is the tool's dispatch key, so the delivery parameter is named `delivery_action`) and give
a target session — by **name** is recommended, because the name is resolved
against the live sessions on every fire, and a resumed session keeps
both its id and its name — only a name held by another live session mints
a fresh name, keeping the id. A conversation picked out of the harness's own
list that canopy has never seen starts a fresh session with a new id and a
new generated name (see
[Graphs](graphs-hooks-and-queues.md#event-keyed-hooks)). Every time the agent's cron or watch
trigger fires, canopy enqueues the prompt as a scheduled send to that session,
and the daemon delivers it on its next scheduler tick. Delivery does not need
a TUI: a message that fires while nobody is watching still reaches a live
session. If the target session is gone by the time it is due, the prompt is
preserved for recall in that project (the same recall the schedule dialog
offers) and a notification says the session was not found, rather than the
message being dropped silently. If a fire is already waiting for the same
target, a new fire replaces that message rather than piling up a second one.

Cron example (MCP call):

```json
{"tool":"agent_edit","arguments":{"action":"add","id":"hourly-orchestrator-nudge","schedule":"0 * * * *","delivery_action":"send_to_session","target_session_name":"orchestrator","prompt":"Check the release board and report anything blocked."}}
```

Watch example:

```json
{"tool":"agent_edit","arguments":{"action":"watch","id":"spec-changed-nudge","path":"/abs/path/to/specs","events":["modify","create"],"delivery_action":"send_to_session","target_session_name":"orchestrator","prompt":"A spec changed; re-read it and update the plan."}}
```

Omit both target fields to target the calling session. Use `agent_edit` `run` to
deliver once now.

## Terminal sessions

Raw shell sessions with per-session command history (TOML-backed),
cross-session autocomplete search and a Warp-like input mode.

## Seed nursery

The **seed nursery** is a collaborative workspace for creating new seed
identities. When you select "Plant New Seed" in the TUI, Canopy creates
a temporary directory containing:

- A draft `identity.toml` with empty/default fields.
- An instruction file (`AGENTS.md`, `GEMINI.md`, etc., per the platform
  registry) with gardener instructions that guide the selected CLI to
  interview the user about the seed's name, directives, and traits.

Once the agent finishes the interview and writes the final
`identity.toml`, the nursery validates it (name uniqueness, field
constraints, 4 KB size cap) and moves it to `~/.canopy/seeds/<id>/`.
The temporary directory is cleaned up automatically.

## Seed identities

A **seed** is a persistent, evolvable agent identity stored as structured
TOML at `~/.canopy/seeds/<id>/identity.toml`:

```toml
name = "Quercus"
created_at = "2026-01-01T00:00:00Z"

[directives]
general = [
    "Prioritize type-safety",
    "Explain structural changes before executing"
]

[traits]
tone = "Concise, Technical"
focus = "Refactoring, Architecture"
```

- Each seed has a unique name (case-insensitive), behavioral directives
  and personality traits; identities are capped at 4 KB and validated.
- Sessions bound to a seed receive its prompt injection automatically.
- The `identity` MCP tool's `evolve` action lets agents refine their own identity
  over time; its `create_seed`, `list_seeds` and `remove_seed` actions manage the
  nursery.

## Context transfer

Conversation context, prompts and output can be transferred between
agents while preserving session state and scrollback history — from the
TUI or as part of automated flows.
