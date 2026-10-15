---
title: Installation
description: Install canopy, run the setup wizard, and keep it updated automatically.
order: 2
---

# Installation

## Requirements

canopy orchestrates AI agent tools — it doesn't ship one. You need at
least one supported **platform installed and authenticated** first (a
terminal AI CLI like `claude`/`codex`/`gemini`, or an editor that ships
one, like Cursor/Antigravity/Qoder). See the
[Requirements section in the README](../README.md#requirements) for the
full platform list and how to check a platform is usable.

## Quick install

**Linux / macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/UniverLab/harness-canopy/main/scripts/install.sh | sh
```

## Via cargo

```bash
cargo install harness-canopy
```

The crate is `harness-canopy`; the installed binary is **`canopy`**.
Available on [crates.io](https://crates.io/crates/harness-canopy).

## From source

```bash
git clone https://github.com/UniverLab/harness-canopy.git
cd harness-canopy
cargo build --release
# Binary at target/release/canopy
```

## First-time setup

```bash
canopy setup
```

The interactive wizard detects installed AI CLI platforms from a
GitHub-hosted registry, configures binary paths, model flags, headless
modes, environment variables and temperature units, and generates
`~/.canopy/config.toml`. Running `canopy` with no arguments triggers
setup automatically if it has never run.

If no supported platform is on `PATH`, setup is meant to say so, list
what it supports, and finish with zero platforms configured. It currently
doesn't reach that message — see
[Requirements: If no platform is installed](../README.md#if-no-platform-is-installed)
in the README for the verified failure and where it comes from.

## Auto-update

The daemon checks GitHub releases for new stable versions every 24 hours,
downloads the platform-specific binary (linux-musl, macos-darwin;
x86_64/aarch64) and atomically replaces the running executable.

The check is tunable: `update.check_interval_hours` (default 24) sets the
interval, and `update.check_enabled = false` turns the check off so no
request is made. The manual `canopy update` command is not affected.

## Running as a service

```bash
canopy daemon install-service     # register with systemd/launchd
canopy daemon uninstall-service
```

`canopy daemon install-service` (and first-time `canopy setup` when it
installs the unit) copies `BROWSER`, `DISPLAY` and `WAYLAND_DISPLAY` —
only when set in the installing shell, verbatim, with whitespace quoted —
into the systemd unit. Re-running preserves previously stored values for
keys missing from the current environment. Hand drop-ins under
`~/.config/systemd/user/canopy.service.d/` are never touched. `canopy
doctor` warns when the unit lacks `BROWSER`.

The generated unit also sets `OOMPolicy=continue` and `MemoryMax=80%`, so a
process spawned by a graph node that exhausts memory is killed alone instead
of taking the daemon (and every other running graph) down with it. `MemoryMax`
is a percentage of the machine's physical memory, so the unit stays portable.
`canopy doctor` warns when an installed unit still keeps `OOMPolicy=stop`
(the systemd default for a unit written before this), naming the reinstall
that fixes it.

`canopy daemon start` and `canopy daemon restart` compare the unit's
`ExecStart` with the `canopy` binary running the command. When it names a
different or already-removed binary — a unit left behind by an older
`cargo install`, or one the installer moved — the unit is re-rendered for the
current binary, keeping the `PATH` and `BROWSER`/`DISPLAY`/`WAYLAND_DISPLAY`
values it already carries, followed by `systemctl --user daemon-reload` and
`systemctl --user reset-failed`. `canopy daemon restart` then restarts the
installed unit with a single `systemctl --user restart`.

When the daemon still never answers its port, the error names the unit's state
rather than only pointing at the logs: an `ExecMainStatus` of `203` reports
`ExecStart binary not executable or missing`, and any other value reports
`ActiveState`, `SubState`, `ExecMainStatus` and `Result` verbatim.

## Data directory

All state lives under `~/.canopy/`:

| Data | Location | Format |
|------|----------|--------|
| Structured data | `~/.canopy/background_agents.db` | SQLite (WAL mode) |
| Database backup | `~/.canopy/background_agents.db.backup` | SQLite, written by the daily health routine |
| Vector embeddings | `~/.canopy/rag/vectors.lancedb` | LanceDB |
| Seed identities | `~/.canopy/seeds/<id>/identity.toml` | TOML |
| Terminal history | `~/.canopy/terminals/<name>/history.toml` | TOML |
| Configuration | `~/.canopy/config.toml` | TOML |
| Agent logs | `~/.canopy/logs/<id>.log` | Text (5 MB rotation) |
| Daemon log | `~/.canopy/daemon.log` | Text |

Each `[[clis]]` entry in `~/.canopy/config.toml` may also carry the
infra-retry budget for every agent node/member dispatched on that
platform: `infra_retry_limit` (default 2), `infra_crash_max_seconds`
(default 60) and `infra_backoff_seconds` (default 30). More specific
ensemble/member/node overrides win; see docs/graphs.md.

An entry may also declare a binary identity check: `identity_args` (an
argv array, e.g. `["--version"]`) plus `identity_pattern` (a regex the
combined stdout+stderr must match). When both are set, every surface that
resolves the binary — `agent_probe`, node dispatch, `agent_add` /
`agent_watch` validation and `canopy doctor` — runs
`<binary> <identity_args>` once per path+mtime (5 s timeout) and reports
the platform as **not installed**
(`<platform>: '<path>' is not <name> (identity check failed: …)`) on a
mismatch or timeout, so a wrong program answering to the name (e.g. the
Debian `blackbox` window manager) is never invoked as if it were the CLI.

A platform whose CLI has no headless/print mode (e.g. `freebuff`) declares
`headless = false`. Such a platform stays available for live interactive
sessions and appears in `canopy doctor` and the new-session picker marked
**interactive only**, but canopy refuses to use it anywhere a headless run
is required: `graph_add_node` / `graph_update_node`, ensemble members,
`agent_add`, and `agent_probe` (reported `interactive_only`, nothing
spawned) all reject it with
`platform '<name>' is interactive-only (no headless mode)`, and
`agent_models` returns an empty list with that reason instead of
enumerating. `graph_preflight` flags every existing node that uses one.
Omit the field — the default is `true` — and the platform behaves exactly
as before.

## Health check

```bash
canopy doctor
```

Checks the data directory, database, config, harnesses, RAG status, file
watchers, daemon process, registry connectivity and auto-update health.

The database has a daily routine: a full integrity check and foreign-key
check, then one verified backup written with `VACUUM INTO`, replacing the
previous backup. It runs only while the daemon is idle (no graph running, no TUI
attached). A busy daemon defers it, and after 72 hours of deferral it runs
anyway. `canopy doctor` reports the routine's last outcome, the backup and
its age, and how many times it has been deferred, and warns when it has never
completed. To run the check and backup now:

```bash
canopy daemon health-check
```
For platforms, it confirms each configured binary is present and
executable on `PATH`, and — when the entry declares `identity_args` /
`identity_pattern` — that the binary really is that program; a wrong
program is reported as not installed. It does **not** check that you're
logged in. Check that per platform, e.g. `claude auth status` or
`codex login status`.
