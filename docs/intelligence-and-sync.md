---
title: Intelligence & Sync
description: The project-scoped knowledge graph and multi-agent workspace coordination.
order: 6
---

# Intelligence & Sync

## Intelligence — the knowledge graph

Canopy persists what agents learn, scoped to projects.

- **Project scoping** — knowledge is keyed by `project_hash` (SHA-256 of
  the canonical workdir path, truncated to 8 hex chars), auto-detected
  from the session workdir. Agents never set it manually.
- **Node kinds** — facts, patterns and session summaries.
- **Writes** — `intelligence_upsert` is all-or-nothing. A relation whose
  target cannot be resolved fails the whole call and leaves no half-written
  node behind. A relation target may be a short id prefix, which is resolved
  to the full id before anything is written.
- **Full-text search** — `intelligence_search` queries nodes by text and
  optional kind filter across all projects.
- **Graph walk** — `intelligence_admin` `walk` traverses from any node up
  to a configurable depth, returning connected facts, patterns and
  cross-references.
- **Project relationships** — `intelligence_admin` `link_projects` links projects
  with typed relations (`depends_on`, `complements`, `extends`, `publishes`;
  `relates_to` accepted as legacy; `contains` is derived from registry paths
  and cannot be hand-linked). Scoped reads traverse outbound edges (each hit
  marked with `via_project` + `via_relation`, `contains` hops read as
  `inherited:contains`), and full context warns about inbound `depends_on`
  dependents via `dependency_impact`.
- **Context retrieval** — `intelligence_get_context` auto-detects the
  project and returns a curated mix of session knowledge, project facts
  and related-project summaries.

A typical agent session starts with `intelligence_get_context` and ends
with an `intelligence_upsert` of a session summary — so the next session
starts smarter.

## Project registry

Canopy registers a project only explicitly: a `.canopy-project` marker file
in the directory root enables automatic registration, or call `project`
`register` with the directory path. Registration derives `contains`
edges from registry paths automatically. `project` `search` and
`project` `update` expose the registry to agents; registry noise is removable
via project deletion / `canopy clean`, and `project` `remap` moves a project
(including its graph node) after a directory rename or move.

## Sync — retired

The multi-agent sync protocol (mission declaration, workspace status,
broadcast messaging, active context) is retired: the live broadcast channels
and their MCP tools are gone. What remains is the durable history — past
missions and chatter stay in the database, where `intelligence_get_context`
surfaces them and the TUI renders them. Sessions coordinate through
`session_list` (see every live session), `scheduled_send_create` (wake a
session later instead of polling), and `intelligence_upsert` /
`intelligence_search` (persist and find shared findings).

The graph engine narrates itself into this same history: graph lifecycle
transitions, spec starts/completions, node and ensemble terminal states, and
hook firings are written directly into the activity log the TUI's activity
panel already reads, attributed as `graph:<name>`.

## Action protocol advisor

`get_tools` returns scope-sensitive action protocols (`session_start`,
`file_write`, `test_run`, `close_session`, `multi_agent`) with risk
levels and recommended tool sets — a built-in playbook agents consult
before acting.
