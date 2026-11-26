---
title: Node Documents
description: "Reusable node documents: install, list, add, remove, and read-only enforcement."
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

`kind` is `agent`, `check`, `gate`, `router`, `deliver`, or `ensemble`. An `ensemble`
document carries the ensemble shape in `config`: `prompt_template`,
`members` (each **without** `platform`/`model`), `min_pass`, and the same
fields `graph_ensemble` `add` accepts (`kind`, timeouts, infra budgets,
`commit_rights`, `read_only`, `mutex`). A `deliver` document hands a rendered
prompt to a live interactive session — the node form of a `send_to_session`
background agent. Its config is exactly one target plus the prompt:
`target_session_name` or `target_session_id` (exactly one, blanks unset)
and a non-empty `prompt_template` rendered with the router decision
markers (`previous_feedback`, `previous_output`, `spec_content`,
`spec_name`). A deliver node holds no platform, model or crew, counts for
no quota, and must have exactly one outgoing edge. A document never carries a harness — who runs a
node is the installer's decision, not the author's. `canopy graph validate
<path>` checks a node document exactly as it checks a graph document
(`ok` with `node_kind`, or every problem with its JSON path).

**Install** one from the catalogue:

```
canopy node install <slug>
canopy node install <owner>/<repo>/<slug>@<ref>
canopy node install draft:<id>
canopy node install <ref> --as <name> --replace --overwrite-presets --yes --show-prompts --json
```

The ref grammar is the graph ref grammar: a bare `<slug>` is the
official UniverLab entry, `<owner>/<repo>/<slug>` a community one
(each with an optional `@<ref>`), plus `draft:<id>` for a
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
`~/.canopy/nodes/<name>.json`, stamped with the catalogue entry it came
from (`slug`, `owner/repo/slug`, or `draft:<id>`). Its bundled `preset`
is installed as `~/.canopy/prompts/<preset.name>.md` with the same
no-silent-overwrite rule as a graph's presets: a file owned by another
source aborts the install unless `--overwrite-presets` (or `--replace`)
is given. The engine resolves that literal path first and falls back
to `~/.canopy/prompts/nodes/<preset.name>.md`, where a daemon-start or
`canopy setup` migration moves leftover top-level files (never
overwriting). A
graph document fetched from a node endpoint is refused, and a 410 says
`<name> was removed from the catalogue after a community report:
<reason> (<issue>)`.

A name already installed from a different source is refused, naming
both sources — the installed copy (and every graph using it) is never
silently replaced. `--as <local-name>` installs under another local
name instead (the bundled preset is renamed with it: a `<doc>-…`
preset keeps its suffix under the new name, any other preset becomes
`<as>-<preset>`, with the config's `prompt_preset` rewritten to
match), or `--replace` overwrites. `--as` takes a plain slug
(lowercase letters, digits, `-`); anything else is refused before
anything is fetched. Reinstalling from the same source
updates in place, and an installed copy still overrides a builtin of
the same name.

Over MCP, `node` `install` { `ref`, `as`?, `replace`?, `overwrite_presets`?, `accept`? } follows
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
each marked `installed` or `builtin`, with the catalogue entry that
installed it next to the name (`catalogue_source` in `--json`; a
hand-written copy shows `local`). The builtins are `implementer`,
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

## Read-only nodes

An agent node can be declared read-only in its **node config** — set
`read_only` through `graph_node` `add`/`update` config or in a node
document's `config`:

```
read_only: true
```

`graph` `get`/`export` and `graph_edit` `import` carry the setting with
the node or ensemble.

The engine fingerprints the working tree when the run starts and compares
it when the run ends, before routing. A read-only node that changes the
tree is a deterministic **fail** with result `read_only_violation`
(routed through the `fail` edge like any other failure). The message
lists up to 20 touched paths (added, modified, deleted) and states that
the changes were left in place. The node's own output is kept, and the
finding is visible in `graph_control`/`graph node_runs` output and the
TUI node tail.

The fingerprint hashes `git status --porcelain`, `git diff HEAD --binary`,
and the path plus content hash of every untracked non-ignored file
(`git ls-files -o --exclude-standard`). Paths the harness legitimately
writes inside the project come from the registry's `project_paths` for
that platform and are excluded before hashing.

A tree that already holds uncommitted work when the node starts never
fails by itself, and a read-only node that changes nothing passes on a
dirty tree. In a non-git workdir the node runs unenforced and the run
output says `read_only not enforced: not a git workdir`.

`read_only: true` cannot be combined with `commit_rights: true` on the
same node or ensemble; validation refuses the combination naming both.
Ensembles accept `read_only` in `graph_ensemble` `add`/`update`, and
every member run inherits it. See [Commit rights](graphs-execution.md#commit-rights)
for the mirror rule.

