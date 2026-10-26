//! SQLite repositories for the Project Intelligence Layer.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::db::Database;

mod containment;
mod node_crud;
mod projects;
mod relation_targets;
mod search;
mod traversal;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelligenceNodeRecord {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub title: String,
    pub body: String,
    pub metadata: Option<String>,
    pub project_hash: Option<String>,
    pub session_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalSessionRecord {
    pub id: String,
    pub title: String,
    pub body: String,
    pub metadata: Option<String>,
    pub project_hash: Option<String>,
    pub session_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl OperationalSessionRecord {
    pub fn into_intelligence_node_record(self) -> IntelligenceNodeRecord {
        IntelligenceNodeRecord {
            id: self.id,
            kind: "session".to_string(),
            status: "noted".to_string(),
            title: self.title,
            body: self.body,
            metadata: self.metadata,
            project_hash: self.project_hash,
            session_id: self.session_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OperationalSessionInput {
    pub id: Option<String>,
    pub title: String,
    pub body: String,
    pub metadata: Option<serde_json::Value>,
    pub project_hash: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelligenceEdgeRecord {
    pub id: i64,
    pub from_node_id: String,
    pub to_node_id: String,
    pub relation: String,
    pub weight: f64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntelligenceRelationInput {
    pub to_node_id: String,
    pub relation: String,
    pub weight: Option<f64>,
}

/// Literal text replacement applied to a node's `body` on update.
///
/// The fragment is matched literally (never as a regular expression) and
/// must occur exactly once: zero matches or more than one match fails the
/// whole write, changing nothing, rather than guessing which occurrence
/// the caller meant.
#[derive(Debug, Clone, Deserialize)]
pub struct BodyReplace {
    pub fragment: String,
    pub replacement: String,
}

/// Input for [`Database::upsert_intelligence_node`].
///
/// Creation and partial update share this shape:
/// - **Create** (no `id`, or an `id` that matches nothing): `kind`, `title`
///   and `body` are required; the call fails without them.
/// - **Update** (an `id` that matches an existing node): every field is
///   optional. A field set to `Some` replaces the stored value; a field
///   left as `None` leaves the stored value untouched. Doubly-optional
///   fields (`metadata`, `project_hash`, `session_id`) distinguish "not
///   mentioned" (`None`, untouched) from "set to null" (`Some(None)`,
///   cleared where the column is nullable).
#[derive(Debug, Clone, Deserialize)]
pub struct IntelligenceNodeInput {
    pub id: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub body_replace: Option<BodyReplace>,
    pub metadata: Option<Option<serde_json::Value>>,
    pub project_hash: Option<Option<String>>,
    pub session_id: Option<Option<String>>,
    pub relations: Option<Vec<IntelligenceRelationInput>>,
}

/// One write-time duplicate candidate, reported without its body — a
/// caller recognises it by id/title/excerpt and fetches the full node
/// itself (via intelligence_search/intelligence_get_context) if it wants
/// more. CB45: a full-body candidate list blew the MCP result budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateCandidate {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub project_hash: Option<String>,
    pub excerpt: String,
}

/// Outcome of [`Database::upsert_intelligence_node`].
#[derive(Debug, Clone)]
pub struct UpsertResult {
    pub record: IntelligenceNodeRecord,
    pub created: bool,
    pub duplicates: Vec<DuplicateCandidate>,
    pub undeclared_references: Vec<String>,
    /// Fields that actually changed, so a caller does not have to read the
    /// node back to find out. `updated_at` is always present: every write
    /// bumps it. A `body_replace` that matched exactly once reports `body`.
    pub changed_fields: Vec<String>,
}

/// Resolved field values for the update path of
/// [`Database::upsert_intelligence_node`], plus the fields that changed.
struct IntelligenceNodeFinals {
    kind: String,
    status: String,
    title: String,
    body: String,
    metadata: Option<String>,
    project_hash: Option<String>,
    session_id: Option<String>,
    changed_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelligenceSearchResult {
    pub results: Vec<IntelligenceNodeRecord>,
    pub examined_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelligenceGraphWalk {
    pub root: IntelligenceNodeRecord,
    pub nodes: Vec<IntelligenceNodeRecord>,
    pub edges: Vec<IntelligenceEdgeRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntelligenceProjectDependencyRecord {
    pub from_node_id: String,
    pub from_title: String,
    pub from_project_hash: Option<String>,
    pub to_node_id: String,
    pub to_title: String,
    pub to_project_hash: Option<String>,
    pub relation: String,
    pub weight: f64,
    pub created_at: i64,
}

/// One project reached by [`Database::traverse_project_scope`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversedProject {
    pub hash: String,
    /// Relation of the edge that reached this project; `None` for the root.
    pub via_relation: Option<String>,
    /// BFS depth from the root (root = 0).
    pub depth: usize,
}

/// Bounded project-scope traversal result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraversalScope {
    pub projects: Vec<TraversedProject>,
    /// Number of projects reached excluding the root.
    pub reached: usize,
}

/// Hard bound on traversal depth: one query never walks further than this.
pub const MAX_TRAVERSAL_DEPTH: usize = 5;

/// Knowledge-node kinds. `project` is structural (CM9) and exempt from this validation.
pub const KNOWLEDGE_KINDS: &[&str] = &["fact", "pattern", "idea", "decision", "defect"];

/// Node status — applies to all nodes (knowledge + project).
pub const NODE_STATUSES: &[&str] = &["noted", "verified", "resolved", "superseded", "deprecated"];

/// Edge relation types for knowledge-node edges.
/// Project edges use their own vocabulary (`PROJECT_RELATIONS` in `domain/project.rs`).
pub const KNOWLEDGE_EDGE_RELATIONS: &[&str] = &[
    "supersedes",
    "contradicts",
    "evidence_for",
    "depends_on",
    "extends",
    "part_of",
];

/// Minimum lexical search score for a same-kind node to count as a
/// duplicate candidate at write time. A `pub const` so it can be tuned
/// without touching the query; a config-file setting can come later.
pub const DUPLICATE_SIMILARITY_THRESHOLD: i64 = 5;

/// Lexical duplicate detection does not catch paraphrase and does not
/// cross languages — surfaced in the upsert output wherever candidates
/// are reported.
pub const LEXICAL_DUPLICATE_LIMITATION: &str = "Lexical duplicate detection does not catch paraphrase and does not cross languages, so a bilingual corpus will pass duplicates through.";

/// Hard cap, in characters, on a duplicate candidate's reported excerpt —
/// applied after taking the first two non-empty lines, so a candidate whose
/// body is one long unbroken line still stays short. CB45.
const DUPLICATE_EXCERPT_MAX_CHARS: usize = 240;

/// First line or two of `body`, capped at [`DUPLICATE_EXCERPT_MAX_CHARS`] —
/// enough for a caller to recognise a duplicate candidate without the
/// response carrying its full body. CB45.
fn duplicate_excerpt(body: &str) -> String {
    let head: String = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    head.chars().take(DUPLICATE_EXCERPT_MAX_CHARS).collect()
}

pub fn validate_knowledge_kind(kind: &str) -> Result<()> {
    if kind == "project" || KNOWLEDGE_KINDS.contains(&kind) {
        Ok(())
    } else {
        anyhow::bail!(
            "unknown node kind '{kind}'; allowed: fact, pattern, idea, decision, defect (structural: project)"
        )
    }
}

pub fn validate_node_status(status: &str) -> Result<()> {
    if NODE_STATUSES.contains(&status) {
        Ok(())
    } else {
        anyhow::bail!(
            "unknown node status '{status}'; allowed: noted, verified, resolved, superseded, deprecated"
        )
    }
}

pub fn validate_knowledge_edge_relation(relation: &str) -> Result<()> {
    if KNOWLEDGE_EDGE_RELATIONS.contains(&relation) {
        Ok(())
    } else {
        anyhow::bail!(
            "unknown edge relation '{relation}'; allowed: {}",
            KNOWLEDGE_EDGE_RELATIONS.join(", ")
        )
    }
}

/// `file:line` citations in a body (`src/foo.rs:42`). Word-ish path chars
/// plus a colon plus a line number; the trailing boundary keeps `a:b`
/// prose and clock times (`12:30`) mostly out.
fn extract_citations(body: &str) -> Vec<String> {
    let re = regex::Regex::new(r"(?x)\b([A-Za-z0-9_./-]*[A-Za-z0-9_-]:\d+)\b")
        .expect("citation regex is static and valid");
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for cap in re.captures_iter(body) {
        let citation = cap[1].to_string();
        if seen.insert(citation.clone()) {
            out.push(citation);
        }
    }
    out
}

/// Mirror of the ranked-search weighting (title hit = 3, body hit = 1)
/// over the upsert query terms. A candidate at or above
/// [`DUPLICATE_SIMILARITY_THRESHOLD`] is reported.
fn lexical_duplicate_score(query_terms: &[String], candidate: &IntelligenceNodeRecord) -> i64 {
    let title = candidate.title.to_lowercase();
    let body = candidate.body.to_lowercase();
    let mut score = 0i64;
    for term in query_terms {
        if title.contains(term) {
            score += 3;
        }
        if body.contains(term) {
            score += 1;
        }
    }
    score
}

/// Node-id-shaped references in a body: `[[wiki-links]]`, full UUIDs, and
/// 8+ char hex strings (short citations like `→ f9f487d9`, `Ver 74bdcdd2`).
fn extract_node_id_references(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |candidate: String| {
        if !candidate.is_empty() && seen.insert(candidate.clone()) {
            out.push(candidate);
        }
    };

    let wiki =
        regex::Regex::new(r"\[\[([^\[\]]+)\]\]").expect("wiki-link regex is static and valid");
    for cap in wiki.captures_iter(body) {
        push(cap[1].trim().to_string());
    }
    let uuid = regex::Regex::new(
        r"\b([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})\b",
    )
    .expect("uuid regex is static and valid");
    for cap in uuid.captures_iter(body) {
        push(cap[1].to_string());
    }
    let hex = regex::Regex::new(r"\b([0-9a-fA-F]{8,64})\b").expect("hex regex is static and valid");
    for cap in hex.captures_iter(body) {
        let token = cap[1].to_string();
        // A pure decimal run is a number, not a node id.
        if token.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        push(token);
    }
    out
}

#[cfg(test)]
mod tests;
