---
title: Quick Start
description: Start the daemon, run the setup wizard, and launch your first agent.
order: 3
---

# Quick Start

## 1. Start the daemon

```bash
canopy daemon start
```

The daemon owns the MCP server (Streamable HTTP on port 7755 + stdio),
the cron scheduler, the file watcher engine and the database.

## 2. Configure (first time)

```bash
canopy setup
```

`canopy setup` is an interactive wizard — run it in a terminal. It
prompts you to select your AI CLI platforms (Claude Code, and others
from the registry), the temperature unit, and RAG settings, asks
whether to receive UniverLab announcements (default no), then writes
`~/.canopy/config.toml`. Setup also rewrites any legacy unauthenticated
canopy MCP entry (the old URL form) to the stdio `canopy bridge` form,
keeping a one-time `<config>.canopy-bak` backup.

If no supported platform CLI is on `PATH`, setup prints `No supported
platform CLI detected on PATH.`, lists every supported platform with its
CLI binary name, and exits cleanly without writing any configuration —
install one, make sure it is on your `PATH`, and re-run `canopy setup`.

## 3. Launch the TUI

```bash
canopy
```

Opens the full-screen **Canopy Hub**. Press `Ctrl+N` (or `n`) to create a
new agent:

- **Interactive** — a live PTY session with your chosen AI CLI.
- **Background** — cron-scheduled or file-watcher-triggered.
- **Terminal** — a raw shell session with per-session history.

Choose the platform, model, and working directory for the session.

## 4. Wire up your AI CLI via MCP

```bash
canopy mcp
```

The MCP wizard syncs, adds and removes canopy's MCP server entry across
all detected platforms, converting formats automatically (JSON ↔ TOML).
`Add` offers three input modes: **Paste JSON** (the snippet from the
server's README, installed verbatim under its own name), **Local command
(stdio)** (command with its arguments plus optional `KEY=VALUE` env vars),
and **Remote URL (http)** (URL plus optional `Name=Value` HTTP headers).
Entries are previewed before writing — overwrites are marked
`(replaces existing)` — and one confirm installs them everywhere.
See the [CLI reference](cli-reference.md) for details.
After that, your AI agents can call all
[36 MCP tools](mcp-tools.md) — schedule background agents, store
knowledge, coordinate with each other.

## 5. Run your first graph

No AI CLI is needed for this: a single `check` node runs `echo hello`
and finishes. Follow [Your First Graph](first-graph.md) — start the
daemon, validate and import one JSON file, run it with `--idea`, and
read its event log.

## 6. Verify

```bash
canopy doctor          # full health diagnostics
canopy daemon status   # is the daemon running?
canopy daemon logs     # tail the daemon log
```

## Where to go next

- [Agents](agents.md) — the three agent kinds.
- [Intelligence & Sync](intelligence-and-sync.md) — persistent knowledge.
- [Graphs](graphs.md) — multi-step automation.
- [RAG Pipeline](rag.md) — index and search your documents.
