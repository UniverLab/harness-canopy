//! Every literal the header can say: ambient lists, the deliberate joke list,
//! and the per-event templates (substituted by `Whimsg::render_event`).
//!
//! All text is literal — no rewording, no translation.

// ── Ambient lists ─────────────────────────────────────────────────

pub(crate) const IDLE: &[&str] = &[
    "the canopy rests",
    "roots deep, branches still",
    "listening to the understory",
    "the mycelium is passing notes",
    "moss grows at its own pace",
    "a fern unrolls, slowly",
    "the forest keeps its own time",
    "counting tree rings",
    "dappled light, no wind",
    "nothing to grow right now",
];

/// `{n}` is filled with the live branch count.
pub(crate) const BUSY: &[&str] = &[
    "{n} branches growing",
    "sap is flowing",
    "busy season in the grove",
    "every branch has a bird on it",
    "the canopy hums with {n} agents",
];

pub(crate) const SCROLL: &[&str] = &[
    "reading the tree rings",
    "deeper into the undergrowth",
    "following the grain",
    "tracing the roots",
];

// ── Jokes ─────────────────────────────────────────────────────────

pub(crate) const JOKES: &[&str] = &[
    "Photosynthesizing…",
    "Mycelializing…",
    "Germinating…",
    "Rhizoming…",
    "Fermenting…",
    "Unfurling…",
    "Barking up the right tree…",
    "composting old logs",
    "the cache is cold, bringing it a blanket",
    "renaming a variable for the fourth time",
    "a woodpecker is code-reviewing the trunk",
    "it compiles. the squirrels are suspicious",
    "the semicolon was in the other pocket",
    "leafing through the docs",
    "pruning dead code, gently",
    "branching out (git agrees)",
    "waiting for the kettle or the build, whichever boils first",
    "politely asking the borrow checker",
    "turning it off and on, with feeling",
];

// ── Event templates ───────────────────────────────────────────────
//
// `{agent}`/`{graph}` = the event's subject, `{spec}` = the spec's first word,
// `{node}` = the event's detail (a node id).

pub(crate) const EVENT_AGENT_SPAWNED: &[&str] = &[
    "{agent} takes root",
    "a new shoot: {agent}",
    "{agent} joins the grove",
];

pub(crate) const EVENT_AGENT_DONE: &[&str] = &[
    "{agent} bore fruit",
    "{agent} is done, another ring in the trunk",
];

pub(crate) const EVENT_AGENT_FAILED: &[&str] = &[
    "{agent} lost a branch, the log knows why",
    "{agent} stopped; its log has the story",
];

pub(crate) const EVENT_SPEC_COMPLETED: &[&str] =
    &["{graph}: {spec} is in the trunk", "{graph} grew {spec}"];

pub(crate) const EVENT_GRAPH_COMPLETED: &[&str] = &[
    "{graph} finished its queue",
    "{graph}: the whole season, harvested",
];

/// Shared by a blocked and a failed graph.
pub(crate) const EVENT_GRAPH_STOPPED: &[&str] =
    &["{graph} is waiting for you", "{graph} hit a rock at {node}"];

pub(crate) const EVENT_HOOK_MESSAGE: &[&str] = &["{graph} left you a message"];
