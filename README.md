<p align="center">
  <img src="https://raw.githubusercontent.com/UniverLab/.github/main/assets/banner-canopy.svg" alt="Canopy"/>
</p>

<p align="center">
  <a href="https://github.com/UniverLab/harness-canopy/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/UniverLab/harness-canopy/ci.yml?branch=main&style=for-the-badge&label=CI" alt="CI"/></a>
  <a href="https://crates.io/crates/harness-canopy"><img src="https://img.shields.io/crates/v/harness-canopy?style=for-the-badge&logo=rust&logoColor=white" alt="Crates.io"/></a>
  <img src="https://img.shields.io/badge/Status-Active-27AE60?style=for-the-badge" alt="Status"/>
  <a href="LICENSE-MIT"><img src="https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-2E8B57?style=for-the-badge" alt="License"/></a>
</p>

harness-canopy is a modern, self-contained MCP (Model Context Protocol) server and TUI for orchestrating AI agent sessions, background tasks, and file event triggers. Designed for reliability, modularity, and performance — it enables advanced scheduling, persistent knowledge graphs, multi-agent coordination, graph automation, and interactive terminal management with zero runtime dependencies.

---

### Demo

![Demo](demo/dist/demo.svg)

---

## Requirements

canopy orchestrates AI agent tools — it doesn't ship one. Before installing,
you need at least one supported **platform installed and authenticated**
on this machine. "Installed" is not enough: the most common first failure
is a platform that's present but not logged in, and canopy has no way to
tell you apart from "canopy is broken."

canopy calls each supported AI tool a **platform**. Most ship as a
standalone terminal command (`claude`, `codex`, `gemini`, ...); a few —
Cursor, Antigravity, Qoder — ship as a code editor with one built in.
canopy drives either kind the same way, through the CLI binary each one
exposes, so editors show up in the same list as terminal tools.

### Supported platforms

| Platform | Binary | Ships as |
|---|---|---|
| claude | `claude` | terminal (Claude Code) |
| codex | `codex` | terminal (Codex CLI) |
| gemini | `gemini` | terminal (Gemini CLI) |
| copilot | `copilot` | terminal (GitHub Copilot CLI) |
| qwen | `qwen` | terminal (Qwen Code) |
| mistral | `vibe` | terminal (Mistral's Vibe CLI) |
| opencode | `opencode` | terminal (OpenCode) |
| kiro | `kiro-cli` | terminal (Kiro) |
| kilo | `kilo` | terminal (Kilo Code) |
| cline | `cline` | editor (Cline, VS Code) |
| continue | `cn` | editor (Continue, VS Code/JetBrains) |
| blackbox | `blackbox` | terminal (Blackbox AI CLI) |
| mimocode | `mimo` | terminal (Mimo Code) |
| cursor | `agent` | editor (Cursor) |
| antigravity | `agy` | editor (Antigravity) |
| qoder | `qoder` | editor (Qoder) |

This list is drawn from the live [platform registry](https://github.com/UniverLab/canopy-registry)
(`index.toml`); `canopy setup` re-fetches it, so newly added platforms show
up there before they show up here.

### Check that a platform is usable

```bash
canopy doctor
```

Its "Harnesses" line confirms each configured platform's binary is present
and executable on `PATH` — **it does not check that you're logged in.**
There's no single command across all 16 platforms for that; check the
platform's own way, e.g. `claude auth status` or `codex login status`. Most
tools will otherwise tell you the moment you actually run them for real.

### If no platform is installed

`canopy setup` detects nothing, lists every supported platform with its
CLI binary name, and exits cleanly without writing any configuration.
Install one of the listed binaries, make sure it's on your PATH, then
re-run `canopy setup`.

---

## Installation

**Linux / macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/UniverLab/harness-canopy/main/scripts/install.sh | sh
```

Or via cargo: `cargo install harness-canopy` (the binary is `canopy`).
See [`docs/installation.md`](docs/installation.md) for all methods and first-time setup.

## Documentation

Full documentation lives in [`docs/`](docs/): installation, quick start, the
TUI, agents and seed identities, intelligence & sync, graphs, the RAG
pipeline, all MCP tools, and the complete CLI reference.

## Contributing

docs/ is published at univerlab.org/canopy/docs and holds user documentation only; design records are kept outside the repository.

---


## Features

### 🎯 Core Platform

- **🚀 High-Performance Scheduler** — Event-driven cron scheduler using Tokio with zero polling overhead. Computes precise wake-up times and sleeps until needed; CPU usage drops to near-zero when idle.
- **📊 Real-time File Watcher** — Instantly reacts to file system events (create, modify, delete, move) using the `notify` crate with configurable debouncing, recursive directory monitoring, and macOS FSEvents compatibility.
- **💾 Persistent State** — All tasks, watchers, execution logs, agent state, sync messages, intelligence nodes, graphs, and projects are stored in an embedded SQLite database with WAL mode and automatic schema migration.
- **🔄 Auto-Update** — `canopy update` checks GitHub releases for a newer stable version, asks first (default no), refuses cargo installs, and restarts the daemon. `canopy update --check` exits 0 (up to date), 1 (update available), or 2 (check failed).
- **🔔 Cross-Platform Notifications** — Native desktop notifications for task completions, failures, and watcher triggers. Auto-detects platform: WSL (PowerShell toasts with AUMID), macOS (`osascript`), Linux (`notify-send`).

### 🤖 Agent Management

- **Interactive PTY Agents** — Each agent runs in a dedicated pseudo-terminal with full vt100 emulation, 24-bit color support, cursor positioning, and interactive applications.
- **Terminal Sessions** — Raw shell sessions with per-session command history (TOML-backed), cross-session autocomplete search, and Warp-like input mode for efficient command entry.
- **Background Agents** — Cron-scheduled and file-watcher-triggered agents with configurable timeouts, automatic retries, execution logging (5 MB rotation), and per-run status tracking.
- **Platform Liveness Probe** — `harness` `probe` tests whether a platform+model pair can actually respond before the `graph_control` run action spends real quota on it. Probes the exact pair a node would use, not just the platform default.
- **Seed Identity System** — Persistent, evolvable agent identities stored as structured TOML at `~/.canopy/seeds/<id>/identity.toml`. Each seed has a unique name, family, behavioral directives, and personality traits. Binded sessions receive the seed's prompt injection automatically. The `identity` `evolve` action lets agents refine themselves over time. 4 KB size cap, case-insensitive name uniqueness, and mandatory field validation.
- **Seed Nursery** — Collaborative workspace for creating new seed identities. Creates a temporary directory with a draft `identity.toml` and CLI-specific instruction files (e.g. `CLAUDE.md`, `AGENTS.md`) that guide the agent to interview the user and define the seed's personality. Validates and registers the seed on completion.
- **Context Transfer** — Seamlessly transfer conversation context, prompts, and output between agents while preserving session state and scrollback history.
- **Prompt Builder** — Structured prompt templates with configurable sections (instruction, context, resources, examples, Canopy protocol), section picker, and @-mention agent references. The session-start Canopy protocol rides on the first prompt of an interactive session only — persisted per session across restarts and resumes — and the locked **Canopy protocol** section (`Ctrl+A`) resends it on purpose.
- **Launchpad** — Start new interactive sessions with previous mission recovery, mission input, and auto-injected context.

### 🧠 Intelligence V2 — Knowledge Graph

- **Project-Scoped Knowledge** — Store facts, patterns, and session summaries scoped to individual projects via `project_hash` (SHA-256 of canonical workdir path, truncated to 8 hex). Auto-detected from the session workdir — agents never need to set it manually.
- **Full-Text Search** — Search intelligence nodes by query and optional kind filter across all projects.
- **Graph Walk** — Traverse the knowledge graph from any node up to a configurable depth, returning connected facts, patterns, and cross-references.
- **Project Relationships** — Link projects with typed relations (`depends_on`, `complements`, `extends`, `publishes`; `relates_to` accepted as legacy, `contains` derived) and query related projects for context enrichment.
- **Context Retrieval** — `intelligence_get_context` auto-detects the project, returns a curated mix of session knowledge, project facts, and related-project summaries.

### 🔄 Multi-Agent Coordination

The sync protocol tools are retired. Sessions coordinate through
`session_list` (see every live session), `scheduled_send_create` (wake a
session later instead of polling), and `intelligence_upsert` /
`intelligence_search` (persist and find shared findings).

### 🔀 Graph DAG Engine

- **Ordered Specs** — Graphs contain sequenced `Spec`s, each with `Node`s connected by `Edge`s with routing conditions (`pass`/`fail`/`always`).
- **Standalone Spec Backlog** — Specs exist independently from graphs; tag them to a workdir for filtering. Managed via `spec` (`list`, `get`) and `spec_edit` (`create`, `update`, `delete`).
- **Spec Queues** — Ordered queues of existing specs that a graph drains one by one. Append and reorder while a graph is running via `queue` (`create`, `add_spec`, `list`, `remove_spec`, `reorder`, `set_group`).
- **Node Kinds** — `agent` nodes invoke CLI tools with prompt templates; `check` nodes execute shell commands; `gate` nodes validate previous output (e.g., `output_contains`); `router` nodes classify and route by token matching with fallback; `quorum` is the engine-managed node that closes an ensemble.
- **Ensembles** — `graph_ensemble` `add` creates a parallel group of 2-8 agent-node members sharing one prompt (or per-member prompt overrides for specialist panels), plus a wait-all quorum that consolidates their outputs and routes onward, in a single MCP call. `graph_ensemble` `update` edits the shared prompt, member list, quorum/exit config, and entry wiring (including multi-source entry and chaining a quorum into another ensemble) as one unit; `graph_ensemble` `delete` removes the whole unit. See [docs/graphs-ensembles.md](docs/graphs-ensembles.md#ensembles).
- **Node Documents** — Reusable versioned `{name, kind, config}` templates installed from the catalogue (`node` `install`) and referenced by name in `graph_node` `add`. Six builtins are embedded in the binary; list them with `node` `list`.
- **Full Lifecycle** — Create, update, run, pause, continue (retry or skip), reset, complete nodes, and report blockers for human intervention. `graph_control` `schedule_autorun` resumes failed/completed graphs at a future time. Archive and restore graphs without losing history. Export and import graph designs as shareable JSON documents, and validate one — graph or single node — before installing it (`graph` `validate`, `canopy graph validate`). Copy nodes and ensembles within or across graphs. View node run history for failure diagnosis.
- **Event-keyed Hooks** — Agent execution, direct shell commands, or interactive messages into a live session on `on_completed`, `on_failed`, `on_blocked`, and `on_spec_completed` (e.g. documentation maintenance fires once per completion; a failing graph at midnight puts a message in the operator's session, delivered when they next open the TUI). Several hooks can serve one event in order; hooks are not retroactive and never change the graph's status.
- **Template Variables** — Graph prompts support `{{graph_name}}`, `{{workdir}}`, `{{spec_id}}`, `{{spec_name}}`, `{{spec_content}}`, `{{spec_code}}`, `{{spec_type}}`, `{{spec_scope}}`, `{{spec_commit_message}}`, `{{node_id}}`, `{{previous_feedback}}`, `{{output:<node>}}` (another node's recorded output, by node name), and `{{spec_start_head}}` (git HEAD at spec start, for check nodes). A prompt template carrying a placeholder nothing can bind — including `{{output:<name>}}` that names an ensemble rather than a node — is refused when the graph is written, not when a member tries to spawn.
- **MCP Tools** — Complete authoring, inspection, runtime, spec, queue, and node-document management.

### 📚 Personal RAG Pipeline

- **Semantic Search** — Embed and query personal documents (Markdown, MDX, PDF) with local ONNX models (fastembed/BGE, multilingual-e5). No cloud provider, no API key.
- **Language-Aware Chunking** — Markdown split by headings with paragraph fallback, similarity-aware merging, and overlap for context preservation.
- **PDF Extraction** — Isolated subprocess prevents parser crashes from taking down the daemon, with HTML detection and raw-text salvage fallback.
- **Auto-Ingestion** — Background watcher monitors configured RAG roots with 3-second debounce, enqueues changes, and reconciles orphan chunks on startup.
- **Per-Agent Rate Limiting** — 10 calls/minute sliding window per agent.
- **`.canopy/ragignore`** — Regex-based exclusion patterns for files and directories.

### 🖥️ Interactive TUI (Canopy Hub)

- **NewAgentDialog** — Three modes (Interactive, Background Cron/Watch, Terminal) with CLI picker, model picker, seed identity selector (`◀ None / SeedName ▶`), directory browser, and yolo mode toggle.
- **Detachable Sessions** — Sessions run in the daemon, not the TUI: quitting the TUI leaves them running, reopening it reattaches to them live with screen and scrollback, a daemon restart respawns every session still listed as active under the same id and name (a terminal comes back with its saved scrollback replayed), sessions started or closed elsewhere update the sidebar live, and a dropped daemon connection reconnects on its own (0.5 s doubling to 5 s) and re-lists what the daemon still has.
- **Scheduled Delivery** — `Shift+Enter` (Kitty keyboard protocol) sends prompts at a chosen time instead of immediately.
- **Session Shelf** — an agent copies up to 10 files per call (PNG, JPEG, WebP, GIF or SVG, sniffed from content, 20 MiB each) onto its session's shelf with the `session_send_file` MCP tool; the F1 hub's `Shelf` tab lists every shelf newest first, toasts each arrival as `📎 <name> — <from>`, and opens, copies the path of, or deletes the selected file, with `canopy shelf list`/`open` reaching the same files from the CLI.
- **Split Groups** — Side-by-side horizontal/vertical views for monitoring multiple agents simultaneously.
- **System Dashboard** — CPU, memory, disk, GPU (NVIDIA/Linux/macOS), temperatures with amber/red alert thresholds. WSL queries Windows host metrics via PowerShell. Where ACPI thermal zones report nothing, run LibreHardwareMonitor (WMI on) to expose CPU temperature.
- **Live Graph View** — Real-time graph rendering with auto-follow on the running node, manual node inspection, and per-node run info (status, elapsed, output tail).
- **Agent Status Colors** — Green for working (recent output), blue for idle, red for failed, gray for exited.
- **Projects Sidebar** — Sections for active graphs, backlog specs, and graph history, filterable by project workdir.
- **Graph Editor** — Inline node config JSON editing with validation.
- **RAG Transfer Modal** — Send semantic search results to other agents as injected context.
- **Context Transfer** — Two-step modal (preview → agent picker) to inject conversation context between sessions.
- **Brian's Brain** — 3-state cellular automaton drawn as Braille glyphs; the cursor seeds clusters along its path at a fixed step rate.
- **Whimsg** — Animated kaomoji status messages with typing effects: curated forest-voice phrases and deliberate jokes, or a real-event message naming the agent, graph, or spec that just happened.
- **Gamification** — 28 achievement-style missions across 6 categories (Environment, Intelligence, Projects, Graph, Seeds, SysInfo) tracked automatically during normal TUI operation; list them read-only with `canopy missions [--json]`.

### 🔧 Additional Features

- **Prompt Template Engine** — Background agent prompts support `{{TIMESTAMP}}`, `{{TASK_ID}}`, `{{LOG_PATH}}`, `{{FILE_PATH}}`, `{{EVENT_TYPE}}` placeholders.
- **Project Registry** — Automatic project index keyed by workdir hash. README descriptions extracted from project roots (min 20 words, before first `##` heading). Search and update via the `project` MCP tool (`search`, `update`).
- **Action Protocol Advisor** — `get_tools` MCP tool returns scope-sensitive action protocols (`session_start`, `file_write`, `test_run`, `close_session`, `multi_agent`) with risk levels and recommended tool sets.
- **Remote Access** — `canopy remote login` signs this host in to GitHub with the OAuth device flow (no browser on the host, no scopes), printing the code to enter at github.com/login/device and storing `{token, login, id}` in `~/.canopy/remote/github.json` (mode 0600); `canopy remote status [--json]` reports the signed-in login, whether remote control is enabled, and the relay URL, and `canopy remote logout` deletes the identity. `canopy remote on`/`off` opens or closes the daemon's relay WebSocket (P-256 host key in `~/.canopy/remote/host_key`), `canopy remote pair` prints a one-time `XXXXX-XXXXX` code with its `<relay_url>/pair#<code>` link and a terminal QR code so a browser pairs once for ten minutes, `canopy remote devices [--json]` lists the paired browsers, `canopy remote revoke <device_id>` deletes one and drops its live connection, and `canopy remote disconnect <device_id>` drops one's live connection while keeping the pairing. A paired browser then reaches the session hub directly over a signed WebRTC data channel — the relay only carries signaling — where terminal sessions stay host-only and `resize` is refused. The token never appears in logs, errors, or status output. The F1 hub's `Remote` tab (`F1`, then `5`) offers the same from the TUI.
- **Setup Wizard** — Interactive `canopy setup` detects installed AI CLIs from a GitHub-hosted registry, configures binary paths, model flags, headless modes, environment variables, and temperature units. Generates `~/.canopy/config.toml`.
- **On-Demand Registry Refresh** — `canopy registry refresh` applies the platform registry to `config.toml` now, ignoring the 24-hour window the TUI's background refresh uses at startup. Reports the CLI fields it changed, the local overrides it kept, and the platforms added, updated or removed. The `harness` `refresh_registry` action does the same from an agent session.
- **MCP Wizard** — `canopy mcp` subcommand for syncing, adding, and removing MCP server entries across all detected platforms with automatic format conversion (JSON ↔ TOML).
- **Skills Manager** — Global skill directory at `~/.agents/skills/` with cross-platform symlinks. List, validate symlink integrity, and remove installed skills across platforms.
- **Doctor Diagnostics** — `canopy doctor` checks data directory, database, config, harnesses, RAG status, file watchers, daemon process, registry connectivity, and auto-update health.
- **Desktop Notifications** — Cross-platform alerts for task completions, failures, watcher triggers, RAG indexing events, and graph lifecycle (started, spec completed, finished with outcome, blocker, hook failure).

---

## MCP Tools (34)

Graph-node sessions (agent and ensemble nodes launched by the graph engine) receive a fixed node profile of 12 tools via `CANOPY_MCP_PROFILE=node`, so every node stays well under the 128-tool cap of OpenAI-compatible providers. All other sessions — the orchestrator, check nodes, hooks, TUI, subagents, and background agents — keep the full tool list.

| Tools | Category |
|---|---|
| `agent`, `agent_edit`, `agent_report`, `subagent` | **Agent management** (4) |
| `harness` | **Harness** (1) |
| `intelligence_get_context`, `intelligence_upsert`, `intelligence_search`, `intelligence_admin` | **Intelligence** (4) |
| `identity` | **Identity** (1) |
| `skill` | **Skills** (1) |
| `graph`, `graph_edit`, `graph_node`, `graph_edge`, `graph_ensemble`, `graph_control`, `graph_complete_node`, `graph_report_blocker`, `sandbox` | **Graph engine** (9) |
| `crew` | **Crews** (1) |
| `config` | **Configuration** (1) |
| `spec`, `spec_edit` | **Spec backlog** (2) |
| `queue` | **Spec queues** (1) |
| `node` | **Node documents** (1) |
| `project` | **Project** (1) |
| `rag` | **RAG** (1) |
| `get_tools` | **Protocol** (1) |
| `session_list` | **Interactive sessions** (1) |
| `scheduled_send_create`, `scheduled_send` | **Scheduled sends** (2) |
| `session_send_file` | **Session shelf** (1) |
| `remote` | **Remote control** (1) |

---

## Architecture Overview

- **Daemon** — Owns the MCP server (Streamable HTTP on port 7755 + stdio), scheduler, watcher engine, and database. Exposes all MCP tools.
- **Scheduler** — Computes next fire times for all active tasks, sleeping until needed. Wakes instantly on changes.
- **Watcher Engine** — Reacts to file system events, triggering tasks as defined.
- **Executor** — Runs tasks and agents, manages locking, logs, and status.
- **Intelligence V2** — Project-scoped knowledge graph with node/edge CRUD, deletion tools, graph walk, project relationships.
- **Sync History** — Durable mission and chatter store that `intelligence_get_context` surfaces and the TUI renders; the live broadcast protocol is retired.
- **Graph Engine** — DAG execution engine: agent/check/gate/router node runners, iteration limits, pause/continue, blocker reporting, spec queues, node documents, scheduled autorun, archive/restore, export/import, node run history, and copy tools.
- **RAG Pipeline** — Background ingestion, language-aware chunking, embedding client, vector store, and rate-limited search.
- **TUI** — Full-screen ratatui terminal UI for managing agents, viewing output, graphs, and system metrics in real time.
- **Gamification** — Mission tracker with 28 achievements across 6 categories, persisted in the database.
- **Skills Manager** — Global skills directory with cross-platform symlinks and integrity validation.

---

## Main Modules

- `application/` — Application ports and abstractions
- `autoupdate/` — Self-update system (GitHub releases)
- `daemon/` — MCP server, handler, params, RAG CLI, doctor
- `db/` — SQLite persistence and migrations (agents, runs, sessions, sync, intelligence, graphs, projects, seeds, groups, state)
- `domain/` — Core models: Agent, Trigger, WatchEvent, Project, SeedIdentity, Graph, GraphSpec, Queue, GraphNode, SyncMessage, IntelligenceNode
- `executor/` — Task and agent execution logic
- `rag/` — RAG pipeline (ingestion, chunking, embedding, vector store, rate limiting)
- `scheduler/` — Internal cron scheduler with template variables
- `tui/` — Terminal UI: agent management, dialogs, sidebar, system dashboard, context transfer
- `watchers/` — File system watcher engine
- `graph_engine/` — DAG graph execution engine

---

## Data Storage

| Data | Location | Format |
|------|----------|--------|
| Structured data | `~/.canopy/background_agents.db` | SQLite (WAL mode) |
| Vector embeddings | `~/.canopy/rag/vectors.lancedb` | LanceDB |
| Seed identities | `~/.canopy/seeds/<id>/identity.toml` | TOML |
| Terminal history | `~/.canopy/terminals/<name>/history.toml` | TOML |
| Configuration | `~/.canopy/config.toml` | TOML |
| Agent logs | `~/.canopy/logs/<id>.log` | Text (5 MB rotation) |
| Daemon log | `~/.canopy/daemon.log` | Text |
| RAG indexed files | `~/.canopy/rag/` | Markdown, PDF |

---

## Usage

1. **Start the daemon:**
   ```bash
   canopy daemon start
   ```

2. **Configure (first time):**
   ```bash
   canopy setup
   ```
   Choose AI CLI platforms, model flags, temperature units, and RAG settings. Generates `~/.canopy/config.toml`.

3. **Launch the TUI:**
   ```bash
   canopy
   ```
   Opens the full-screen Canopy Hub. Press `Ctrl+N` or `n` to create a new agent (Interactive, Background, or Terminal). Select a seed identity to give the agent a persistent personality.

4. **Check health:**
   ```bash
   canopy doctor
   ```

5. **Manage MCP servers across platforms:**
   ```bash
   canopy mcp
   ```

6. **RAG indexing:**
   ```bash
   canopy rag auto-index start   # Start background watcher
   canopy rag auto-index stop    # Stop
   canopy rag report             # Per-file indexing report
   ```

7. **Install a graph from the catalogue:**
   ```bash
   canopy graph install my-graph   # Review the commands and prompts it runs, confirm, then import it as a new local graph
   ```

All state persists in `~/.canopy/`. The TUI shows an update notice at most every 24 hours — `canopy update` installs it.

---

## Tech Stack

|Rust 2021| Tokio | Axum | rusqlite | LanceDB | notify | vt100 | ratatui | clap | serde | tracing | fastembed |

---

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. Releases up to 3.0.1 were published under MIT only.

---

An experiment of [UniverLab](https://github.com/UniverLab) — an open computational laboratory.
Made with ❤️ by [JheisonMB](https://github.com/JheisonMB)