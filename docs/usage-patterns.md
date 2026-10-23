---
title: Usage Patterns
description: The shapes a graph can take, what each one is good at, and where each one breaks.
order: 8
---

# Usage Patterns

A graph is a shape you choose before you write a single prompt. The shape
decides what happens when something goes wrong — which is the only part of
the design that matters, because the happy path is the same in every graph
ever built: an agent does the work and it was fine.

Everything below is a shape. The prompts are implementation detail; you can
swap models, harnesses and wording freely inside a shape without changing
what the graph *is*. Getting the shape wrong is expensive in a way that
getting a prompt wrong is not.

## How to read this page

Patterns carry a status, because a shape that has drained a real backlog and
a shape that exists only in a diagram deserve different levels of trust:

| Status | Meaning |
|---|---|
| **Proven** | Has run real specs to completion on this machine. |
| **Designed** | The engine supports it today; we have not run it. |
| **Blocked** | Needs something the engine does not have yet, named below. |

**Per-graph logging is built in.** You no longer need a `printf … >> file`
command hook on `on_completed`/`on_failed` to keep a per-graph log — the
engine records that history itself. Read it with `canopy graph log <graph-id>`
(or the `graph_events` MCP tool). See the
[Event log](graphs.md#event-log) section.

---

## 1. Gated implement — Proven

The workhorse. An agent writes code, a deterministic check verifies it, a
reviewer judges whether the spec was honored, and only then does anything
land.

```mermaid
graph LR
    I[implement] -->|pass| C[check: build + tests]
    C -->|pass| R[review + commit]
    C -->|fail| I
    R -->|fail| I
```

Three rules make this work, and each was learned by watching it not work:

**The implement edge is `pass`, never `always`.** An `always` edge routes a
*failed* implement into the reviewer, which then approves work that was never
written. A real run marked a spec `completed` with no commit behind it that
way.

**The check sits before the reviewer.** Code that does not build never
reaches review. Otherwise the reviewer writes `CHANGES_NEEDED: implementation
is broken` and the spec is marked `completed` anyway, because routing keys on
the run's status, not on its prose.

**Auto-fixing commands must not bounce work.** Run `cargo fmt --all`, which
rewrites, not `cargo fmt --check`, which fails. Only genuine breakage should
route back to implement.

**Hand plans through spec notes, not files.** A Designer node writes its
plan with `spec_note_write(run_id, body)` instead of a temp file no later
node can find, and the verify step reads
`grep -q '^## Designer' "$CANOPY_SPEC_NOTES_FILE"` — every agent prompt
already carries the notes, so nothing needs deleting and nothing goes stale.
See [Spec notes](graphs.md#spec-notes).

Use it for: essentially everything. Start here and add shape only when a
specific failure justifies it.

---

## 2. Cross-platform check — Proven

The same shape, with the reviewer deliberately on a different vendor from the
implementer.

```mermaid
graph LR
    I["implement (claude/opus)"] -->|pass| X["crosscheck (opencode/qwen)"]
    X -->|pass| F[finish]
    X -->|fail| I
```

A model reviewing its own family's output shares its blind spots, and shares
its quota ceiling. Splitting the two across vendors buys independent judgment
and means one exhausted subscription does not stall the whole graph.

Keep the cross-check prompt narrow — diff plus spec plus "find what is
wrong". A cross-checker invited to re-implement will re-implement.

Use it for: security-sensitive changes, and anywhere you suspect the fix is
overfit to one model's reading of the spec.

---

## 3. Resilience branch — Proven

Pattern 1 fails the spec the moment implement dies, so nobody ever reads the
failure. If the CLI died on a quota that resets at a known hour, that
knowledge is thrown away. Give implement two exits.

```mermaid
graph LR
    I[implement] -->|pass| C[check]
    I -->|fail| M[resilience]
    C -->|pass| R[review + commit]
    C -->|fail| I
    R -->|fail| I
    M -->|pass| I
```

The resilience node is a cheap model on a *different quota* from the
implementer — otherwise it is dead exactly when you need it. It reads the
dead node's output and either repairs a transient failure and reports pass
(so implement retries), or recognizes exhaustion, calls
`graph_schedule_autorun` with the reset time, and reports fail — the spec
fails, but the graph wakes itself at that instant.

Give it the clock. Models do not know what time it is, and "resets 6am" is
ambiguous without it: a real run at 01:52 scheduled the wake-up for 6am *the
next day*, a 23-hour stall. The prompt must require `date -Iseconds` first.

Use it for: any unattended graph that runs longer than your shortest quota
window.

---

## 4. Draft ensemble — Proven

An ensemble is a set of agent nodes that get the same prompt, plus a quorum
(parallel) or single dispatch (cascade/round_robin). A parallel ensemble takes
2–8 members by default, since it runs every member concurrently; the
sequential `cascade`/`round_robin` kinds take 2–16, because they dispatch one
member at a time and a longer list only adds rotation/fallback depth. Both caps
can be raised in settings (see [Tuning the engine](graphs.md#tuning-the-engine)). The canonical use spends the expensive
model once, on a pre-digested problem.

```mermaid
graph LR
    subgraph ENS["ensemble: drafters"]
        E1["free model A"]
        E2["free model B"]
        E3["free model C"]
    end
    ENS --> Q[quorum]
    Q -->|pass| I["implement (paid model)"]
    Q -->|fail| STOP["spec fails"]
```

The ensemble is **one unit**, not three hand-wired nodes:
`graph_add_ensemble` creates the members, the quorum, and the entry and exit
edges in a single call. Wiring three parallel `pass` edges out of one node by
hand is the "ambiguous outgoing edges" mistake, and it aborts the run.

Three free models propose an approach; the quorum consolidates their
proposals into one document; the expensive implementer receives all three and
writes the code. You paid for one expensive run instead of one expensive
exploration plus one expensive implementation.

The mirror image also works: 2–3 free reviewers read the same diff and the
quorum consolidates their findings into one verdict.

Use it for: problems where the *approach* is the hard part and the typing is
not.

---

## 5. Router by complexity — Proven

The first shape that needs the router node. Two implementers on different
harnesses and price points; a cheap classifier reads the spec and decides
which one earns it.

```mermaid
graph LR
    RT{"router: how hard is this?"} -->|simple| CH["implement (cheap)"]
    RT -->|complex| EX["implement (expensive)"]
    CH -->|pass| C[check]
    EX -->|pass| C
    C -->|pass| R[review + commit]
    C -->|fail| EX
    CH -->|fail| EX
```

Two things about this diagram are load-bearing and easy to get wrong.

**A failed cheap implement escalates to the expensive one — it does not go
back to the router.** The router would classify the same spec the same way
and send it back to the model that just failed, forever. Routing decisions
based on the spec are stable by definition; only new information should
produce a new decision.

**The check's fail edge also goes to the expensive implementer**, for the
same reason: by the time a build is red, the cheap classification has been
falsified by evidence.

**The router must be cheap.** A router as expensive as the expensive
implementer means you paid the high price on every task, including the ones
you routed away from it.

---

## 6. Free-tier cascade — Proven

You have four CLIs with generous free tiers and small ceilings. Spend all of
them in sequence before touching anything you pay for. One ensemble node of
`kind: cascade` does it in a single call:

```mermaid
graph LR
    subgraph ENS["ensemble: free-tier cascade"]
        A["cursor"]
        B["antigravity"]
        C["codex"]
        D["kiro"]
    end
    ENS --> F["paid fallback"]
```

`graph_add_ensemble` with `kind: "cascade"` creates the four members, orders them
from cheapest to dearest, and wires a single exit — `on_pass_to` the next node once
a member passes, `on_fail_to` (or dead end) if every member fails. The members are
the same prompt on different harnesses; you choose the order, and the engine walks
it.

The cascade runs its members **one at a time**. A member that fails or infra-crashes
falls through to the next member cold; the first member that passes exits the ensemble.
A bounce — the review after the implementer rejects the work — resumes the member that
last passed and only falls through to the next one if that resumed member fails again.
A member with `infra_retry_limit: 0` falls through immediately on an infra crash instead
of burning time on retries. See [Ensembles](graphs.md#ensembles) for the full semantics.

The order above is the real free-to-paid walk for this project's own Committer: a
cascade ensemble is what lands every commit here, which is why this pattern is Proven.

### Routing on why a member failed

The cascade falls through on *any* failure — a quota exhaustion, a network error, and
a genuinely wrong implementation all look the same to the ensemble. That is correct
when every member is equally capable and you are only spending ceilings in order. It is
wrong when the right response to "wrong code" is to send the work back to a fixer, not
down to a weaker harness.

For that case you still need the router variant (below). Keep it short: a router is
only justified when the *reason* for the failure changes where the work should go. If
you are only spending free tiers in sequence, the plain cascade above is enough.

### The naive version and why it is a trap

```mermaid
graph LR
    A["implement (cursor)"] -->|fail| B["implement (antigravity)"]
    B -->|fail| C["implement (codex)"]
    C -->|fail| D["implement (kiro)"]
```

`fail` conflates "my quota is gone" with "this code is wrong". A spec with a genuine
bug burns all four free tiers on the same wrong task, and you end the day with every
ceiling spent and nothing done. A chain of fail edges only makes sense if it routes on
*why* the node failed — which is a router, one node for the whole cascade instead of a
triage node between every pair:

```mermaid
graph LR
    A["implement (cursor)"] -->|fail| RT{"router: why did it fail?"}
    B["implement (antigravity)"] -->|fail| RT
    C["implement (codex)"] -->|fail| RT
    RT -->|exhausted| B
    RT -->|exhausted| C
    RT -->|transient| A
    RT -->|broken| STOP[fail the spec]
```

This variant is **Designed** (the engine supports it; we have not run it). Build it only
when you need the cascade to route on failure reason instead of falling through blindly.

### Two things worth knowing before you build either variant

**Exhaustion does not look the same on every harness.** Some CLIs exit non-zero with a
message naming the reset time; others simply stop responding and die on the node's
timeout. A cascade member that times out falls through to the next member — which is
usually what you want. A router reading the output generalizes across both.

**The engine has no memory of a spent harness.** If spec 1 burns cursor and antigravity,
spec 2 starts at cursor again and spends an iteration rediscovering that it is dead —
and it does that for every remaining spec in the queue. Today the only lever is
`graph_schedule_autorun`, which parks the *whole graph* until a reset. A per-harness
"unavailable until" that a router could read does not exist yet.

---

## 7. Specialist review panel — Designed

An ensemble where each member reviews a different axis: one reads for
security, one for performance, one for maintainability, one for the
project's own patterns.

```mermaid
graph LR
    I[implement] -->|pass| C[check]
    subgraph ENS["ensemble: review panel"]
        S1["security"]
        S2["performance"]
        S3["maintainability"]
        S4["patterns"]
    end
    C -->|pass| ENS
    ENS --> Q[quorum]
    Q -->|pass| CM[commit]
    Q -->|fail| I
```

Per-member prompt overrides are available — each member can carry its own
`prompt_override` in `graph_add_ensemble`, so a specialist panel runs all
four axes in parallel with one call instead of four sequential nodes.

**`min_pass` means the wrong thing here.** With homogeneous members,
`min_pass: 2 of 3` reads as "a majority agrees" and that is sensible. With
specialists, each member owns a *different* axis, so `min_pass: 3 of 4` reads
as "one whole dimension of quality may fail and we ship anyway". You want all
four to pass.

But `min_pass = all` with four cheap models means the grumpiest one blocks
every change forever. The honest requirement is not a vote count at all —
it is **severity**: block on a real finding, record a nitpick. The quorum
counts votes and cannot express that.

The workable version today: `min_pass = all`, and push the severity
discipline into each member's prompt — *"fail only on a defect in your axis
that would matter in production; everything else goes in your report, not
your verdict."* That raises the floor without pretending the engine
understands severity.

---

## 8. Event intake — Proven in shape

A graph does not have to be started by you. It can fire on a cron slot or a
filesystem watch, which makes it the back half of an event pipeline whose
front half is anything that can write a file.

```mermaid
graph LR
    GH[GitHub issue] --> SC[script: gh issue view > inbox.json]
    SC --> W((watch: inbox.json))
    W --> IN["intake: read the file, write a spec"]
    IN --> I[implement]
    I --> C[check]
    C --> R[review + commit]
```

The front half does not need a public endpoint. A real webhook does — a
tunnel, a forwarding service, something listening — and for work that can
wait five minutes, one cron line calling `gh issue list --search` and writing
a file is strictly simpler and stays local-first. Use a real webhook only
when the latency actually matters.

The intake node is a normal agent node whose job is to turn the file into a
spec via `spec_create` and `queue_add_spec`. Once it does, the rest of the
graph is pattern 1 and knows nothing about GitHub.

**The gap**: a watch-triggered graph is told *that* it fired, never *what*
fired it. The watcher logs the changed path and then calls the engine with
only the graph id (`src/watchers/mod.rs:98`). If your script writes to one
well-known path, the intake node can just read that path and the gap costs
you nothing — which is why this is a solved pattern today. It bites the
moment you watch a *directory* and need to know which file arrived.

---

## 9. Adversarial pair — Designed

A reviewer asks "is this right?". An adversary asks "how do I break this?".
Those are different jobs and they produce different findings.

```mermaid
graph LR
    I[implement] -->|pass| C[check]
    C -->|pass| A["adversary: break it"]
    A -->|"found a break"| I
    A -->|"could not break it"| R[review + commit]
```

Note the inverted edges: the adversary reports **fail** when it *succeeds* at
breaking the change. Its prompt should demand a concrete reproduction — an
input, a sequence, a race — not an opinion. An adversary that cannot produce
one has not found anything, and saying so must be a legitimate, unpunished
outcome, or it will invent findings to look useful.

Pairs naturally with pattern 8: an issue that arrives from outside is exactly
the case where you want something actively trying to falsify the fix before
it lands.

---

## Resuming and redirecting a run

Three levers, from least to most intrusive:

- **Retry or skip the stuck node.** `graph_continue` with
  `retry_current_node` re-runs the node the run stopped on; `skip_next_spec`
  moves past the spec. Use these first.
- **Start a spec at a node you choose.** When everything before a node is
  already right, a directed run (`graph_continue run_from_node`, or
  `canopy graph run <graph> --from <node>`) begins at that node and reuses the
  recorded output of each node upstream of it (the start node and everything
  downstream always run again, and its own old output is never reused). Use it
  after a crash, a rewired ensemble or a hand fix, instead of paying for the
  earlier nodes again. The run is refused, and names the offending node, when
  an upstream node's latest run is failed, error or interrupted, or when a
  prompt or check still needs output that was never recorded for the spec. On
  a queue-driven graph the directed run restarts the spec you name — by
  default the graph's current spec, a failed member before the next pending
  one — and the members queued
  before it are skipped, so the retry does not quietly turn into the next
  spec. The node you name always runs first, even when it is the node an
  ensemble fans out from.
- **Keep a related run warm.** Put specs that share context in one queue
  group. A grouped spec resumes the harness session its previous completed
  sibling used, so the repo is not analysed again from cold.

## Choosing a shape

| If the thing you fear is… | Use |
|---|---|
| Broken code landing | 1 — gated implement |
| One vendor's blind spot | 2 — cross-platform check |
| A quota ending overnight | 3 — resilience branch |
| Spending the good model on exploration | 4 — draft ensemble |
| Spending the good model on trivia | 5 — router by complexity |
| Paying at all before the free tiers are gone | 6 — cascade ensemble (free → paid) |
| A whole dimension going unreviewed | 7 — specialist panel |
| Work arriving while you sleep | 8 — event intake |
| A fix that is right but fragile | 9 — adversarial pair |

Shapes compose. The graph that drains this project's own backlog is 1 + 3,
and grows a 5 with the router.

---

## What the engine does not have yet

Collected from the patterns above, because a limitation stated once in
context is worth more than a roadmap:

- **Memory of a spent harness.** Nothing survives a spec boundary to say
  "cursor is exhausted until 4pm", so a cascade rediscovers it every time.
- **Severity in a quorum.** Members vote pass/fail; there is no way to say
  "this finding blocks, that one is a note".
- **The triggering event's payload.** A watch-fired graph gets the fact, not
  the path.

See [Recipes](recipes.md) for these shapes built end to end, with the actual
calls and prompts.
