---
title: Sharing Graphs
description: Export and import, validate before you share, and install a graph.
order: 13
---

# Sharing Graphs

## Export and import

A graph's design — its name, description, nodes, edges, and ensembles —
can leave one machine as a single JSON file and be recreated on
another. Sharing a graph becomes sending a file, not narrating the
`graph_node` `add`/`graph_edge` `add`/`graph_ensemble` `add` calls that built
it, and the file is also a diff: review a change to a graph, or keep
one in a repo next to the code it operates on.

```
canopy graph export <graph_id> [--output <path>]
canopy graph import <path> [--workdir <dir>] [--name <name>]
```

`export` writes to `--output`, or to stdout (so it can be piped) when
omitted. `import` always creates a **new** graph — it never updates,
merges, or overwrites an existing one; `--workdir` defaults to the
current directory, and `--name` overrides the file's own name. If the
resolved name is already taken in the target workdir, import still
succeeds under a numeric suffix (`"My Graph (2)"`) and reports which
name it used. The same two operations exist over MCP as
`graph` `export` (`graph_id`) and
`graph_edit` `import` (`document`, `workdir?`, `name?`), so a graph is drivable end
to end through MCP as well as the CLI.

Every daemon database connection waits up to 5 s for a concurrent writer
before giving up, so imports normally queue behind a busy database rather
than failing. If the wait still expires, `graph_edit` `import` returns
`graph_edit: database busy after 5 s — nothing was written, retry`:
the import is all-or-nothing, nothing partial is left behind, and the
call is safe to retry as-is.

What the file **excludes** is deliberate: no ids (edges reference
nodes by `name`, which is what makes the file reviewable and
hand-editable — node names must therefore be unique within an exported
graph, or export refuses with the names it found), no `workdir`, no
specs, and no run/status state. A graph file is a shape and a set of
instructions, not somebody else's backlog or history.

`platform`/`model` are always included for every agent node and
ensemble member (`format_version: 2`), so an exported file round-trips
its harness bindings through `import` unchanged. A node that uses its
platform's default model exports `"model": null` explicitly, and an
ensemble member without a binding or override exports explicit `null`s
for the unset fields. Import accepts both `format_version: 1`
(members with no binding, reported as missing a platform) and `2`
(bindings restored verbatim); it never rejects a platform the
importing machine has not configured — validating that a pair is
usable belongs to `graph_control` `preflight`. `import`'s response always lists
every agent node left without a platform, so there's exactly one
thing to check before running an imported graph:
`nodes_missing_platform` in the MCP response, or the same list
printed by the CLI.

An ensemble's `kind` is carried in the document for `cascade` and
`round_robin`; export omits it for `parallel`, which is what an
absent `kind` means on import. Import refuses a `kind` value it does
not know — naming the ensemble and the value — and refuses a document
that carries any field it does not know how to restore, as part of
its all-or-nothing contract. `format_version` 6 remains the newest
accepted version (it also carries the bundled `prompt_presets`, below);
1, 2, 3, 4 and 5 documents import unchanged (they carry
fewer fields, never more).

A node's `prompt_preset` names a preset that must exist on the
importing machine: a file `<canopy_dir>/prompts/<name>.md` (the same
file the engine reads at spawn time) or one of the builtins (`implementer`,
`reviewer`, `resilience`, which are seeded on daemon start and fall back
to embedded constants). Since version 6 the document **bundles** every
custom preset its agent nodes reference — a `prompt_presets` array of
`{"name", "content"}` entries carrying the full text, so a shared
recipe arrives with everything its nodes need. On import, each bundled
preset is installed into the prompts dir when no file with that name
exists locally; a local file with identical content is a no-op, and one
with different content **refuses the import** — naming the preset — so a
preset you edited locally is never overwritten. Builtin presets are
never bundled. A preset the document references but neither bundles nor
has locally still refuses the whole import — naming **every** unknown
preset and writing nothing — rather than importing a graph whose agent
would silently run on an empty prompt:

```
Unknown prompt preset(s): my-auditor, my-reviewer. Add file(s) under /home/you/.canopy/prompts or use a builtin (implementer, reviewer, resilience).
```

Ensemble members are not checked — a member
carries no preset of its own. Nothing else about prompt resolution
changes: at spawn time an explicit `prompt_template` still wins, then
`prompt_preset`, then the fallback template, and a blank
`prompt_preset` names no preset and takes that fallback exactly as
before.

A [crew](graphs-ensembles.md#crews)-bound design travels by name, never by its members:
`graph` `export` writes `crew: "<name>"` on a bound ensemble (with no
`members` array), on a bound agent node's config, and on a bound agent
hook — and, since version 5, a graph's event-keyed `hooks` ride along
in the document as well, which is what gives a bound hook something to
travel in. `graph_edit` `import` keeps the reference when a crew of that name
exists locally: the ensemble binds and re-materializes the crew's
members, and a bound node or hook resolves its pair at dispatch from
the crew. When the name is missing locally the import still succeeds,
lists every unresolved binding under `missing_crews` (next to
`nodes_missing_platform`) in the MCP response or the CLI output, and
leaves the binding pending — a pending ensemble cannot dispatch until
a crew of that name is created, and `graph_control` `preflight` reports it as
`would_fail`.

A crew-bound ensemble travels without `members` (the crew supplies them)
but *not* without its wiring: an absent `members` key is simply an empty
list, and the ensemble's `entry_from_node`/`entry_condition`, every
`extra_entry_sources` item, and any `on_pass_to`/`on_fail_to` exit
chained into another ensemble are all recorded at import. They are wired
through the same sync path the engine already uses the moment the crew's
members are materialised — during the import when the crew exists
locally, or on the crew's first sync (dispatch, or `crew` `create`/
`update`) when it is created later. Either way the result is a fully
wired ensemble, so a shared design reaching a machine that has the crews
arrives runnable; a document that names neither a crew nor any members is
still refused, naming the ensemble.

An [ensemble](graphs-ensembles.md#ensembles) round-trips as one ensemble — not as its
expanded member/quorum nodes — via its own `ensembles` array entry.
The `entry_from_node`, `on_pass_to`, and `on_fail_to` fields use a bare
string for a plain node. When wiring to another ensemble's quorum, export
uses `{ "ensemble": "<name>" }` so an ensemble target is distinct from a
plain node with the same name; import resolves that table to the target
ensemble's join node and restores its member fan-out edges.

An ensemble may be entered from more than one node
(`graph_ensemble` `update`'s `add_entry_from`). Export writes every such
source in `extra_entry_sources`, an array of
`{"from_node": "<node name>", "condition": "<pass|fail|always|error>"}` entries
beyond the primary (`entry_from_node` + `entry_condition`). Import restores each as
an edge to every member, so multi-source entry — e.g. a committer entered on
`pass` by an unlock check and on `fail` by a commit-verification check — survives
the round trip. The field is omitted when an ensemble has only its primary source.

Here is a complete, hand-writable example: an implementer, a 2-model
ensemble of reviewers, and a committer the quorum routes to on pass.

```json
{
  "format_version": 5,
  "name": "implement-and-review",
  "description": "Implement a spec, get two model opinions, then commit.",
  "nodes": [
    {
      "name": "implementer",
      "kind": "agent",
      "position": 1,
      "config": {
        "platform": "opencode",
        "model": null,
        "prompt_template": "Implement: {{spec_content}}",
        "timeout_minutes": 30
      }
    },
    {
      "name": "committer",
      "kind": "agent",
      "position": 4,
      "config": {
        "platform": "opencode",
        "model": "opencode/muse-spark-1.3-contributor-free",
        "prompt_template": "Review the feedback and commit if satisfied.",
        "commit_rights": true,
        "timeout_minutes": 15
      }
    }
  ],
  "edges": [],
  "ensembles": [
    {
      "name": "reviewers",
      "kind": "cascade",
      "prompt_template": "Review this diff for correctness: {{previous_feedback}}",
      "entry_from_node": "implementer",
      "entry_condition": "always",
      "on_pass_to": "committer",
      "min_pass": 1,
      "timeout_minutes": 20,
      "members": [
        {"platform": "copilot", "model": null, "prompt_override": null},
        {"platform": "opencode", "model": "opencode-go/qwen3.7-plus", "prompt_override": null}
      ]
    }
  ]
}
```

Note what is absent: no `id` anywhere, and the ensemble's own
member/quorum nodes never appear in `nodes` — only its
`entry_from_node`/`on_pass_to` (both plain node names) and its
`members` array do. `implementer` uses its platform's default model,
hence the explicit `"model": null`; a `format_version: 1` document
with `{}` members still imports, with every unbound node reported as
`nodes_missing_platform`. `reviewers` runs as a cascade: one member at
a time, first usable verdict wins. `reviewers` has a single entry source, so
`extra_entry_sources` is omitted; an ensemble entered from several nodes carries
each additional one there.

## Validate before you share

Before sending a file — or installing one you received — check it without
touching the database:

```
canopy graph validate <path> [--json]
```

The same check exists as the MCP
`graph` `validate` (`path?` or `document?`, exactly one of the two). Both
detect the document kind on their own: an object with `nodes` is a graph
document, one with `kind` plus `config` is a node document. Neither needs
a workdir, and neither writes anything.

A graph document runs every structural check `import` runs —
`format_version`, no unknown field anywhere in the document, known
config keys per node kind, the `mutex` shape, ensemble shape, and
template bindings — through the same code, plus one sharing rule of its
own: every `prompt_preset` a node names must be bundled in the document
(`prompt_presets`, version 6) or a builtin (`implementer`, `reviewer`,
`resilience`). A bundled entry itself must be installable anywhere too —
a plain non-blank name, not a path, not a builtin, not repeated, with
non-empty content — the same entry rules `import` enforces before
writing anything. Validation ignores what this machine happens to have
installed, so a green report means the document itself carries
everything its nodes need; `import` can still refuse to overwrite a
locally edited preset file whose content differs from the bundled one.

A node document is one shareable node:

```json
{
  "format_version": 1,
  "name": "my-check",
  "kind": "check",
  "config": { "command": "true" }
}
```

Its rules: `format_version` is `1`, `name` is a slug (`[a-z0-9-]{1,48}`),
`kind` is a known authorable kind (`join` is engine-managed and refused),
the config holds only keys that kind reads, no `platform`, `model`, or
`cli` key appears anywhere in the config (harnesses are chosen by the
installer, never the author), and a bundled `preset` carries the same
name as the config's `prompt_preset` when both are present.

On success the report carries `ok`, the kind, the name, the
node/edge/ensemble counts, and the agent nodes that will still need a
harness (`needs_harness`); a node document also carries `node_kind`
(`agent`, `check`, `gate`, `router`) and reports one node, no edges and
no ensembles. On failure it carries every problem found — not only the
first — each with a JSON path (`nodes[3].config.prompt_preset`) and a
message. The CLI exits 0 on success and 1 on failure; `--json` prints
the machine-readable report.

## Install a graph

A published graph becomes a local graph in one command, through the
graph catalogue at `graphs.univerlab.org` — fetching through the
catalogue is what counts the install:

```
canopy graph install <ref> [--workdir <dir>] [--name <name>] [--overwrite-presets] [--yes] [--show-prompts] [--json]
canopy graph install draft:<id>
```

The ref is `<slug>` for an official graph or `<owner>/<repo>/<slug>`
for a community one, each with an optional `@<ref>` suffix selecting a
version (`my-graph`, `acme/widgets/my-graph`,
`acme/widgets/my-graph@v1.2`). The slug is 1–48 lowercase letters,
digits, or `-`; owner and repo follow GitHub's naming rules. Anything
else is refused with the accepted forms in the message.

A draft shared from the editor at `graphs.univerlab.org/editor`
installs with `canopy graph install draft:<id>` (a node draft with
`canopy node install draft:<id>`); `<id>` is 26 lowercase base32
characters (`[a-z2-7]`), and anything else after `draft:` is refused
with the accepted forms. The catalogue serves the draft directly at
`<catalogue_url>/i/draft/<id>` — no redirect, and a redirect is
refused rather than followed — under the same User-Agent, 15 s timeout
and 1 MiB cap as any other ref. A draft lives 30 minutes, installs
once, and carries no review: the summary and the confirmation question
both say `This is an unreviewed draft, not a published document.`, and
the review and the report name the source `draft`. The review fetch is
what consumes the draft, so a refused install leaves it gone — the
refusal says so and asks you to copy a new link from the editor — and
a draft that expired or was already installed reports `draft not
found: drafts last 30 minutes and are deleted by their first
install`.

**Review before anything is written.** A shared graph is code: check
and gate nodes run shell commands with your permissions, hooks run
commands too, and agent nodes carry prompts that steer an agent able to
edit files and run tools. So the install first fetches and validates
the document and prints a review summary of everything it will run:

- every shell command — check/gate `config.command` and hook
  `command` — labelled by node or hook, verbatim;
- every agent instruction — `prompt_template`, `prompt_override`, the
  bundled or builtin preset content behind `prompt_preset`, and hook
  `prompt` — with its total line count and its first 3 lines;
  `--show-prompts` prints each one in full;
- **attention marks** for command shapes worth a second look: a pipe
  into `sh`/`bash` (typically `curl … | sh`), `sudo`, `rm -rf`,
  `chmod 777`, `git push` or `--force`, writes under `~/.ssh`,
  `~/.config` or `/etc`, `eval`, `base64 -d`, and a pipe into
  `python -c` or `node -e`. A mark is a flag for your eyes, never a
  block — the same marks the catalogue pages show.

Only then does it ask `Install <name> from <source>? [y/N]`, where the
source is `official`, `owner/repo` (plus the ref, when given) or
`draft` — a draft's question also carries the unreviewed-draft line.
Official graphs are asked about exactly like community ones — official
is not exempt. Answering anything but `y`/`yes` installs nothing. When
stdin or stdout is not a TTY (a pipe, a script, an agent) the install
refuses after printing the summary, exiting 1 with
`refusing to install without confirmation: re-run with --yes after
reviewing the commands above` — re-run with `--yes` to proceed without
the question, or `--json` to get the review as JSON on stdout (the
question then goes to stderr). The summary is printed in every mode;
`--yes` skips the question, never the review.

The fetch is one GET with a `canopy/<version>` User-Agent, following
redirects (at most 3, only ever to https — a catalogue URL you set to
`http` yourself may redirect within http, and any other scheme is
refused) with a 15 s timeout and the body capped at 1 MiB — no other
network traffic. A draft is the exception: it is served directly, and
a redirect is an error, not a hop. HTTP errors are reported with the
status and the URL tried; a 404 says the graph was not found in the
catalogue, and a 410 says `<name> was removed from the catalogue after
a community report: <reason> (<issue>)` — a document the catalogue took
down after a report is never installed.

The fetched document goes through the same validation as
[`validate`](#validate-before-you-share): any problem aborts the
install and lists every problem. Its bundled presets (version 6) are
written to `~/.canopy/prompts/` — a file already there with identical
content is a no-op, while one with different content aborts the whole
install before anything is written, naming the file, unless
`--overwrite-presets` is passed. The import itself is the
[`import`](#export-and-import) path: always a new graph (a numeric
suffix when the name is taken), `--name` overriding the document's
name, `--workdir` defaulting to the current directory (which must be
absolute and exist).

The report carries the graph id and name, its source, the workdir, the
presets written, the review, and the agent nodes still needing a
platform/model with the next command to assign them (`--json` prints
the machine-readable report). The same operation exists as the MCP tool
`graph_edit` `install` (`ref`, `workdir`, `name?`, `overwrite_presets?`, `accept?`):
called **without** `accept: true` it installs nothing and returns
`{ needs_review: true, review: { source, commands, prompts,
attention } }` — the same review printed above; called with
`accept: true` it installs and echoes that review in the result. An
agent driving the MCP tool must show the review to its user before
passing `accept: true`. A draft keeps the two-call shape: the daemon
parks the body the review call fetched, so `accept: true` installs it
without a second request — and a draft that reports `draft not found`
is gone for good; ask for a new link from the editor rather than
retrying. The catalogue base URL is the `graphs.catalogue_url` setting
(default `https://graphs.univerlab.org`,
see [Tuning the engine](graphs-operations.md#tuning-the-engine)).

