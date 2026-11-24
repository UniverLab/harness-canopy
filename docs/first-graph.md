---
title: Your First Graph
description: Install canopy, run one check node, and read its log in five minutes.
order: 4
---

# Your First Graph

Five minutes from a fresh machine to a finished graph run. You need only
the `canopy` binary — no configuration file, no AI CLI, no specs. The
graph below has a single `check` node that runs `echo hello` in a shell,
so nothing needs a platform installed. (To run agents you do need one:
see [Installation](installation.md).)

## 1. Start the daemon

<!-- test -->
```bash
canopy daemon start
```

The daemon owns the database and runs your graphs. Everything it keeps
lives under `~/.canopy/`, which it creates on first start — an empty
home directory is fine.

## 2. Write the graph file

Save this as `first-graph.json`. It is a
[graph export document](graphs-sharing.md): `format_version` 7, one node
named `hello`, and empty `edges` and `ensembles`.

```json
{
  "format_version": 7,
  "name": "first-graph",
  "nodes": [
    {
      "name": "hello",
      "kind": "check",
      "position": 1,
      "config": {
        "command": "echo hello"
      }
    }
  ],
  "edges": [],
  "ensembles": []
}
```

## 3. Validate it

<!-- test -->
```bash
canopy graph validate first-graph.json
```

Validation is read-only: it checks the file against the same rules
`import` enforces, without touching the database. A valid document
prints:

```text
ok: graph 'first-graph' is valid (1 nodes, 0 edges, 0 ensembles).
```

Fix anything it reports, then continue.

## 4. Import it

<!-- test -->
```bash
canopy graph import first-graph.json
```

Import always creates a **new** graph — it never updates or overwrites
an existing one. The new graph has the file's nodes and edges but no
specs yet, so the next step passes its input on the command line.

## 5. Run it

<!-- test -->
```bash
canopy graph run "first-graph" --idea "hello"
```

`graph run` takes the graph name (or id) and runs it in the background.
`--idea` is the free-form text fed to the nodes as `{{spec_content}}`
when the graph has no bound specs and no queue — which is exactly what
an import produces. The `hello` node runs `echo hello` and finishes.

## 6. Read the log

<!-- test -->
```bash
canopy graph log "first-graph"
```

The log prints the graph's durable event log, one line per event:
a timestamp, the event kind, any `spec=`/`node=` names, and a short
message. For this run you will see `GRAPH_STARTED`, `SPEC_STARTED`,
`NODE_STARTED` (`Node 'hello' started.`), `SPEC_COMPLETED`, and
`GRAPH_COMPLETED` (`Completed (1/1 specs).`). A node that passes has no
separate finish event, and command output such as `echo hello` is not
printed here — this is a lifecycle trail, not a transcript. If a node
fails, its error appears in the `NODE_FINISHED` message.

## Where to go next

- [Quick Start](quickstart.md) — the daemon, the setup wizard, and your first agent.
- [Graphs](graphs.md) — nodes, edges, and specs.
- [Sharing Graphs](graphs-sharing.md) — export, import, and validate in detail.
- [CLI Reference](cli-reference.md) — every `canopy` command.
