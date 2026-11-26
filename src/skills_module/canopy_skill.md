---
name: canopy
canopy_version: {{VERSION}}
description: "Use at the start of ANY task in a workspace where the Canopy MCP server is available, and whenever the user asks what Canopy can do: it gives the Canopy protocol, the capabilities, and the rule to list the other skills with canopy skill list and load the ones that apply with canopy skill get <name>."
---

# Canopy

You are working in a workspace coordinated by Canopy. The Canopy MCP
server is the control plane. Read this skill once, then follow it.

## 1. Protocol

Call `get_tools` first and follow whatever it returns for the scope:

- At the start of any task, call `get_tools` with `scope="session_start"`
  before anything else. It tells you what to do first.
- Before editing files, call `get_tools` with `scope="file_write"`
  (passing the file path) and follow the read-before-modifying rules.
- Before running tests or builds, call `get_tools` with
  `scope="test_run"` and follow the run-and-report rules.
- Before ending the session, call `get_tools` with
  `scope="close_session"` and follow the wrap-up rules.
- When coordinating with other agents or sessions, call `get_tools`
  with `scope="multi_agent"`.
- Record durable facts and patterns with `intelligence_upsert`, never
  only in chat. Read project context with `intelligence_get_context`.

## 2. Capabilities

Discover the live tool surface first with `get_tools`: it tells you
exactly which tools each scope needs. The MCP tools by group:

| Group | Purpose |
|---|---|
| agents | Run background agents and ephemeral subagents; read their logs and daemon health (`agent`, `agent_edit`, `subagent`). |
| graphs | Read, create, change, and run graphs: nodes, edges, ensembles, runs, pauses, and schedules (`graph`, `graph_edit`, `graph_node`, `graph_edge`, `graph_ensemble`, `graph_control`, `node`). |
| specs and queues | Read and write specs and spec notes; author ordered spec queues (`spec`, `spec_edit`, `queue`). |
| crews and ensembles | Manage crews and harness/model assignments; inspect model catalogs and platform liveness (`crew`, `harness`). |
| sessions and scheduled sends | List live sessions, shelve and read session files, schedule prompts for future delivery (`session_list`, `session_send_file`, `scheduled_prompts`). |
| intelligence and RAG | Persist and search durable facts, patterns, and ideas; search indexed personal content (`intelligence_get_context`, `intelligence_upsert`, `intelligence_search`, `rag`). |
| remote | Control this hub from the browser: pairing, devices, and trust (`remote`). |
| config | Read and change canopy settings; register and search projects (`config`, `project`). |

Common needs, one line each:

- remember across sessions: persist with `intelligence_upsert`, recall with `intelligence_get_context`.
- run something on a schedule: schedule a background agent (`agent_edit`) or a prompt delivery (`scheduled_prompts`).
- run a multi-step process as a graph: author it (`graph_edit`, `graph_node`, `graph_edge`) and run it (`graph_control`).
- review work with another model: launch a `subagent` (or an ensemble member) on a different platform/model pair.
- send a message to another session: schedule a prompt to it (`scheduled_prompts`) or shelve a file for it (`session_send_file`).

## 3. Skills

Skills beyond this one are discovered live, never from a fixed list:

1. At the start of the task, run `canopy skill list`. It prints every
   available skill with the description that says when to use it.
2. Load each skill that applies to the task with
   `canopy skill get <name>` and follow it.
3. Never copy a skill into a skills directory. The live list is always
   the output of `canopy skill list`; reading a skill is always
   `canopy skill get <name>`.

Examples of what the list will show: `execution-mindset` on every
task; `architect-mindset` when the work is design (boundaries, plans,
specs); `code-engineering` when the work is code.
