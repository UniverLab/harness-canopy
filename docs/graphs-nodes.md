---
title: Node Documents
description: Reusable node documents: install, list, add, remove.
order: 14
---

# Node Documents

## Nodes

A **node document** is one reusable node — a Rust Designer, a Python
Validator, the classic implementer — that is shared and installed like a
whole graph, then added to any graph by name:

```json
{
  "format_version": 1,
  "name": "auditor",
  "kind": "agent",
  "description": "Audits a change against the spec.",
  "config": { "prompt_template": "audit this", "timeout_minutes": 10 },
  "preset": { "name": "auditor-preset", "content": "audit well" }
}
```

`kind` is `agent`, `check`, `gate`, `router`, or `ensemble`. An `ensemble`
document carries the ensemble shape in `config`: `prompt_template`,
`members` (each **without** `platform`/`model`), `min_pass`, and the same
fields `graph_ensemble` `add` accepts (`kind`, timeouts, infra budgets,
`commit_rights`, `mutex`). A document never carries a harness — who runs a
node is the installer's decision, not the author's. `canopy graph validate
<path>` checks a node document exactly as it checks a graph document
(`ok` with `node_kind`, or every problem with its JSON path).

**Install** one from the catalogue:

```
canopy node install <slug>
canopy node install <owner>/<repo>/<slug>@<ref>
canopy node install draft:<id>
canopy node install <ref> --overwrite-presets --yes --show-prompts --json
```

The ref grammar is the graph ref grammar (`<slug>` or
`<owner>/<repo>/<slug>`, optional `@<ref>`), plus `draft:<id>` for a
node draft shared from the editor — the same one-use, 30-minute,
unreviewed draft as a graph, with the same `draft not found` message
(see [Install a graph](graphs-sharing.md#install-a-graph)); a draft holding a graph
document is refused, saying which kind it found. The document is
fetched through `graphs.catalogue_url` and validated exactly as `canopy
graph validate` validates it. Before anything is written, the same
**review** `canopy graph install` prints comes first: every shell
command (`config.command`) labelled by node, every agent instruction
(`prompt_template`, `prompt_override`, and the preset content behind
`prompt_preset` — bundled or builtin — with its first 3 lines and its
total line count, full text with `--show-prompts`), and the attention
marks for command shapes worth a second look (pipe to shell, `sudo`,
`rm -rf`, `chmod 777`, `git push`/`--force`, writes under `~/.ssh`,
`~/.config` or `/etc`, `eval`, `base64 -d`, pipe to `python -c`/`node
-e`). Then it asks `Install <name> from <source>? [y/N]` — official
documents are asked about exactly like community ones. Without a TTY
and without `--yes` it prints the summary and refuses, exiting 1 with
`refusing to install without confirmation: re-run with --yes after
reviewing the commands above`; a draft's refusal adds that the draft
is gone and asks for a new link from the editor. `--yes` installs after
the summary without the question, and `--json` prints the review as
JSON on stdout with the question on stderr.

Only after the confirmation is the document written to
`~/.canopy/nodes/<name>.json`. Its bundled `preset` is installed as
`~/.canopy/prompts/<preset.name>.md` with the same no-silent-overwrite rule
as a graph's presets: a local file with different content aborts the
install unless `--overwrite-presets` is given. A graph document fetched
from a node endpoint is refused, and a 410 says `<name> was removed
from the catalogue after a community report: <reason> (<issue>)`.

Over MCP, `node` `install` { `ref`, `overwrite_presets`?, `accept`? } follows
the same gate: **without** `accept: true` it installs nothing and
returns `{ needs_review: true, review: { source, commands, prompts,
attention } }`; with `accept: true` it installs and echoes the same
review in the result. An agent must show the review to its user before
passing `accept: true`.

**List** what is available:

```
canopy node list
canopy node list --json
```

The list shows installed copies and the builtins embedded in the binary,
each marked `installed` or `builtin`. The builtins are `implementer`,
`cargo-gates`, `reviewer-committer`, `commit-check`, `resilience`, and
`ensemble-proposers`; they resolve offline, with no install step. An
installed copy of a builtin's name wins over the builtin.

**Add** one to a graph:

```
graph_node { action: "add", spec_id, name: "audit", node: "auditor",
             config_overrides: { "platform": "claude", "model": "opus" } }
graph_ensemble { action: "add", spec_id, name: "Proposers", node: "ensemble-proposers",
                 members: [{ "platform": "openrouter" }, …],
                 from_node: "<kickoff>", condition: "always",
                 on_pass_to: "<arbiter>" }
```

`node` is the alternative to `kind` + `config`, and
`config_overrides` shallow-merges on top of the document's config — the
override wins, the rest of the document is preserved. An agent node
document has no harness, so the call is refused with a message naming the
document unless the overrides supply `platform` (or `cli`, or `crew`). An
ensemble document supplies the shared prompt, the member count and the
ensemble shape, but its members still need a platform each from the
caller's `members` list.

**Remove** an installed copy:

```
canopy node remove <name>
```

A builtin with no installed copy cannot be removed; an installed copy that
overrides a builtin is removed like any other, and the builtin resolves
again afterwards.

Node documents replaced the retired blueprint tools and the `blueprint`
parameter outright: `graph_node` and `graph_ensemble` `add` take `node`,
`kind` + `config`, or — for ensembles — a crew, and nothing else.

