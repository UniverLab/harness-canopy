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
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_db() -> Database {
        let dir = tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn sample_node_input(id: &str) -> IntelligenceNodeInput {
        IntelligenceNodeInput {
            id: Some(id.to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some(format!("Node {id}")),
            body: Some("Test body".to_string()),
            body_replace: None,
            project_hash: Some(Some("proj1".to_string())),
            session_id: None,
            metadata: None,
            relations: None,
        }
    }

    #[test]
    fn upsert_and_get_intelligence_node() {
        let db = test_db();
        let input = sample_node_input("node1");
        db.upsert_intelligence_node(input).unwrap();

        let retrieved = db.get_intelligence_node("node1").unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, "node1");
        assert_eq!(retrieved.title, "Node node1");
    }

    #[test]
    fn get_intelligence_node_not_found() {
        let db = test_db();
        let result = db.get_intelligence_node("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_intelligence_nodes_empty() {
        let db = test_db();
        let nodes = db.list_intelligence_nodes(None, 100).unwrap();
        assert!(nodes.is_empty());
    }

    #[test]
    fn search_includes_activity_log_entries() {
        let db = test_db();
        db.insert_activity_log_entry(
            "/tmp/bitacora-search",
            "hook",
            Some("hook-3"),
            "info",
            "zephyr deploy marker uniqueword",
            None,
        )
        .unwrap();
        let result = db
            .search_intelligence_nodes("zephyr uniqueword", None, 10)
            .unwrap();
        let activity_hit = result.results.iter().find(|r| r.kind == "activity");
        assert!(
            activity_hit.is_some(),
            "bitácora entry should surface in Knowledge search"
        );
        assert!(activity_hit
            .unwrap()
            .body
            .contains("zephyr deploy marker uniqueword"));
    }

    #[test]
    fn search_shows_activity_alongside_knowledge() {
        let db = test_db();
        for i in 0..3 {
            db.upsert_intelligence_node(sample_node_input(&format!("shared-{i}")))
                .unwrap();
        }
        db.insert_activity_log_entry(
            "/tmp/search-know-plus-activity",
            "sync",
            Some("agent-1"),
            "info",
            "shared activity event",
            None,
        )
        .unwrap();

        let result = db.search_intelligence_nodes("shared", None, 10).unwrap();
        assert!(
            result.results.iter().any(|r| r.kind != "activity"),
            "knowledge nodes should appear in search"
        );
        assert!(
            result.results.iter().any(|r| r.kind == "activity"),
            "activity entries should appear alongside knowledge in search"
        );
    }

    #[test]
    fn list_activity_kind_reads_bitacora_not_nodes() {
        let db = test_db();
        db.insert_activity_log_entry(
            "/tmp/bitacora-list",
            "user",
            None,
            "info",
            "bitacora list marker",
            None,
        )
        .unwrap();
        let nodes = db.list_intelligence_nodes(Some("activity"), 10).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].kind, "activity");
        assert!(nodes[0].body.contains("bitacora list marker"));
    }

    #[test]
    fn list_intelligence_nodes_merges_knowledge_and_activity() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("alpha"))
            .unwrap();
        db.insert_activity_log_entry(
            "/tmp/list-know-plus-activity",
            "sync",
            Some("agent-1"),
            "info",
            "activity alongside knowledge",
            None,
        )
        .unwrap();

        let nodes = db.list_intelligence_nodes(None, 10).unwrap();
        assert!(
            nodes.iter().any(|n| n.kind == "fact"),
            "knowledge nodes should appear in unfiltered list"
        );
        assert!(
            nodes.iter().any(|n| n.kind == "activity"),
            "activity entries should appear alongside knowledge"
        );
    }

    #[test]
    fn list_intelligence_nodes_with_nodes() {
        let db = test_db();
        let input1 = sample_node_input("node1");
        let input2 = sample_node_input("node2");
        db.upsert_intelligence_node(input1).unwrap();
        db.upsert_intelligence_node(input2).unwrap();

        let nodes = db.list_intelligence_nodes(None, 100).unwrap();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn list_intelligence_nodes_by_kind() {
        let db = test_db();
        let mut input1 = sample_node_input("node1");
        input1.kind = Some("fact".to_string());
        let mut input2 = sample_node_input("node2");
        input2.kind = Some("pattern".to_string());
        db.upsert_intelligence_node(input1).unwrap();
        db.upsert_intelligence_node(input2).unwrap();

        let nodes = db.list_intelligence_nodes(Some("fact"), 100).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id, "node1");
    }

    #[test]
    fn delete_intelligence_node() {
        let db = test_db();
        let input = sample_node_input("node1");
        db.upsert_intelligence_node(input).unwrap();

        let relations_removed = db.delete_intelligence_node("node1").unwrap();
        assert_eq!(relations_removed, Some(0));
        let retrieved = db.get_intelligence_node("node1").unwrap();
        assert!(retrieved.is_none());
    }

    #[test]
    fn delete_intelligence_node_missing_returns_none() {
        let db = test_db();
        let result = db.delete_intelligence_node("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delete_intelligence_node_removes_edges_on_both_sides() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("a")).unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            relations: Some(vec![IntelligenceRelationInput {
                to_node_id: "a".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
            ..sample_node_input("b")
        })
        .unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            relations: Some(vec![IntelligenceRelationInput {
                to_node_id: "b".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
            ..sample_node_input("c")
        })
        .unwrap();

        // "b" sits in the middle of a -> b, b -> c: two edges touch it.
        let relations_removed = db.delete_intelligence_node("b").unwrap();
        assert_eq!(relations_removed, Some(2));
        assert!(db.get_intelligence_node("b").unwrap().is_none());
        assert!(db.get_intelligence_node("a").unwrap().is_some());
        assert!(db.get_intelligence_node("c").unwrap().is_some());
        assert!(db.list_recent_intelligence_edges("a").unwrap().is_empty());
        assert!(db.list_recent_intelligence_edges("c").unwrap().is_empty());
    }

    #[test]
    fn graph_walk_after_deleting_middle_node_does_not_reference_it() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("a")).unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            relations: Some(vec![IntelligenceRelationInput {
                to_node_id: "a".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
            ..sample_node_input("b")
        })
        .unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            relations: Some(vec![IntelligenceRelationInput {
                to_node_id: "b".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
            ..sample_node_input("c")
        })
        .unwrap();

        db.delete_intelligence_node("b").unwrap();

        let from_a = db.walk_intelligence_graph("a", 3).unwrap().unwrap();
        assert!(!from_a.nodes.iter().any(|n| n.id == "b"));
        assert!(!from_a
            .edges
            .iter()
            .any(|e| e.from_node_id == "b" || e.to_node_id == "b"));

        let from_c = db.walk_intelligence_graph("c", 3).unwrap().unwrap();
        assert!(!from_c.nodes.iter().any(|n| n.id == "b"));
        assert!(!from_c
            .edges
            .iter()
            .any(|e| e.from_node_id == "b" || e.to_node_id == "b"));
    }

    #[test]
    fn delete_intelligence_edge_removes_single_relation_only() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("a")).unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            relations: Some(vec![IntelligenceRelationInput {
                to_node_id: "a".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
            ..sample_node_input("b")
        })
        .unwrap();

        let edges = db.list_recent_intelligence_edges("a").unwrap();
        assert_eq!(edges.len(), 1);
        let edge_id = edges[0].id;

        let removed = db.delete_intelligence_edge(edge_id).unwrap();
        assert!(removed);
        assert!(db.get_intelligence_edge(edge_id).unwrap().is_none());
        assert!(db.get_intelligence_node("a").unwrap().is_some());
        assert!(db.get_intelligence_node("b").unwrap().is_some());
    }

    #[test]
    fn delete_intelligence_edge_missing_returns_false() {
        let db = test_db();
        let removed = db.delete_intelligence_edge(999999).unwrap();
        assert!(!removed);
    }

    #[test]
    fn list_intelligence_projects_empty() {
        let db = test_db();
        let projects = db.list_intelligence_projects(None, 100).unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn list_intelligence_projects_with_projects() {
        let db = test_db();
        let mut input1 = sample_node_input("proj1");
        input1.kind = Some("project".to_string());
        let mut input2 = sample_node_input("proj2");
        input2.kind = Some("project".to_string());
        db.upsert_intelligence_node(input1).unwrap();
        db.upsert_intelligence_node(input2).unwrap();

        let projects = db.list_intelligence_projects(None, 100).unwrap();
        assert_eq!(projects.len(), 2);
    }

    #[test]
    fn resolve_prefix_unique_match() {
        let db = test_db();
        let full = "3a476c63-6b4a-4860-9c18-1c784b40a4b2";
        // A decoy that shares the first six characters but diverges inside the
        // 8-char prefix — the LIKE must actually discriminate, not just return
        // the only row in the table.
        for id in [full, "3a476c99-dead-4860-9c18-1c784b40a4b2"] {
            db.upsert_intelligence_node(IntelligenceNodeInput {
                id: Some(id.to_string()),
                kind: Some("fact".to_string()),
                status: None,
                title: Some("Original".to_string()),
                body: Some("body".to_string()),
                body_replace: None,
                metadata: None,
                project_hash: Some(Some("proj-a".to_string())),
                session_id: None,
                relations: None,
            })
            .unwrap();
        }
        let resolved = db.resolve_node_id_by_prefix("3a476c63").unwrap();
        assert_eq!(resolved, Some(full.to_string()));
    }

    #[test]
    fn resolve_prefix_ambiguous() {
        let db = test_db();
        let id1 = "abc123-0000-0000-0000-000000000001";
        let id2 = "abc456-0000-0000-0000-000000000002";
        for id in [id1, id2] {
            db.upsert_intelligence_node(IntelligenceNodeInput {
                id: Some(id.to_string()),
                kind: Some("fact".to_string()),
                status: None,
                title: Some(format!("Node {id}")),
                body: Some("body".to_string()),
                body_replace: None,
                metadata: None,
                project_hash: Some(Some("proj-a".to_string())),
                session_id: None,
                relations: None,
            })
            .unwrap();
        }
        let err = db.resolve_node_id_by_prefix("abc").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Ambiguous"),
            "expected ambiguous error, got: {msg}"
        );
        assert!(msg.contains(id1), "expected candidate {id1} in: {msg}");
        assert!(msg.contains(id2), "expected candidate {id2} in: {msg}");
    }

    #[test]
    fn resolve_prefix_no_match() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("xyz-0000-0000-0000-000000000001".to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("X".to_string()),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some("proj-a".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
        let resolved = db.resolve_node_id_by_prefix("abc").unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn resolve_prefix_treats_like_wildcards_literally() {
        let db = test_db();
        // Two ids that differ only at position 2. A naive `LIKE prefix || '%'`
        // where `prefix` contains '_' would match BOTH and raise a bogus
        // "ambiguous" error; escaped, "ab_cd" matches neither.
        for id in [
            "ab1cd-0000-0000-0000-000000000001",
            "ab2cd-0000-0000-0000-000000000002",
        ] {
            db.upsert_intelligence_node(IntelligenceNodeInput {
                id: Some(id.to_string()),
                kind: Some("fact".to_string()),
                status: None,
                title: Some("N".to_string()),
                body: Some("body".to_string()),
                body_replace: None,
                metadata: None,
                project_hash: Some(Some("proj-a".to_string())),
                session_id: None,
                relations: None,
            })
            .unwrap();
        }
        let resolved = db.resolve_node_id_by_prefix("ab_cd").unwrap();
        assert_eq!(resolved, None, "'_' must be a literal, not a wildcard");
    }

    #[test]
    fn upsert_returns_created_true_for_new_node() {
        let db = test_db();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some("new-node-1".to_string()),
                kind: Some("fact".to_string()),
                status: None,
                title: Some("First".to_string()),
                body: Some("body".to_string()),
                body_replace: None,
                metadata: None,
                project_hash: Some(Some("proj-a".to_string())),
                session_id: None,
                relations: None,
            })
            .unwrap();
        assert!(result.created, "expected created=true for new node");
    }

    #[test]
    fn upsert_returns_created_false_for_existing_node() {
        let db = test_db();
        let id = "existing-node-1";
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some(id.to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("First".to_string()),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some("proj-a".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some(id.to_string()),
                kind: Some("fact".to_string()),
                status: None,
                title: Some("Updated".to_string()),
                body: Some("body2".to_string()),
                body_replace: None,
                metadata: None,
                project_hash: Some(Some("proj-a".to_string())),
                session_id: None,
                relations: None,
            })
            .unwrap();
        assert!(!result.created, "expected created=false for update");
        assert_eq!(result.record.title, "Updated");
    }

    #[test]
    fn intelligence_upsert_with_prefix_updates_existing() {
        let db = test_db();
        let full = "3a476c63-6b4a-4860-9c18-1c784b40a4b2";
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some(full.to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("Original".to_string()),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some("proj-a".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
        // Simulate handler prefix resolution: 8-char prefix should resolve to full.
        let resolved = db.resolve_node_id_by_prefix("3a476c63").unwrap().unwrap();
        assert_eq!(resolved, full);
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some(resolved),
                kind: Some("fact".to_string()),
                status: None,
                title: Some("Updated via prefix".to_string()),
                body: Some("body2".to_string()),
                body_replace: None,
                metadata: None,
                project_hash: Some(Some("proj-a".to_string())),
                session_id: None,
                relations: None,
            })
            .unwrap();
        assert!(
            !result.created,
            "prefix upsert should be update, not create"
        );
        assert_eq!(result.record.id, full);
        assert_eq!(result.record.title, "Updated via prefix");
        // Ensure no duplicate was created.
        let all = db.list_intelligence_nodes(None, 100).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn intelligence_upsert_with_ambiguous_prefix_errors() {
        let db = test_db();
        let id1 = "abc123-0000-0000-0000-000000000001";
        let id2 = "abc456-0000-0000-0000-000000000002";
        for id in [id1, id2] {
            db.upsert_intelligence_node(IntelligenceNodeInput {
                id: Some(id.to_string()),
                kind: Some("fact".to_string()),
                status: None,
                title: Some(format!("Node {id}")),
                body: Some("body".to_string()),
                body_replace: None,
                metadata: None,
                project_hash: Some(Some("proj-a".to_string())),
                session_id: None,
                relations: None,
            })
            .unwrap();
        }
        let err = db.resolve_node_id_by_prefix("abc").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Ambiguous"));
        assert!(msg.contains(id1));
        assert!(msg.contains(id2));
    }

    #[test]
    fn intelligence_graph_walk_with_prefix() {
        let db = test_db();
        let full = "deadbeef-0000-0000-0000-000000000001";
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some(full.to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("Root".to_string()),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some("proj-a".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
        let resolved = db.resolve_node_id_by_prefix("deadbeef").unwrap().unwrap();
        let walk = db.walk_intelligence_graph(&resolved, 1).unwrap().unwrap();
        assert_eq!(walk.root.id, full);
    }

    #[test]
    fn intelligence_delete_node_with_prefix() {
        let db = test_db();
        let full = "feedface-0000-0000-0000-000000000001";
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some(full.to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("To delete".to_string()),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some("proj-a".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
        let resolved = db.resolve_node_id_by_prefix("feedface").unwrap().unwrap();
        let removed = db.delete_intelligence_node(&resolved).unwrap();
        assert_eq!(removed, Some(0));
        assert!(db.get_intelligence_node(full).unwrap().is_none());
    }

    #[test]
    fn operational_migration_moves_session_nodes_to_dedicated_table() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.db");

        // Seed pre-migration state directly in `intelligence_nodes`: 3
        // operational ids (the id prefixes the three writers use) plus 2
        // knowledge nodes. Before CM8 the writers wrote them this way.
        // Raw SQL (not upsert): these rows predate the CM10 kind vocabulary,
        // and the test guards the migration, not the write path.
        {
            let db = Database::new(&path).unwrap();
            {
                let conn = db.conn.lock().unwrap();
                for id in ["run:abc", "sync:/tmp:agent1", "launchpad:uuid-1"] {
                    conn.execute(
                        "INSERT INTO intelligence_nodes (id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at)
                         VALUES (?1, 'session', 'noted', ?2, 'op body', NULL, NULL, NULL, strftime('%s','now'), strftime('%s','now'))",
                        rusqlite::params![id, format!("Op {id}")],
                    )
                    .unwrap();
                }
            }
            for (id, kind) in [("fact1", "fact"), ("pattern1", "pattern")] {
                db.upsert_intelligence_node(IntelligenceNodeInput {
                    id: Some(id.to_string()),
                    kind: Some(kind.to_string()),
                    status: None,
                    title: Some(format!("Knowledge {id}")),
                    body: Some("knowledge body".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                })
                .unwrap();
            }
        }

        // Reopening runs `init()`, which runs the CM8 migration. This test
        // guards the production migration in `db/mod.rs` — inlining the SQL
        // here would let a deleted migration still pass.
        let db = Database::new(&path).unwrap();

        let ops = db.list_operational_sessions(10).unwrap();
        assert_eq!(ops.len(), 3, "operational_sessions should have 3 rows");
        let ids: std::collections::HashSet<_> = ops.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains("run:abc"));
        assert!(ids.contains("sync:/tmp:agent1"));
        assert!(ids.contains("launchpad:uuid-1"));
        // Row content is carried across unchanged.
        let run_row = db.get_operational_session("run:abc").unwrap().unwrap();
        assert_eq!(run_row.title, "Op run:abc");
        assert_eq!(run_row.body, "op body");

        let knowledge = db.list_intelligence_nodes(None, 10).unwrap();
        assert_eq!(
            knowledge.len(),
            2,
            "intelligence_nodes should have 2 rows after migration"
        );
        let k_ids: std::collections::HashSet<_> = knowledge.iter().map(|r| r.id.as_str()).collect();
        assert!(k_ids.contains("fact1"));
        assert!(k_ids.contains("pattern1"));

        // Idempotent: reopening again (migration re-runs) is a no-op.
        drop(db);
        let db = Database::new(&path).unwrap();
        assert_eq!(db.list_operational_sessions(10).unwrap().len(), 3);
        assert_eq!(db.list_intelligence_nodes(None, 10).unwrap().len(), 2);
    }

    #[test]
    fn run_upsert_writes_to_operational_sessions_not_intelligence_nodes() {
        let db = test_db();
        let (rec, _) = db
            .upsert_operational_session(OperationalSessionInput {
                id: Some("run:test-run-1".to_string()),
                title: "Run title".to_string(),
                body: "Run body".to_string(),
                metadata: Some(serde_json::json!({"source":"run"})),
                project_hash: None,
                session_id: Some("run:test-run-1".to_string()),
            })
            .unwrap();
        assert_eq!(rec.id, "run:test-run-1");
        assert!(db
            .get_operational_session("run:test-run-1")
            .unwrap()
            .is_some());
        assert!(db
            .get_intelligence_node("run:test-run-1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn search_operational_sessions_returns_operational_records() {
        let db = test_db();
        db.upsert_operational_session(OperationalSessionInput {
            id: Some("run:search-me".to_string()),
            title: "Searchable Mission Title".to_string(),
            body: "body".to_string(),
            metadata: None,
            project_hash: None,
            session_id: None,
        })
        .unwrap();
        db.upsert_operational_session(OperationalSessionInput {
            id: Some("run:other".to_string()),
            title: "Unrelated".to_string(),
            body: "other body".to_string(),
            metadata: None,
            project_hash: None,
            session_id: None,
        })
        .unwrap();

        let results = db
            .search_operational_sessions("Searchable Mission", 10)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "run:search-me");
    }

    #[test]
    fn prune_operational_sessions_removes_old_records() {
        let db = test_db();
        let old_ts = chrono::Utc::now().timestamp() - (31 * 86400);
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO operational_sessions (id, title, body, metadata, project_hash, session_id, created_at, updated_at) VALUES (?1, ?2, ?3, NULL, NULL, NULL, ?4, ?4)",
                rusqlite::params!["run:old", "Old", "old body", old_ts],
            )
            .unwrap();
        }
        db.upsert_operational_session(OperationalSessionInput {
            id: Some("run:recent".to_string()),
            title: "Recent".to_string(),
            body: "recent".to_string(),
            metadata: None,
            project_hash: None,
            session_id: None,
        })
        .unwrap();

        let pruned = db.prune_operational_sessions(30).unwrap();
        assert_eq!(pruned, 1);
        assert!(db.get_operational_session("run:old").unwrap().is_none());
        assert!(db.get_operational_session("run:recent").unwrap().is_some());
    }

    #[test]
    fn intelligence_search_excludes_operational_records() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("fact-search".to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("UniqueSearchTerm Fact Title".to_string()),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap();
        db.upsert_operational_session(OperationalSessionInput {
            id: Some("run:op-search".to_string()),
            title: "UniqueSearchTerm Operational Title".to_string(),
            body: "op body".to_string(),
            metadata: None,
            project_hash: None,
            session_id: None,
        })
        .unwrap();

        let result = db
            .search_intelligence_nodes("UniqueSearchTerm", None, 10)
            .unwrap();
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].id, "fact-search");
    }

    #[test]
    fn launchpad_operational_sessions_are_searchable_by_workdir() {
        let db = test_db();
        let workdir = "/tmp/launchpad-test";
        db.upsert_operational_session(OperationalSessionInput {
            id: Some("launchpad:test-uuid".to_string()),
            title: "Test Launchpad Mission".to_string(),
            body: "Launchpad body".to_string(),
            metadata: Some(serde_json::json!({
                "source": "launchpad",
                "workdir": workdir,
                "mode": "new",
                "summary": "summary text"
            })),
            project_hash: None,
            session_id: Some("launchpad:test-uuid".to_string()),
        })
        .unwrap();

        // LaunchpadDialog::for_workdir searches operational_sessions by workdir;
        // verify the underlying search returns the record.
        let results = db.search_operational_sessions(workdir, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Test Launchpad Mission");
        let meta: serde_json::Value =
            serde_json::from_str(results[0].metadata.as_deref().unwrap()).unwrap();
        assert_eq!(
            meta.get("source").and_then(|v| v.as_str()),
            Some("launchpad")
        );
    }

    // ── CM10: knowledge-node schema ──────────────────────────────────

    #[test]
    fn upsert_rejects_unknown_kind() {
        let db = test_db();
        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                kind: Some("unknown".to_string()),
                ..sample_node_input("bad-kind")
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown node kind"),
            "expected kind rejection, got: {err}"
        );
    }

    #[test]
    fn upsert_rejects_unknown_status() {
        let db = test_db();
        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                status: Some("bogus".to_string()),
                ..sample_node_input("bad-status")
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown node status"),
            "expected status rejection, got: {err}"
        );
    }

    #[test]
    fn upsert_defaults_status_to_noted() {
        let db = test_db();
        let result = db
            .upsert_intelligence_node(sample_node_input("default-status"))
            .unwrap();
        assert_eq!(result.record.status, "noted");
    }

    #[test]
    fn upsert_rejects_superseded_without_supersedes_edge() {
        let db = test_db();
        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                status: Some("superseded".to_string()),
                ..sample_node_input("sup-no-edge")
            })
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("superseded"), "expected superseded in: {msg}");
        assert!(msg.contains("supersedes"), "expected supersedes in: {msg}");
    }

    #[test]
    fn upsert_accepts_superseded_with_supersedes_edge() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("sup-a"))
            .unwrap();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                status: Some("superseded".to_string()),
                relations: Some(vec![IntelligenceRelationInput {
                    to_node_id: "sup-a".to_string(),
                    relation: "supersedes".to_string(),
                    weight: None,
                }]),
                ..sample_node_input("sup-b")
            })
            .unwrap();
        assert_eq!(result.record.status, "superseded");
    }

    #[test]
    fn upsert_rejects_unknown_edge_relation() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("rel-target"))
            .unwrap();
        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                relations: Some(vec![IntelligenceRelationInput {
                    to_node_id: "rel-target".to_string(),
                    relation: "related_to".to_string(),
                    weight: None,
                }]),
                ..sample_node_input("rel-src")
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown edge relation"),
            "expected edge rejection, got: {err}"
        );
    }

    #[test]
    fn upsert_create_with_unknown_second_relation_writes_nothing() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("t1"))
            .unwrap();
        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                relations: Some(vec![
                    IntelligenceRelationInput {
                        to_node_id: "t1".to_string(),
                        relation: "extends".to_string(),
                        weight: None,
                    },
                    IntelligenceRelationInput {
                        to_node_id: "does-not-exist-zzz".to_string(),
                        relation: "extends".to_string(),
                        weight: None,
                    },
                ]),
                ..sample_node_input("new-x")
            })
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("relation target 'does-not-exist-zzz' matches no intelligence node"),
            "expected unknown-target rejection, got: {err}"
        );
        // The whole upsert rolled back: no node, no partial edge from the
        // valid first relation, and the target gained no edge either.
        assert!(db.get_intelligence_node("new-x").unwrap().is_none());
        assert_eq!(db.list_intelligence_nodes(None, 100).unwrap().len(), 1);
        assert!(db
            .list_recent_intelligence_edges("new-x")
            .unwrap()
            .is_empty());
        assert!(db.list_recent_intelligence_edges("t1").unwrap().is_empty());
    }

    #[test]
    fn upsert_create_with_unique_prefix_relation_stores_full_id() {
        let db = test_db();
        let full = "aaaaaaaa-1111-2222-3333-444444444444";
        db.upsert_intelligence_node(sample_node_input(full))
            .unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            relations: Some(vec![IntelligenceRelationInput {
                to_node_id: full[..8].to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
            ..sample_node_input("new-y")
        })
        .unwrap();
        let edges = db.list_recent_intelligence_edges("new-y").unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to_node_id, full);
    }

    #[test]
    fn upsert_create_with_ambiguous_prefix_writes_nothing() {
        let db = test_db();
        let id1 = "abc12345-0000-0000-0000-000000000001";
        let id2 = "abc12345-0000-0000-0000-000000000002";
        db.upsert_intelligence_node(sample_node_input(id1)).unwrap();
        db.upsert_intelligence_node(sample_node_input(id2)).unwrap();
        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                relations: Some(vec![IntelligenceRelationInput {
                    to_node_id: "abc12345".to_string(),
                    relation: "extends".to_string(),
                    weight: None,
                }]),
                ..sample_node_input("amb-x")
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("Ambiguous"),
            "expected ambiguous-prefix rejection, got: {err}"
        );
        assert!(db.get_intelligence_node("amb-x").unwrap().is_none());
        assert_eq!(db.list_intelligence_nodes(None, 100).unwrap().len(), 2);
        assert!(db.list_recent_intelligence_edges(id1).unwrap().is_empty());
        assert!(db.list_recent_intelligence_edges(id2).unwrap().is_empty());
    }

    #[test]
    fn upsert_update_with_unknown_relation_keeps_old_body_and_edges() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("t1"))
            .unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            body: Some("v1".to_string()),
            relations: Some(vec![IntelligenceRelationInput {
                to_node_id: "t1".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
            ..sample_node_input("u1")
        })
        .unwrap();
        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                body: Some("v2".to_string()),
                relations: Some(vec![IntelligenceRelationInput {
                    to_node_id: "unknown-id-9999".to_string(),
                    relation: "extends".to_string(),
                    weight: None,
                }]),
                ..sample_node_input("u1")
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("matches no intelligence node"),
            "expected unknown-target rejection, got: {err}"
        );
        // The update path is transactional: the old body survives and the
        // previous edge was not dropped by the failing replacement.
        assert_eq!(db.get_intelligence_node("u1").unwrap().unwrap().body, "v1");
        let edges = db.list_recent_intelligence_edges("u1").unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to_node_id, "t1");
    }

    #[test]
    fn duplicate_detection_shared_citations() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            body: Some("see src/foo.rs:42 for the implementation".to_string()),
            body_replace: None,
            ..sample_node_input("cite-a")
        })
        .unwrap();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                body: Some("also see src/foo.rs:42".to_string()),
                body_replace: None,
                ..sample_node_input("cite-b")
            })
            .unwrap();
        assert!(
            result.duplicates.iter().any(|n| n.id == "cite-a"),
            "expected cite-a among duplicates, got: {:?}",
            result.duplicates.iter().map(|n| &n.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn duplicate_detection_dedupes_a_neighbour_cited_twice() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            body: Some("see src/foo.rs:10 and src/bar.rs:20".to_string()),
            body_replace: None,
            ..sample_node_input("dedupe-a")
        })
        .unwrap();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                body: Some("also see src/foo.rs:10 and src/bar.rs:20".to_string()),
                body_replace: None,
                ..sample_node_input("dedupe-b")
            })
            .unwrap();
        assert_eq!(
            result
                .duplicates
                .iter()
                .filter(|n| n.id == "dedupe-a")
                .count(),
            1,
            "a neighbour reaching both citations must be reported once, got: {:?}",
            result.duplicates.iter().map(|n| &n.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn duplicate_detection_lexical_similarity() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            title: Some("Database connection cache".to_string()),
            ..sample_node_input("lex-a")
        })
        .unwrap();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                title: Some("Database connection cache strategy".to_string()),
                ..sample_node_input("lex-b")
            })
            .unwrap();
        assert!(
            result.duplicates.iter().any(|n| n.id == "lex-a"),
            "expected lex-a among duplicates, got: {:?}",
            result.duplicates.iter().map(|n| &n.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn duplicate_candidate_excludes_full_body() {
        let db = test_db();
        let big_body = format!("see src/cb45.rs:1 for details. {}", "y".repeat(20_000));
        db.upsert_intelligence_node(IntelligenceNodeInput {
            body: Some(big_body),
            body_replace: None,
            ..sample_node_input("cb45-big")
        })
        .unwrap();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                body: Some("also see src/cb45.rs:1".to_string()),
                body_replace: None,
                ..sample_node_input("cb45-new")
            })
            .unwrap();
        let candidate = result
            .duplicates
            .iter()
            .find(|c| c.id == "cb45-big")
            .expect("expected cb45-big among duplicates");
        assert!(
            candidate.excerpt.len() < 300,
            "excerpt must be short, got {} chars",
            candidate.excerpt.len()
        );
        assert!(
            !candidate.excerpt.contains(&"y".repeat(1000)),
            "excerpt must not carry the candidate's large body"
        );
    }

    #[test]
    fn undeclared_reference_warning() {
        let db = test_db();
        let node_a_id = "aaaaaaaa-1111-2222-3333-444444444444";
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some(node_a_id.to_string()),
            ..sample_node_input("undecl-a")
        })
        .unwrap();
        // `undecl-c` exists so `undecl-b` also exercises a declared edge
        // alongside its undeclared reference.
        db.upsert_intelligence_node(sample_node_input("undecl-c"))
            .unwrap();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                body: Some(format!("this builds on {node_a_id}")),
                body_replace: None,
                relations: Some(vec![IntelligenceRelationInput {
                    to_node_id: "undecl-c".to_string(),
                    relation: "extends".to_string(),
                    weight: None,
                }]),
                ..sample_node_input("undecl-b")
            })
            .unwrap();
        assert!(
            result
                .undeclared_references
                .iter()
                .any(|id| id == node_a_id),
            "expected undeclared ref to {node_a_id}, got: {:?}",
            result.undeclared_references
        );
    }

    #[test]
    fn no_warning_for_node_without_edges() {
        let db = test_db();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                body: Some("a fresh finding with no links".to_string()),
                body_replace: None,
                ..sample_node_input("orphan")
            })
            .unwrap();
        assert!(
            result.undeclared_references.is_empty(),
            "expected no warning, got: {:?}",
            result.undeclared_references
        );
    }

    #[test]
    fn undeclared_reference_warns_even_without_declared_edges() {
        let db = test_db();
        let referenced = "bbbbbbbb-1111-2222-3333-444444444444";
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some(referenced.to_string()),
            ..sample_node_input("referenced")
        })
        .unwrap();

        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                body: Some(format!("this builds on {referenced}")),
                body_replace: None,
                relations: None,
                ..sample_node_input("unlinked")
            })
            .unwrap();

        assert_eq!(result.undeclared_references, vec![referenced.to_string()]);
    }

    #[test]
    fn project_kind_exempt_from_kind_validation() {
        let db = test_db();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                kind: Some("project".to_string()),
                ..sample_node_input("proj-exempt")
            })
            .unwrap();
        assert_eq!(result.record.kind, "project");
    }

    #[test]
    fn status_preserved_on_update_when_not_provided() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            status: Some("verified".to_string()),
            ..sample_node_input("keep-status")
        })
        .unwrap();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                title: Some("Updated title".to_string()),
                ..sample_node_input("keep-status")
            })
            .unwrap();
        assert!(!result.created);
        assert_eq!(result.record.status, "verified");
    }

    // ── CM11: partial update ───────────────────────────────────────

    #[test]
    fn partial_update_title_only_leaves_other_fields_unchanged() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("partial-1".to_string()),
            kind: Some("fact".to_string()),
            status: Some("verified".to_string()),
            title: Some("Original title".to_string()),
            body: Some("Original body".to_string()),
            body_replace: None,
            metadata: Some(Some(serde_json::json!({"k": "v"}))),
            project_hash: Some(Some("proj-a".to_string())),
            session_id: Some(Some("sess-1".to_string())),
            relations: None,
        })
        .unwrap();

        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some("partial-1".to_string()),
                kind: None,
                status: None,
                title: Some("Updated title".to_string()),
                body: None,
                body_replace: None,
                metadata: None,
                project_hash: None,
                session_id: None,
                relations: None,
            })
            .unwrap();

        assert!(!result.created);
        assert_eq!(result.record.title, "Updated title");
        assert_eq!(result.record.body, "Original body");
        assert_eq!(result.record.kind, "fact");
        assert_eq!(result.record.status, "verified");
        assert_eq!(result.record.metadata.as_deref(), Some(r#"{"k":"v"}"#));
        assert_eq!(result.record.project_hash.as_deref(), Some("proj-a"));
        assert_eq!(result.record.session_id.as_deref(), Some("sess-1"));
        assert!(result.changed_fields.contains(&"title".to_string()));
        assert!(result.changed_fields.contains(&"updated_at".to_string()));
        assert!(!result.changed_fields.contains(&"body".to_string()));
        assert!(!result.changed_fields.contains(&"kind".to_string()));
        assert!(!result.changed_fields.contains(&"metadata".to_string()));
    }

    #[test]
    fn partial_update_omitting_relations_leaves_them_in_place() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("rel-keep-target"))
            .unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            relations: Some(vec![IntelligenceRelationInput {
                to_node_id: "rel-keep-target".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
            ..sample_node_input("rel-keep-src")
        })
        .unwrap();

        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some("rel-keep-src".to_string()),
                kind: None,
                status: None,
                title: Some("New title".to_string()),
                body: None,
                body_replace: None,
                metadata: None,
                project_hash: None,
                session_id: None,
                relations: None,
            })
            .unwrap();

        assert!(!result.created);
        assert!(
            !result.changed_fields.contains(&"relations".to_string()),
            "omitted relations must not be reported as changed: {:?}",
            result.changed_fields
        );
        let edges = db
            .list_recent_intelligence_edges("rel-keep-target")
            .unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from_node_id, "rel-keep-src");
    }

    #[test]
    fn partial_update_empty_relations_clears_them() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("rel-clear-target"))
            .unwrap();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            relations: Some(vec![IntelligenceRelationInput {
                to_node_id: "rel-clear-target".to_string(),
                relation: "extends".to_string(),
                weight: None,
            }]),
            ..sample_node_input("rel-clear-src")
        })
        .unwrap();

        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some("rel-clear-src".to_string()),
                kind: None,
                status: None,
                title: None,
                body: None,
                body_replace: None,
                metadata: None,
                project_hash: None,
                session_id: None,
                relations: Some(vec![]),
            })
            .unwrap();

        assert!(!result.created);
        assert!(result.changed_fields.contains(&"relations".to_string()));
        assert!(db
            .list_recent_intelligence_edges("rel-clear-target")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn body_replace_single_match_changes_only_fragment() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("br-1".to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("T".to_string()),
            body: Some("foo bar baz".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap();

        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some("br-1".to_string()),
                kind: None,
                status: None,
                title: None,
                body: None,
                body_replace: Some(BodyReplace {
                    fragment: "bar".to_string(),
                    replacement: "qux".to_string(),
                }),
                metadata: None,
                project_hash: None,
                session_id: None,
                relations: None,
            })
            .unwrap();

        assert!(!result.created);
        assert_eq!(result.record.body, "foo qux baz");
        assert_eq!(
            result.changed_fields,
            vec!["body".to_string(), "updated_at".to_string()]
        );
    }

    #[test]
    fn body_replace_zero_matches_fails_and_changes_nothing() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("br-zero".to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("T".to_string()),
            body: Some("foo bar".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap();

        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some("br-zero".to_string()),
                kind: None,
                status: None,
                title: None,
                body: None,
                body_replace: Some(BodyReplace {
                    fragment: "xyz".to_string(),
                    replacement: "qux".to_string(),
                }),
                metadata: None,
                project_hash: None,
                session_id: None,
                relations: None,
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "expected a not-found refusal, got: {err}"
        );
        let after = db.get_intelligence_node("br-zero").unwrap().unwrap();
        assert_eq!(after.body, "foo bar");
    }

    #[test]
    fn body_replace_two_matches_fails_and_changes_nothing() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("br-two".to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("T".to_string()),
            body: Some("bar foo bar".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap();

        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some("br-two".to_string()),
                kind: None,
                status: None,
                title: None,
                body: None,
                body_replace: Some(BodyReplace {
                    fragment: "bar".to_string(),
                    replacement: "qux".to_string(),
                }),
                metadata: None,
                project_hash: None,
                session_id: None,
                relations: None,
            })
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ambiguous") && msg.contains("2 times"),
            "expected an ambiguous-target refusal naming the count, got: {msg}"
        );
        let after = db.get_intelligence_node("br-two").unwrap().unwrap();
        assert_eq!(after.body, "bar foo bar");
    }

    #[test]
    fn body_and_body_replace_are_mutually_exclusive() {
        let db = test_db();
        db.upsert_intelligence_node(sample_node_input("br-excl"))
            .unwrap();
        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                body: Some("whole new body".to_string()),
                body_replace: Some(BodyReplace {
                    fragment: "Test".to_string(),
                    replacement: "Best".to_string(),
                }),
                ..sample_node_input("br-excl")
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("mutually exclusive"),
            "expected a mutual-exclusion refusal, got: {err}"
        );
    }

    #[test]
    fn create_without_id_still_works() {
        let db = test_db();
        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: None,
                kind: Some("fact".to_string()),
                status: None,
                title: Some("Created".to_string()),
                body: Some("Fresh body".to_string()),
                body_replace: None,
                metadata: None,
                project_hash: Some(Some("proj-a".to_string())),
                session_id: None,
                relations: None,
            })
            .unwrap();
        assert!(result.created);
        assert!(!result.record.id.is_empty());
        assert_eq!(result.record.title, "Created");
        assert!(result.changed_fields.contains(&"title".to_string()));
    }

    #[test]
    fn create_requires_kind_title_body() {
        let db = test_db();
        let err = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: None,
                kind: None,
                status: None,
                title: Some("T".to_string()),
                body: Some("B".to_string()),
                body_replace: None,
                metadata: None,
                project_hash: None,
                session_id: None,
                relations: None,
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("create requires"),
            "expected a create-requires refusal, got: {err}"
        );
    }

    #[test]
    fn changed_fields_names_what_changed() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("cf-1".to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("T".to_string()),
            body: Some("B".to_string()),
            body_replace: None,
            metadata: Some(Some(serde_json::json!({"a": 1}))),
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap();

        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some("cf-1".to_string()),
                kind: None,
                status: None,
                title: Some("T2".to_string()),
                body: None,
                body_replace: None,
                metadata: Some(Some(serde_json::json!({"a": 2}))),
                project_hash: None,
                session_id: None,
                relations: None,
            })
            .unwrap();

        assert!(result.changed_fields.contains(&"title".to_string()));
        assert!(result.changed_fields.contains(&"metadata".to_string()));
        assert!(!result.changed_fields.contains(&"body".to_string()));
        assert!(!result.changed_fields.contains(&"kind".to_string()));
        assert!(!result.changed_fields.contains(&"status".to_string()));
        assert!(!result.changed_fields.contains(&"relations".to_string()));
    }

    #[test]
    fn explicit_null_clears_nullable_fields() {
        let db = test_db();
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("null-1".to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("T".to_string()),
            body: Some("B".to_string()),
            body_replace: None,
            metadata: Some(Some(serde_json::json!({"a": 1}))),
            project_hash: Some(Some("proj-a".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();

        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some("null-1".to_string()),
                kind: None,
                status: None,
                title: None,
                body: None,
                body_replace: None,
                metadata: Some(None),
                project_hash: Some(None),
                session_id: None,
                relations: None,
            })
            .unwrap();

        assert!(!result.created);
        assert_eq!(result.record.metadata, None);
        assert_eq!(result.record.project_hash, None);
        assert_eq!(result.record.title, "T");
        assert!(result.changed_fields.contains(&"metadata".to_string()));
        assert!(result.changed_fields.contains(&"project_hash".to_string()));
    }

    #[test]
    fn body_replace_real_case_hooks_decision() {
        // The real case: one stale line inside a body of several thousand
        // characters, corrected without touching anything else.
        let db = test_db();
        let filler = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(100);
        let stale = "fuera de esta versión: hooks internos";
        let body = format!("{filler}\n{stale}\n{filler}");
        assert!(body.len() > 6000);
        assert_eq!(body.matches(stale).count(), 1);
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("hooks-decision".to_string()),
            kind: Some("decision".to_string()),
            status: None,
            title: Some("Scope".to_string()),
            body: Some(body.clone()),
            body_replace: None,
            metadata: None,
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap();

        let result = db
            .upsert_intelligence_node(IntelligenceNodeInput {
                id: Some("hooks-decision".to_string()),
                kind: None,
                status: None,
                title: None,
                body: None,
                body_replace: Some(BodyReplace {
                    fragment: stale.to_string(),
                    replacement: "dentro de esta versión: hooks internos".to_string(),
                }),
                metadata: None,
                project_hash: None,
                session_id: None,
                relations: None,
            })
            .unwrap();

        let expected = body.replacen(stale, "dentro de esta versión: hooks internos", 1);
        assert_eq!(result.record.body, expected);
        // Everything before and after the fragment is byte-identical.
        let at = body.find(stale).unwrap();
        assert_eq!(&result.record.body[..at], &body[..at]);
        assert_eq!(
            &result.record.body[at + "dentro de esta versión: hooks internos".len()..],
            &body[at + stale.len()..]
        );
        assert_eq!(
            result.changed_fields,
            vec!["body".to_string(), "updated_at".to_string()]
        );
    }
}
