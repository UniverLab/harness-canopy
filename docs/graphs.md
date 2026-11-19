---
title: Graphs
description: The DAG graph engine — specs, nodes, edges, gates and lifecycle.
order: 7
---

# Graphs

The graph engine runs multi-step processes as a DAG, combining agent
invocations, shell checks and validation gates.

## Structure

```
Graph
└── Spec (ordered)
    └── Node ──Edge(pass/fail/always)──▶ Node
```

- **Specs** — ordered units of work inside a graph.
- **Nodes** — five kinds:
  - `agent` — invokes a CLI tool with a prompt template.
  - `check` — executes a shell command.
  - `gate` — validates previous output (e.g. `output_contains`).
  - `router` — classifies and routes by token matching with fallback.
    With `"decision": true` in its config, a router instead asks the
    platform's decision model (a fast typed-choice classifier such as
    `typesafe/jev`, invoked in print mode with the request JSON on stdin)
    which declared route to take — no agent is spawned. Routes map onto the
    model's `criteria` as label/description pairs; the rendered
    `prompt_template` (with `{{previous_output}}` also available) becomes
    the request state, and an optional `"min_confidence"` (0–1, default 0)
    sends low-confidence answers to the declared fallback. The platform must
    declare a decision-model invocation (`decision_args`) in the registry or
    the node is refused.

    With `"mode": "value"` a router is deterministic: it reads a value the
    engine already knows, with no model call, and matches it case-sensitively
    against its routes. `mode` is optional and takes only `"model"` (the
    default, both behaviours above) or `"value"`:

    ```json
    {
      "mode": "value",
      "source": "spec_type",
      "routes": [{ "label": "feat" }, { "label": "fix" }],
      "default_route": "fix"
    }
    ```

    `source` is one of `spec_type`, `spec_scope`, `spec_tier` (the spec's
    review tier, empty when none) and `previous_output` (the previous node's
    trimmed stdout for a check node, or the same text a model router receives
    for any other node). A value router declares 1–8 routes whose labels are
    the values that select them (`description` is optional and unused), an
    optional `default_route` naming one of those labels, and no `fallback`,
    `decision` or `min_confidence` — those are model-router fields and are
    refused. Every route needs exactly one outgoing edge. A value that
    matches no route takes `default_route` or fails the node; no silent
    fallback and no platform is required.
  - `quorum` — engine-managed node that closes an
    [ensemble](graphs-ensembles.md#ensembles), never created directly.
- **Edges** — connect nodes with routing conditions: `pass`, `fail`,
  `always`.
- **Node MCP profile** — agent and ensemble nodes launched by the graph
  engine receive a fixed set of 12 tools in `tools/list`
  (`CANOPY_MCP_PROFILE=node`); check nodes, hooks, and other sessions keep
  the full list. See [MCP Tools](mcp-tools.md).

## Read next

- [Template Variables](graphs-templates.md) — the placeholders agent prompts and
  check commands expand.
- [Ensembles](graphs-ensembles.md) — parallel, cascade, round-robin and random member
  groups, crews, and availability at dispatch.
- [Execution](graphs-execution.md) — commit rights, the self-report
  requirement, the infra-retry budget, the execution limit, lifecycle,
  deterministic commits and spec notes.
- [Isolation](graphs-isolation.md) — sandboxes, concurrency, and named mutexes.
- [Hooks and Queues](graphs-hooks-and-queues.md) — event-keyed hooks, the
  standalone spec backlog, and spec queues.
- [Sharing Graphs](graphs-sharing.md) — export and import, validate before you
  share, and install a graph from the catalogue.
- [Node Documents](graphs-nodes.md) — reusable node documents: install, list,
  add to a graph, remove.
- [Graph Operations](graphs-operations.md) — archive and restore, node run
  history, the event log, and tuning the engine.

The graph tools live in [MCP Tools](mcp-tools.md).
