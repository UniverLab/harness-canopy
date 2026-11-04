---
title: Isolation
description: Sandboxes, concurrency, and named mutexes.
order: 11
---

# Isolation

## Sandboxes

`graph_control` `run` accepts `sandbox: true`: canopy creates a temporary git worktree
under `~/.canopy/worktrees/<project-hash>/<sandbox-id>`, on a new
`canopy/sandbox-*` branch cut from the workdir's current branch, and runs
every node of that run inside it — your checkout is never edited. The canopy
protocol goes into the worktree's instruction file rather than the prompt.

A sandbox is temporary, but never at the cost of work:

- A **completed** run removes its sandbox only when it provably holds
  nothing: no commits that exist nowhere else **and** no uncommitted path —
  tracked or untracked — other than the instruction file canopy itself wrote.
  A sandbox holding anything is kept, and the reason is recorded with it
  (how many uncommitted paths, and the first 20).
- A **failed** graph never tears its sandbox down, whatever it holds. Only a
  completed run cleans up; landing, continuing or discarding a failed run's
  sandbox is your call.
- When the check itself fails — git cannot say whether the worktree is dirty
  — the sandbox is kept too. Uncertain means keep, like an unknown commit
  count.

`graph_control` `continue` on a failed sandboxed graph resumes **inside the kept
worktree**, so a retry keeps the work that is already there. If that worktree
is gone, the continue is refused, naming the sandbox id, instead of quietly
running the graph against your original workdir.

Leftover sandboxes are dealt with explicitly, never guessed at, and both
surfaces share one implementation: the [`sandbox` MCP tool](mcp-tools.md)
(`list`, `get`, `land`, `discard`), or the commands in the
[CLI reference](cli-reference.md#sandboxes). `graph` `get` reports the
graph's newest sandbox as `{id, status}` (`null` when it never ran
sandboxed), so an agent can name the sandbox it is about to land or
discard instead of inventing an id.

## Concurrency

Running many graphs at once, against different working directories, is
a supported capability — not an accident of the implementation. Start,
run, pause, resume, or finish one graph, and no other graph's status,
specs, node runs, or worktree are affected. There is no cap on how
many graphs can run concurrently, and nothing needs to be configured to
enable it: it is the default behavior of the daemon.

The one boundary: **two graphs must not share a workdir.** Two graphs
racing to commit, check out, or edit files in the same working tree
will fight over it — the engine does nothing to make that safe, and
doing so is deliberately out of scope. Point concurrent graphs at
different working directories (or at worktrees of the same repo) and
they run independently with no coordination required from you.

## Named mutexes

Some nodes saturate a shared resource — a gate that compiles the whole
workspace wants the entire CPU, a future GPU node wants the only GPU.
A node (agent, check, gate) declares the resources it needs in its
config, and an ensemble declares them per member run:

```json
{ "command": "cargo test --workspace", "mutex": ["cpu"] }
```

Each name must match `^[a-z][a-z0-9-]{0,31}$` (e.g. `cpu`, `gpu`);
anything else is refused naming the node and the value. The engine
grants each resource to one node run at a time across all graphs of
the daemon: a node that needs `["cpu", "gpu"]` waits until it can hold
both at once (it never holds one while waiting for the other), and
grants go in FIFO order except that a waiter whose full set is free is
never stuck behind an earlier waiter for a different resource.

While waiting the run has status `waiting` — visible in `graph` `get`,
`graph` `node_runs`, and the TUI graph view together with who holds
each resource — and the node's timeout only starts when the lease is
granted. The event log records `mutex_waiting` and
`mutex_acquired` for every wait. The lease is released when the run
ends for any reason. Export carries each node's and ensemble's `mutex`
(`format_version` 6); older documents import with the field absent.
Inspect the live state with `canopy mutex list [--json]`. Mutexes are
scoped to one daemon (one machine); there are no counting semaphores or
priorities.

## CPU quota cap

Mutexes serialize gate nodes, but agents still run cargo builds, tests,
npm and Playwright outside any gate — so the daemon also caps its own
cgroup. `canopy setup` writes `CPUQuota=` into the `canopy.service`
systemd user unit from the `daemon.cpu_quota_percent` setting, which
bounds every agent and gate the daemon spawns.

The value is percent of one core (`600` = 6 cores) and defaults to half
the host's logical CPUs (at least one core); `0` disables the cap. Changing it with `config`
`set` applies to the running daemon at once via `systemctl --user
set-property` (runtime and persistent) — restarting the daemon is never
required. `agent` `status` shows the configured value, the value the
cgroup enforces, and whether it enforces one.

Every spawned node also exports `CARGO_BUILD_JOBS`,
`NEXTEST_TEST_THREADS` and `RUST_TEST_THREADS` set to the cap in cores
(minimum 2), so tools size their worker counts to the budget instead of
thrashing under the quota.

Where the cap cannot apply — no systemd user instance, no delegated cpu
controller, or macOS/Windows — setup says so once and keeps the previous
behavior. A `50-CPUQuota.conf` drop-in you already own is respected and
reported, never overwritten: while it exists the unit carries no
`CPUQuota=` line of its own.

