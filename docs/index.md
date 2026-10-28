---
title: Canopy
description: Self-contained MCP server and TUI for orchestrating AI agent sessions, background tasks, and file event triggers.
order: 1
---

# Canopy

Canopy (`harness-canopy`) is a modern, self-contained **MCP (Model Context
Protocol) server and terminal UI** for orchestrating AI agent sessions,
background tasks and file event triggers. One Rust binary, zero runtime
dependencies.

It turns a single machine into a small agent operations center:

- **Schedule** agents on cron expressions or file-system events.
- **Run** interactive agents in real PTYs with full terminal emulation.
- **Coordinate** multiple agents working in the same workspace.
- **Remember** — a project-scoped knowledge graph that persists facts,
  patterns and session summaries across sessions.
- **Search** your own documents with a local-first RAG pipeline.
- **Automate** multi-step processes with a DAG graph engine.

Everything persists in an embedded SQLite database under `~/.canopy/` —
no external services, no cloud account.

## The pieces

| Piece | What it does |
|---|---|
| **Daemon** | MCP server (Streamable HTTP + stdio), scheduler, watcher engine, database |
| **Canopy Hub (TUI)** | Full-screen terminal UI for agents, graphs and system metrics |
| **34 MCP tools** | Agent management, harness, intelligence, identity, skills, graphs, crews, config, specs, queues, nodes, RAG, projects, sessions, session shelf, remote control, scheduled sends |
| **Seed identities** | Persistent, evolvable agent personalities stored as TOML |
| **Gamification** | 28 missions across 6 categories tracking usage milestones |

## How the documentation is organized

- [Installation](installation.md) — install and set up the daemon.
- [Quick Start](quickstart.md) — daemon, setup wizard, first agent.
- [The TUI — Canopy Hub](tui.md) — the interactive terminal interface.
- [Agents](agents.md) — interactive, background and terminal agents; seed identities.
- [Intelligence & Sync](intelligence-and-sync.md) — knowledge graph and multi-agent coordination.
- [Graphs](graphs.md) — the DAG graph engine: overview and structure.
- [Template Variables](graphs-templates.md) — placeholders in agent prompts and check commands.
- [Ensembles](graphs-ensembles.md) — parallel, cascade and round-robin member groups, crews, availability.
- [Execution](graphs-execution.md) — commit rights, self-reports, retries, the execution limit, lifecycle, deterministic commits, spec notes.
- [Isolation](graphs-isolation.md) — sandboxes, concurrency, and named mutexes.
- [Hooks and Queues](graphs-hooks-and-queues.md) — event-keyed hooks, the spec backlog, and spec queues.
- [Sharing Graphs](graphs-sharing.md) — export and import, validate, and install from the catalogue.
- [Node Documents](graphs-nodes.md) — reusable node documents.
- [Graph Operations](graphs-operations.md) — archive and restore, node run history, the event log, engine tuning.
- [Graph patterns](https://graphs.univerlab.org/patterns) — the shapes a graph can take, and where each breaks.
- [Graph catalogue](https://graphs.univerlab.org) — ready-made graphs to install.
- [RAG Pipeline](rag.md) — personal document search.
- [MCP Tools](mcp-tools.md) — all 34 tools by category.
- [CLI Reference](cli-reference.md) — every `canopy` command.

## Part of UniverLab

Canopy is an experiment of [UniverLab](https://github.com/UniverLab),
an open computational laboratory. It follows the lab's engineering
principles: one tool one job, reproducibility first, offline-friendly
design.
