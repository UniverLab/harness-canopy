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
curl -fsSL https://raw.githubusercontent.com/UniverLab/harness-canopy/{{TAG}}/scripts/install.sh | sh
```

(`{{TAG}}` is stamped with the release tag by `scripts/release-docs.sh`
before tagging, so the command serves the tagged, immutable install script.)

## Signature verification

The install script verifies the download before installing it. Each release
publishes `SHA256SUMS.txt` (one `<sha256>  <asset>` line per archive) plus a
detached minisign signature, `SHA256SUMS.txt.minisig`, whose trusted comment
is `canopy <tag>`. The script checks the signature with
[minisign](https://jedisct1.github.io/minisign/) against the release public
key:

```text
RWRDOZx1ameWKtWCHDVp3YmIsBiJszM2U4SJbCMkZ8/iSkW9Pi1yWzDD
```

(key id `2A96676A759C3943`), binds it to the tag through the trusted comment,
then checks the archive's SHA-256 against its checksums entry. Any failure
aborts the install: minisign must be installed (the script names it and exits
1 otherwise), and both a release without `SHA256SUMS.txt` and a missing
sha256 tool are fatal, not skipped. To skip signature verification in the
install script (not recommended), set `CANOPY_ALLOW_UNSIGNED=1`; it prints
`WARNING: CANOPY_ALLOW_UNSIGNED=1, skipping signature verification` and still
verifies the checksum. `canopy update` has no such override.

The Linux binary needs glibc 2.39 or newer: Ubuntu 24.04, Debian 13 or later.
On an older system, use the cargo install below.

## Via cargo

```bash
cargo install harness-canopy
```

This build leaves out two patched dependencies that the release binaries carry, because cargo ignores `[patch]` in published crates. Without them, remote control can drop over a relay, so prefer the install script.

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

`canopy setup` is an interactive wizard and needs a terminal: it
shows every registry harness with its install status and dated free-tier
fact, pre-selects the installed ones to configure, and prompts for the
temperature unit (plus RAG settings and skill sync), asks whether to receive
UniverLab announcements (default no; stays off unless you say yes),
then writes `~/.canopy/config.toml`. For installed harnesses with a vendor
login command, `l` opens the login flow in a PTY (Esc cancels) and `r`
retries a failed check. On an install that still holds the
previous STUN fallback, setup prints `STUN fallback is set to the
previous default (stun.l.google.com); change it with canopy config set
remote.stun_fallback`. Its flags are `--local-registry` (use a
local registry directory instead of fetching from GitHub),
`--force-skills` (also remove legacy skill copies that were edited
locally; unedited copies are removed regardless),
`--list-harnesses [--json]` (print every registry harness with its
install status and free-tier fact non-interactively, without running the
wizard), `--install <platform> [--yes]` (run that harness's vendor
installer — see below), `--login <platform>` (run a harness's vendor
login command in a PTY and verify authentication), and
`--check-logins` (check authentication status for every installed
harness). Setup also writes the embedded `canopy` skill as a regular file
to `<skills_dir>/canopy/SKILL.md` for every configured harness whose
skills directory exists (creating it), and refreshes it whenever the
binary's version changes — the same refresh runs on `canopy skill
install`, after `canopy update`, and on every daemon start (which only
refreshes harnesses that already have the file). The other
skills are read live with `canopy skill list` / `canopy skill get`; they
are never copied into a skills directory. Setup removes the copies
earlier versions installed — harness symlinks into `~/.agents/skills/`,
skill directories there that still match the Essential Pack, and the old
`~/.canopy/skills/` store entries — and keeps anything it cannot prove
canopy installed. The same cleanup runs on `canopy skill install`, so
`canopy update` runs it too. When canopy opens an interactive
terminal session for a harness, it also adds a one-line pointer to the
`canopy` skill (inside `<!-- canopy:skill:begin -->` / `<!-- canopy:skill:end -->`
markers) to the user-level instruction file named for that harness in the
platform registry, only when that file already exists on disk (it never
creates the file or modifies repository instruction files). `canopy uninstall`
removes that block from each user instruction file that contains it. It also rewrites legacy unauthenticated canopy MCP entries (the
old URL form, with or without headers) to the stdio `canopy bridge`
form, creating a one-time `<config>.canopy-bak` backup before the first
rewrite of a file. Running `canopy` with no arguments triggers setup
automatically if it has never run. Before or after setup,
[Network and privacy](network-and-privacy.md) lists every outbound
connection canopy makes and how to turn each one off.

If no supported platform CLI is on `PATH`, `canopy setup` prints `No
supported platform CLI detected on PATH.` — meaning no platform is
installed at all, not that an installed one is failing to respond (if a
platform is installed but its binary is not on `PATH`, setup still
cannot see it; to check whether an installed platform actually answers,
run `canopy doctor` — a non-interactive report that needs no terminal —
or `harness probe`). It then lists every supported platform with its CLI
binary name, and exits cleanly without writing any configuration.
Install one of the listed binaries, make sure it is on your `PATH`, then
re-run `canopy setup`.

### Installing a harness from setup

In the harness picker, a missing harness shows what setup can do about
it: `i install` (press `i` to install it), `needs <executable> first`
(the vendor installer needs that tool on `PATH`, and setup never
installs prerequisites), or `install: see <docs_url>` /
`install: not available for this OS` (no installer for your OS).
Pressing `i` shows the vendor name, the exact installer command for your
OS, its `source_url`, and the warning `this runs a command from the
harness vendor on your machine`, then asks `Install? [y/N]` — nothing
runs unless you answer `y`. `--yes` never skips this confirmation in the
interactive list. On success setup re-checks the binary; a harness that
is installed but not on the daemon's `PATH` is reported with the
directory to add, never fixed silently. On failure the row shows
`failed: <exit code>` and the last 20 lines of installer output stay
visible.

The same installer runs non-interactively as `canopy setup --install
<platform>`: it prints the exact command and `source_url` first, then
asks `Install? [y/N]` (declining runs nothing and exits `0`). With
`--yes` it runs without prompting — still printing the command first —
which is required when stdin/stdout is not a terminal (without `--yes`
there it errors out and runs nothing, exiting `1`). Other non-zero exits:
unknown platform, no installer for the running OS, a missing `requires`
executable, or installer failure (the installer's own exit code).

When the registry changes a vendor installer, the refresh holds the new
command in `registry-pending.json` and keeps the approved one: setup
shows `install: pending approval` and will not run the staged command
until you approve it with `canopy registry apply` (or drop it with
`canopy registry discard`).

### Logging in to a harness from setup

In the harness picker, each installed harness checks its login status in
the background (5 s timeout per check) and shows one of: `authenticated`,
`not authenticated yet` (press `r` to retry the check),
`login: not verifiable` (no check command in the registry), or
`login: see <docs_url>` / `login: unknown` (no vendor login command in
the registry).

Pressing `l` on an installed row with a vendor login command prints
`A browser window opens; finish the sign-in there, then press Enter here`
and runs that command in a local PTY with your current environment
(`BROWSER`, `DISPLAY`, `WAYLAND_DISPLAY`, etc.) so the harness opens the
browser itself. Press `Esc` to kill the login process and return to the
list. When the login process exits and a check command is defined, setup
runs the check (5 s timeout) and updates the row to `authenticated` or
`not authenticated yet`. Canopy never reads, logs, or copies
credentials; only the check command's exit code is used.

The same flow runs directly with `canopy setup --login <platform>`
(requires an interactive terminal; exits `1` when stdin/stdout is not a
TTY, on unknown/uninstalled platforms, missing or unapproved login
commands, cancelled/failed login, or failed/timed-out verification, and
`0` on success). `canopy setup --check-logins` checks all installed
harnesses in parallel, prints `<name>: authenticated`,
`<name>: not authenticated yet`, or `<name>: not verifiable`, and exits
`0` when every verifiable installed harness is authenticated (`1` if any
is not). Changed `login_command` and `login_check_command` entries from
the registry are staged in `registry-pending.json` and require
`canopy registry apply` before setup will run them.

The registry itself keeps changing after setup, and the TUI only refreshes it
in the background at startup, and only when the last refresh is more than 24
hours old. To apply the current registry right away without the TUI, run
`canopy registry refresh` — see
[CLI Reference: Platform registry](cli-reference.md#platform-registry).

## Auto-update

The TUI may run a throttled, read-only release lookup in the background
(`maybe_spawn_update_notice`). It is gated by `update.check_enabled`
(set it to `false` and no request is ever made) and
`update.check_interval_hours` (default 24), and it only shows an update
notice — nothing is downloaded and nothing is replaced. Network failures
are silent.

`canopy update` is the only command that downloads and replaces the
executable. It asks `Update to <tag>? [y/N]` first (default no) unless
`--yes` is passed, refuses to overwrite a cargo-installed binary
(printing the `cargo install` command instead), and restarts the daemon
afterwards (asking first when a graph is running).

`canopy update --check` only prints whether an update exists and changes
nothing: exit 0 means up to date, 1 means an update is available, and 2
means the check failed (network, DNS, TLS, HTTP error, or unparsable
response).

Before replacing the executable, `canopy update` verifies the release the
same way the install script does: the `SHA256SUMS.txt.minisig` signature must
be valid for `SHA256SUMS.txt` under the release public key compiled into the
binary, with trusted comment `canopy <tag>`, and the archive's SHA-256 must
match its checksums entry. Any failure aborts before anything is replaced.
Releases before `v3.3.0` carry no signature; updating to one prints
`warning: release <tag> is unsigned; installing without signature
verification` and proceeds. At or after `v3.3.0` a missing signature aborts
with `signature missing: <tag> has no SHA256SUMS.txt.minisig`, and there is
no environment override for `canopy update` (the install script's
`CANOPY_ALLOW_UNSIGNED=1` does not apply to it).

The published Linux binary is `x86_64-unknown-linux-gnu` (glibc, not
static musl — it loads the ONNX Runtime that local RAG embeddings need).
macOS builds are `aarch64-apple-darwin` and `x86_64-apple-darwin` (see
`scripts/install.sh`); `release.yml` builds Linux directly and delegates
the rest to the shared `UniverLab/workflows` release workflow.

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

The generated unit also places `StartLimitIntervalSec=60` and
`StartLimitBurst=5` in `[Unit]` (where systemd reads them) and adds
`After=network-online.target` / `Wants=network-online.target`, so the
restart limit is actually applied and the daemon waits for the network to
be online before it starts. `canopy doctor` warns when an installed unit
keeps its restart limits outside `[Unit]` or lacks the network-online wait,
and reports that the restart limits are not applied until the unit is
updated; run `canopy daemon install-service` to rewrite the unit.

The generated unit also carries `CPUQuota=` from the
`daemon.cpu_quota_percent` setting (half the host's logical CPUs by
default; `0` disables it), so graph work can never saturate every core of
the host — see [CPU quota cap](graphs-isolation.md#cpu-quota-cap).

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

All state lives under `~/.canopy/` (on Unix, created and maintained with mode
`0700` for `~/.canopy/` and `~/.canopy/logs/`, refusing if the state directory
is a symlink; new `background_agents.db` and log files are created `0600`, and
daemon/TUI startup strips group and world permission bits recursively from
existing non-symlink entries without altering the process umask):

| Data | Location | Format |
|------|----------|--------|
| Structured data | `~/.canopy/background_agents.db` | SQLite (WAL mode) |
| Database backup | `~/.canopy/background_agents.db.backup` | SQLite, written by the daily health routine |
| Vector embeddings | `~/.canopy/rag/vectors.lancedb` | LanceDB |
| Terminal history | `~/.canopy/terminals/<name>/history.toml` | TOML |
| Configuration | `~/.canopy/config.toml` | TOML |
| Agent logs | `~/.canopy/logs/<id>.log` | Text (5 MB rotation) |
| TUI log | `~/.canopy/logs/tui.log` | Text (append-only) |
| TUI panic log | `~/.canopy/logs/tui-panic.log` | Text (append-only) |
| Daemon log | `~/.canopy/daemon.log` | Text |

Each `[[clis]]` entry in `~/.canopy/config.toml` may also carry the
infra-retry budget for every agent node/member dispatched on that
platform: `infra_retry_limit` (default 2), `infra_crash_max_seconds`
(default 60) and `infra_backoff_seconds` (default 30). More specific
ensemble/member/node overrides win; see docs/graphs.md.

An entry may also declare a binary identity check: `identity_args` (an
argv array, e.g. `["--version"]`) plus `identity_pattern` (a regex the
combined stdout+stderr must match). When both are set, every surface that
resolves the binary — `harness` `probe`, node dispatch, `agent_edit` `add` /
`agent_edit` `watch` validation and `canopy doctor` — runs
`<binary> <identity_args>` once per path+mtime (5 s timeout) and reports
the platform as **not installed**
(`<platform>: '<path>' is not <name> (identity check failed: …)`) on a
mismatch or timeout, so a wrong program answering to the name (e.g. the
Debian `blackbox` window manager) is never invoked as if it were the CLI.

A platform whose CLI has no headless/print mode (e.g. `freebuff`) declares
`headless = false`. Such a platform stays available for live interactive
sessions and appears in `canopy doctor` and the new-session picker marked
**interactive only**, but canopy refuses to use it anywhere a headless run
is required: `graph_node` `add` / `graph_node` `update`, ensemble members,
`agent_edit` `add`, and `harness` `probe` (reported `interactive_only`, nothing
spawned) all reject it with
`platform '<name>' is interactive-only (no headless mode)`, and
`harness` `models` returns an empty list with that reason instead of
enumerating. `graph_control` `preflight` flags every existing node that uses one.
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
attached). Sessions whose process is already gone are reconciled shut first —
at daemon startup and before each check — so stale rows never block it. A
busy daemon defers it, and after 72 hours of deferral it runs
anyway. `canopy doctor` reports the routine's last outcome, the backup and
its age, and how many times it has been deferred, and warns when it has never
completed. The same routine also prunes storage growth: graph run payloads
older than `retention.graph_runs_days` (default 60) are stripped to metadata,
then deleted past twice that age — runs of graphs that are running, pausing
or paused, and of specs still running, pending or interrupted, are never
touched — and stale sessions older than `clean.retention_days` are removed.
When a prune deletes at least `clean.reclaim_min_deleted_rows` rows the freed
space is reclaimed with `VACUUM`, and `canopy doctor` prints the current row
counts with the active retention settings on one `Retention:` line. To run the check and backup now:

```bash
canopy daemon health-check
```

When the database file itself is damaged, `canopy daemon start` fails
before doing anything and names the verified backup plus the restore
command; the damaged file is never modified. To restore it (stop the
daemon first):

```bash
canopy doctor --restore [--from <file>]
```

The damaged file is renamed aside to `<name>.damaged-<UTC timestamp>`
(never deleted), the backup is copied into place, and the restored file is
verified before the command reports success.
For platforms, it confirms each configured binary is present and
executable on `PATH`, and — when the entry declares `identity_args` /
`identity_pattern` — that the binary really is that program; a wrong
program is reported as not installed. It does **not** check that you're
logged in. Check that per platform, e.g. `claude auth status` or
`codex login status`.
