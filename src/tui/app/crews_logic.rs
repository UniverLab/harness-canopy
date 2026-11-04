//! CT30: pure logic for the Crews panel face — parsing the daemon's
//! roster/pair/crew JSON, building the pair rows, planning roster writes,
//! and mutating crew member lists. No I/O, no `App`: the worker thread and
//! the key handler both consume these, and every decision the face makes
//! about availability reuses CM48's single `effective_state` precedence
//! instead of reimplementing it.

use anyhow::{anyhow, Result};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde_json::Value;

use crate::db::roster_entries::{RosterEntry, RosterState};
use crate::domain::roster::effective_state;
use crate::tui::app::dialog::datetime_picker::local_naive_to_utc;

/// One refresh's worth of Crews-face data, straight from the daemon tools
/// (`harness roster_list`, `crew assignments {group_by:"pair"}`, `crew list`).
#[derive(Debug, Clone, Default)]
pub(crate) struct CrewsSnapshot {
    pub roster: Vec<RosterEntry>,
    pub pairs: Vec<PairSummary>,
    pub crews: Vec<CrewView>,
}

/// One `crew assignments {group_by:"pair"}` summary row: every graph that uses
/// one distinct `platform/model` pair. The wire JSON also carries
/// `ensemble_count`/`node_count`/`hook_count`, which the face does not show.
#[derive(Debug, Clone)]
pub(crate) struct PairSummary {
    pub platform: String,
    pub model: String,
    pub graph_count: usize,
    pub graph_names: Vec<String>,
}

/// One crew as shown in the Crews section. `members` keeps the raw JSON
/// values from `crew list` (with `timeout_minutes`/`infra_*` intact) so a
/// member edit can send them back through `crew update` verbatim (D6);
/// `member_pairs` is the parallel `(platform, model)` view used for
/// availability lookups (`model: null` maps to the empty string).
#[derive(Debug, Clone)]
pub(crate) struct CrewView {
    pub name: String,
    pub description: Option<String>,
    pub version: i64,
    pub members: Vec<Value>,
    pub member_pairs: Vec<(String, String)>,
    /// Bindings loaded on demand by `crew get` when the crew is expanded.
    pub bindings: Vec<CrewBindingView>,
    /// Ensembles + nodes + hooks from the `crew list` counts object — the
    /// row's binding count before a `crew get` has filled `bindings`.
    pub binding_total: usize,
}

/// One `crew get` binding: the graph it lives in, the binding kind and the
/// binding's own name (ensemble name, node name, or hook event).
#[derive(Debug, Clone)]
pub(crate) struct CrewBindingView {
    pub graph: String,
    pub kind: String,
    pub name: String,
}

fn string_field(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("field '{key}' missing or not a string"))
}

/// Parse the `harness roster_list` JSON array into `RosterEntry` rows the client
/// can feed to `effective_state` (D8 — no per-pair `harness roster_check`).
pub(crate) fn parse_roster(value: &Value) -> Result<Vec<RosterEntry>> {
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("roster list: expected a JSON array"))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let id = item
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("roster entry missing numeric 'id'"))?;
        let platform = string_field(item, "platform")?;
        let model_pattern = item
            .get("model_pattern")
            .and_then(Value::as_str)
            .map(str::to_string);
        let state = RosterState::parse(&string_field(item, "state")?)?;
        let reason = item
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        let until = item
            .get("until")
            .and_then(Value::as_str)
            .map(str::to_string);
        let updated_at = item
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        out.push(RosterEntry {
            id,
            platform,
            model_pattern,
            state,
            reason,
            until,
            updated_at,
        });
    }
    Ok(out)
}

/// Parse the `crew assignments {group_by:"pair"}` JSON array of pair summaries.
pub(crate) fn parse_pair_summaries(value: &Value) -> Result<Vec<PairSummary>> {
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("crew assignments pair summary: expected a JSON array"))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(PairSummary {
            platform: string_field(item, "platform")?,
            model: string_field(item, "model")?,
            graph_count: item.get("graph_count").and_then(Value::as_u64).unwrap_or(0) as usize,
            graph_names: item
                .get("graph_names")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|n| n.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Parse the `crew list` JSON array. The `bindings` object here carries
/// only counts; the expanded rows load real bindings via `crew get`.
pub(crate) fn parse_crews(value: &Value) -> Result<Vec<CrewView>> {
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("crew list: expected a JSON array"))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let members = item
            .get("members")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let member_pairs = members
            .iter()
            .map(|member| {
                let platform = member
                    .get("platform")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let model = member
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                (platform, model)
            })
            .collect();
        let bindings = item.get("bindings");
        let binding_total = bindings
            .and_then(|b| b.get("ensembles"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize
            + bindings
                .and_then(|b| b.get("nodes"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize
            + bindings
                .and_then(|b| b.get("hooks"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
        out.push(CrewView {
            name: string_field(item, "name")?,
            description: item
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            version: item.get("version").and_then(Value::as_i64).unwrap_or(0),
            members,
            member_pairs,
            bindings: Vec::new(),
            binding_total,
        });
    }
    Ok(out)
}

/// One row of the Pairs section: a distinct `platform/model` pair with its
/// effective availability and how many graphs/crews use it.
#[derive(Debug, Clone)]
pub(crate) struct PairRow {
    pub platform: String,
    /// Empty means the platform's default model.
    pub model: String,
    pub available: bool,
    pub expired: bool,
    pub reason: Option<String>,
    pub until: Option<String>,
    pub matched_entry_id: Option<i64>,
    pub graph_count: usize,
    pub crew_count: usize,
    pub graph_names: Vec<String>,
}

impl PairRow {
    /// Display label: `platform/model`, or `platform/(default)` for a pair
    /// that takes the platform's default model.
    pub(crate) fn label(&self) -> String {
        let model = if self.model.is_empty() {
            "(default)"
        } else {
            self.model.as_str()
        };
        format!("{}/{}", self.platform, model)
    }
}

/// Build the Pairs-section rows from one snapshot: every distinct pair from
/// the graph summaries plus every crew member (deduplicated), with its
/// effective availability from CM48's `effective_state`. Unavailable rows
/// sort first; the rest are alphabetical by label.
pub(crate) fn build_pair_rows(snapshot: &CrewsSnapshot, now: &DateTime<Utc>) -> Vec<PairRow> {
    let mut keys = std::collections::BTreeSet::new();
    for summary in &snapshot.pairs {
        keys.insert((summary.platform.clone(), summary.model.clone()));
    }
    for crew in &snapshot.crews {
        for (platform, model) in &crew.member_pairs {
            keys.insert((platform.clone(), model.clone()));
        }
    }

    let mut rows: Vec<PairRow> = keys
        .into_iter()
        .map(|(platform, model)| {
            let availability = effective_state(
                &snapshot.roster,
                &platform,
                if model.is_empty() { None } else { Some(&model) },
                now,
            );
            let summary = snapshot
                .pairs
                .iter()
                .find(|p| p.platform == platform && p.model == model);
            let crew_count = snapshot
                .crews
                .iter()
                .filter(|crew| {
                    crew.member_pairs
                        .iter()
                        .any(|(p, m)| *p == platform && *m == model)
                })
                .count();
            PairRow {
                platform,
                model,
                available: availability.available,
                expired: availability.expired,
                reason: availability.reason,
                until: availability.until,
                matched_entry_id: availability.matched_entry_id,
                graph_count: summary.map(|s| s.graph_count).unwrap_or(0),
                crew_count,
                graph_names: summary.map(|s| s.graph_names.clone()).unwrap_or_default(),
            }
        })
        .collect();
    rows.sort_by_key(|row| (row.available, row.label()));
    rows
}

/// The provider-prefix pattern for a model id: everything up to and
/// including its first `/`, plus `*` — `opencode-go/kimi-k2` becomes
/// `opencode-go/*`. Models without a `/` have no provider prefix.
pub(crate) fn provider_prefix(model: &str) -> Option<String> {
    model.find('/').map(|i| format!("{}*", &model[..=i]))
}

/// How many Pairs-section rows (same platform) a prefix pattern covers.
pub(crate) fn prefix_coverage(rows: &[PairRow], platform: &str, pattern: &str) -> usize {
    let prefix = pattern.strip_suffix('*').unwrap_or(pattern);
    rows.iter()
        .filter(|row| row.platform == platform && row.model.starts_with(prefix))
        .count()
}

/// One roster write the face can make: each variant maps to exactly one
/// MCP tool call (`to_call`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RosterWrite {
    Set {
        platform: String,
        model_pattern: Option<String>,
        state: &'static str,
        reason: Option<String>,
        until: Option<String>,
    },
    Clear {
        platform: String,
        model_pattern: Option<String>,
    },
}

impl RosterWrite {
    /// The `(tool, arguments)` pair for this write.
    pub(crate) fn to_call(&self) -> (&'static str, Value) {
        match self {
            RosterWrite::Set {
                platform,
                model_pattern,
                state,
                reason,
                until,
            } => (
                "harness",
                serde_json::json!({
                    "action": "roster_set",
                    "platform": platform,
                    "model_pattern": model_pattern,
                    "state": state,
                    "reason": reason,
                    "until": until,
                }),
            ),
            RosterWrite::Clear {
                platform,
                model_pattern,
            } => (
                "harness",
                serde_json::json!({
                    "action": "roster_clear",
                    "platform": platform,
                    "model_pattern": model_pattern,
                }),
            ),
        }
    }
}

/// The `model_pattern` a pair's exact writes use: `None` for the platform
/// default (a platform-wide entry), `Some(model)` otherwise.
fn pattern_for(model: &str) -> Option<String> {
    if model.is_empty() {
        None
    } else {
        Some(model.to_string())
    }
}

/// Plan the Space-toggle for one pair row (D9). Available → mark the pair
/// unavailable; unavailable → clear the deciding exact entry when that
/// alone explains the unavailability, otherwise set an explicit exact
/// `available` entry so a prefix or platform-wide entry can't keep
/// blocking the pair.
pub(crate) fn plan_toggle(
    row: &PairRow,
    roster: &[RosterEntry],
    now: &DateTime<Utc>,
) -> Vec<RosterWrite> {
    if row.available {
        return vec![RosterWrite::Set {
            platform: row.platform.clone(),
            model_pattern: pattern_for(&row.model),
            state: "unavailable",
            reason: Some("marked in TUI".to_string()),
            until: None,
        }];
    }
    let Some(entry_id) = row.matched_entry_id else {
        // An unavailable row always has a deciding entry; nothing to plan.
        return Vec::new();
    };
    let Some(entry) = roster.iter().find(|e| e.id == entry_id) else {
        return Vec::new();
    };
    let exact = !row.model.is_empty() && entry.model_pattern.as_deref() == Some(row.model.as_str());
    if exact {
        let rest: Vec<RosterEntry> = roster
            .iter()
            .filter(|e| e.id != entry_id)
            .cloned()
            .collect();
        let still_blocked = !effective_state(&rest, &row.platform, Some(&row.model), now).available;
        if still_blocked {
            // A prefix or platform-wide entry still blocks the pair: an
            // explicit exact `available` entry overrides it.
            return vec![RosterWrite::Set {
                platform: row.platform.clone(),
                model_pattern: Some(row.model.clone()),
                state: "available",
                reason: None,
                until: None,
            }];
        }
        return vec![RosterWrite::Clear {
            platform: row.platform.clone(),
            model_pattern: Some(row.model.clone()),
        }];
    }
    // The deciding entry is prefix or platform-wide: set an exact
    // `available` entry for this pair (platform-wide pair → override the
    // platform-wide entry itself).
    vec![RosterWrite::Set {
        platform: row.platform.clone(),
        model_pattern: pattern_for(&row.model),
        state: "available",
        reason: None,
        until: None,
    }]
}

/// The `e`-dialog write: mark the pair unavailable with the given reason
/// and optional expiry (`until` already converted to UTC and formatted).
pub(crate) fn plan_unavailable_until(
    platform: &str,
    model: &str,
    reason: Option<String>,
    until_utc: Option<DateTime<Utc>>,
) -> RosterWrite {
    RosterWrite::Set {
        platform: platform.to_string(),
        model_pattern: pattern_for(model),
        state: "unavailable",
        reason: Some(reason.unwrap_or_else(|| "marked in TUI".to_string())),
        until: until_utc.map(|u| u.to_rfc3339_opts(SecondsFormat::Secs, true)),
    }
}

/// Parse the `e`-dialog's Until field (`YYYY-MM-DD HH:MM`, local time) to
/// its UTC instant. Empty means no expiry; anything malformed or in the
/// past is an error and no write is planned.
pub(crate) fn parse_local_until(
    input: &str,
    now: &DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let naive = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M")
        .map_err(|_| "Until must be YYYY-MM-DD HH:MM".to_string())?;
    let utc = local_naive_to_utc(naive);
    if utc <= *now {
        return Err("Until must be in the future".to_string());
    }
    Ok(Some(utc))
}

/// One member edit the face can make on a crew's member list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemberEdit {
    Add { member: Value },
    Remove { index: usize },
    Move { index: usize, down: bool },
}

/// Apply a member edit to a raw member list (the JSON values from
/// `crew list`/`crew get`). Refuses to empty a crew and refuses moves
/// past either end.
pub(crate) fn apply_member_edit(members: &mut Vec<Value>, edit: &MemberEdit) -> Result<(), String> {
    match edit {
        MemberEdit::Add { member } => {
            members.push(member.clone());
            Ok(())
        }
        MemberEdit::Remove { index } => {
            if members.len() <= 1 {
                return Err("a crew needs at least one member".to_string());
            }
            if *index >= members.len() {
                return Err("member not found".to_string());
            }
            members.remove(*index);
            Ok(())
        }
        MemberEdit::Move { index, down } => {
            let target = if *down {
                index + 1
            } else {
                index.checked_sub(1).ok_or("already at the end")?
            };
            if target >= members.len() {
                return Err("already at the end".to_string());
            }
            members.swap(*index, target);
            Ok(())
        }
    }
}

/// The platform a new member should be added on: the platform of the
/// crew's last member (crews are never empty).
pub(crate) fn selected_member_platform(members: &[Value]) -> Option<String> {
    members
        .last()?
        .get("platform")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Keep `selected` visible in a `height`-row window starting at `scroll`:
/// the existing panel convention, computed pure so both the key handler
/// and the renderer agree.
pub(crate) fn keep_visible(selected: usize, scroll: usize, height: usize) -> usize {
    if height == 0 {
        return scroll;
    }
    if selected < scroll {
        selected
    } else if selected >= scroll + height {
        selected + 1 - height
    } else {
        scroll
    }
}

/// The status line after a successful crew member edit. The running-graph
/// note is appended when any bound graph is currently running (D15) — the
/// caller decides that from live state, this only formats.
pub(crate) fn format_crew_write_message(name: &str, version: i64, bound_running: bool) -> String {
    let mut text = format!("Crew '{name}' is now version {version}.");
    if bound_running {
        text.push_str(" Bound running graphs pick it up at their next dispatch.");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn entry(
        id: i64,
        platform: &str,
        pattern: Option<&str>,
        state: RosterState,
        until: Option<&str>,
    ) -> RosterEntry {
        RosterEntry {
            id,
            platform: platform.to_string(),
            model_pattern: pattern.map(str::to_string),
            state,
            reason: Some("quota".to_string()),
            until: until.map(str::to_string),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn summary(platform: &str, model: &str, graphs: usize) -> PairSummary {
        PairSummary {
            platform: platform.to_string(),
            model: model.to_string(),
            graph_count: graphs,
            graph_names: vec![format!("graph-{platform}-{model}")],
        }
    }

    fn crew(name: &str, members: &[(&str, &str)]) -> CrewView {
        let members: Vec<Value> = members
            .iter()
            .map(|(p, m)| {
                serde_json::json!({
                    "position": 0,
                    "platform": p,
                    "model": if m.is_empty() { Value::Null } else { Value::String(m.to_string()) },
                    "timeout_minutes": Value::Null,
                })
            })
            .collect();
        CrewView {
            name: name.to_string(),
            description: None,
            version: 1,
            member_pairs: members
                .iter()
                .map(|m| {
                    (
                        m["platform"].as_str().unwrap_or_default().to_string(),
                        m["model"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect(),
            members,
            bindings: Vec::new(),
            binding_total: 0,
        }
    }

    fn row(platform: &str, model: &str) -> PairRow {
        PairRow {
            platform: platform.to_string(),
            model: model.to_string(),
            available: true,
            expired: false,
            reason: None,
            until: None,
            matched_entry_id: None,
            graph_count: 0,
            crew_count: 0,
            graph_names: Vec::new(),
        }
    }

    #[test]
    fn pair_rows_list_unavailable_first_then_alphabetical() {
        let snapshot = CrewsSnapshot {
            roster: vec![entry(
                1,
                "opencode-go",
                None,
                RosterState::Unavailable,
                None,
            )],
            pairs: vec![
                summary("opencode-go", "kimi-k2", 1),
                summary("claude", "sonnet", 2),
            ],
            crews: Vec::new(),
        };
        let rows = build_pair_rows(&snapshot, &now());
        let labels: Vec<String> = rows.iter().map(PairRow::label).collect();
        assert_eq!(
            labels[0], "opencode-go/kimi-k2",
            "unavailable first: {labels:?}"
        );
        assert_eq!(&labels[1..], &["claude/sonnet".to_string()]);
        assert!(!rows[0].available);
        assert!(rows[1].available);
    }

    #[test]
    fn expired_pair_is_available_with_expired_flag() {
        let snapshot = CrewsSnapshot {
            roster: vec![entry(
                1,
                "opencode-go",
                Some("kimi-k2"),
                RosterState::Unavailable,
                Some("2026-05-01T00:00:00Z"),
            )],
            pairs: vec![summary("opencode-go", "kimi-k2", 1)],
            crews: Vec::new(),
        };
        let rows = build_pair_rows(&snapshot, &now());
        assert!(rows[0].available, "an expired entry reads as available");
        assert!(rows[0].expired, "and the expiry is reported");
    }

    #[test]
    fn default_model_pair_has_empty_model_and_label_default() {
        let snapshot = CrewsSnapshot {
            roster: Vec::new(),
            pairs: Vec::new(),
            crews: vec![crew("review", &[("claude", "")])],
        };
        let rows = build_pair_rows(&snapshot, &now());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "");
        assert_eq!(rows[0].label(), "claude/(default)");
    }

    #[test]
    fn pair_dedupes_across_graphs_and_crews() {
        let snapshot = CrewsSnapshot {
            roster: Vec::new(),
            pairs: vec![summary("opencode-go", "kimi-k2", 3)],
            crews: vec![
                crew("a", &[("opencode-go", "kimi-k2")]),
                crew("b", &[("opencode-go", "kimi-k2")]),
            ],
        };
        let rows = build_pair_rows(&snapshot, &now());
        assert_eq!(rows.len(), 1, "one row per distinct pair: {rows:?}");
        assert_eq!(rows[0].crew_count, 2);
        assert_eq!(rows[0].graph_count, 3);
    }

    #[test]
    fn provider_prefix_takes_up_to_first_slash() {
        assert_eq!(
            provider_prefix("opencode-go/kimi-k2").as_deref(),
            Some("opencode-go/*")
        );
        assert_eq!(provider_prefix("gpt-5"), None);
        assert_eq!(provider_prefix(""), None);
        assert_eq!(
            provider_prefix("opencode-go/a/b").as_deref(),
            Some("opencode-go/*")
        );
    }

    #[test]
    fn prefix_coverage_counts_only_matching_platform_rows() {
        // Roster prefix patterns are full ids (`opencode-go/*`), so the rows
        // carry platform-prefixed models here.
        let rows = vec![
            row("opencode-go", "opencode-go/a"),
            row("opencode-go", "opencode-go/b"),
            row("claude", "claude/a"),
        ];
        assert_eq!(prefix_coverage(&rows, "opencode-go", "opencode-go/*"), 2);
        assert_eq!(prefix_coverage(&rows, "claude", "opencode-go/*"), 0);
    }

    #[test]
    fn toggle_available_marks_unavailable_with_reason_and_no_until() {
        let mut target = row("opencode-go", "opencode-go/kimi-k2");
        target.available = true;
        let writes = plan_toggle(&target, &[], &now());
        assert_eq!(
            writes,
            vec![RosterWrite::Set {
                platform: "opencode-go".to_string(),
                model_pattern: Some("opencode-go/kimi-k2".to_string()),
                state: "unavailable",
                reason: Some("marked in TUI".to_string()),
                until: None,
            }]
        );
        let (tool, args) = writes[0].to_call();
        assert_eq!(tool, "harness");
        assert_eq!(
            args,
            serde_json::json!({
                "action": "roster_set",
                "platform": "opencode-go",
                "model_pattern": "opencode-go/kimi-k2",
                "state": "unavailable",
                "reason": "marked in TUI",
                "until": null,
            })
        );
    }

    #[test]
    fn toggle_back_on_exact_entry_clears_it() {
        let roster = vec![entry(
            7,
            "opencode-go",
            Some("kimi-k2"),
            RosterState::Unavailable,
            None,
        )];
        let mut target = row("opencode-go", "kimi-k2");
        target.available = false;
        target.matched_entry_id = Some(7);
        let writes = plan_toggle(&target, &roster, &now());
        assert_eq!(
            writes,
            vec![RosterWrite::Clear {
                platform: "opencode-go".to_string(),
                model_pattern: Some("kimi-k2".to_string()),
            }]
        );
        let (tool, args) = writes[0].to_call();
        assert_eq!(tool, "harness");
        assert_eq!(
            args,
            serde_json::json!({"action": "roster_clear", "platform": "opencode-go", "model_pattern": "kimi-k2"})
        );
    }

    #[test]
    fn toggle_back_on_exact_entry_under_prefix_sets_available() {
        // An exact unavailable entry AND a prefix entry that still blocks
        // the model: clearing the exact entry alone would leave the pair
        // unavailable, so an explicit exact `available` entry is set.
        let roster = vec![
            entry(
                7,
                "opencode-go",
                Some("kimi-k2"),
                RosterState::Unavailable,
                None,
            ),
            entry(
                8,
                "opencode-go",
                Some("kimi-*"),
                RosterState::Unavailable,
                None,
            ),
        ];
        let mut target = row("opencode-go", "kimi-k2");
        target.available = false;
        target.matched_entry_id = Some(7);
        let writes = plan_toggle(&target, &roster, &now());
        assert_eq!(
            writes,
            vec![RosterWrite::Set {
                platform: "opencode-go".to_string(),
                model_pattern: Some("kimi-k2".to_string()),
                state: "available",
                reason: None,
                until: None,
            }]
        );
    }

    #[test]
    fn toggle_back_on_prefix_entry_sets_exact_available() {
        let roster = vec![entry(
            8,
            "opencode-go",
            Some("kimi-*"),
            RosterState::Unavailable,
            None,
        )];
        let mut target = row("opencode-go", "kimi-k2");
        target.available = false;
        target.matched_entry_id = Some(8);
        let writes = plan_toggle(&target, &roster, &now());
        assert_eq!(
            writes,
            vec![RosterWrite::Set {
                platform: "opencode-go".to_string(),
                model_pattern: Some("kimi-k2".to_string()),
                state: "available",
                reason: None,
                until: None,
            }]
        );
    }

    #[test]
    fn parse_local_until_rejects_past_and_malformed() {
        assert_eq!(parse_local_until("", &now()), Ok(None));
        assert_eq!(
            parse_local_until("2001-01-01 10:00", &now()),
            Err("Until must be in the future".to_string())
        );
        assert_eq!(
            parse_local_until("tomorrow", &now()),
            Err("Until must be YYYY-MM-DD HH:MM".to_string())
        );
    }

    #[test]
    fn apply_member_edit_refuses_to_empty_a_crew() {
        let mut members = vec![serde_json::json!({"platform": "claude", "model": null})];
        let before = members.clone();
        let result = apply_member_edit(&mut members, &MemberEdit::Remove { index: 0 });
        assert_eq!(result, Err("a crew needs at least one member".to_string()));
        assert_eq!(members, before, "the list must stay unchanged");
    }

    #[test]
    fn apply_member_edit_moves_and_appends() {
        let mut members = vec![
            serde_json::json!({"platform": "a", "model": null}),
            serde_json::json!({"platform": "b", "model": null}),
            serde_json::json!({"platform": "c", "model": null}),
        ];
        apply_member_edit(
            &mut members,
            &MemberEdit::Move {
                index: 0,
                down: true,
            },
        )
        .unwrap();
        let platforms: Vec<&str> = members
            .iter()
            .filter_map(|m| m["platform"].as_str())
            .collect();
        assert_eq!(platforms, vec!["b", "a", "c"]);

        apply_member_edit(
            &mut members,
            &MemberEdit::Move {
                index: 2,
                down: false,
            },
        )
        .unwrap();
        let platforms: Vec<&str> = members
            .iter()
            .filter_map(|m| m["platform"].as_str())
            .collect();
        assert_eq!(platforms, vec!["b", "c", "a"]);

        let added = serde_json::json!({"platform": "d", "model": null});
        apply_member_edit(
            &mut members,
            &MemberEdit::Add {
                member: added.clone(),
            },
        )
        .unwrap();
        assert_eq!(members.last(), Some(&added), "Add appends at the end");
    }

    #[test]
    fn apply_member_edit_refuses_move_past_the_ends() {
        let mut members = vec![
            serde_json::json!({"platform": "a", "model": null}),
            serde_json::json!({"platform": "b", "model": null}),
        ];
        assert_eq!(
            apply_member_edit(
                &mut members,
                &MemberEdit::Move {
                    index: 0,
                    down: false
                }
            ),
            Err("already at the end".to_string())
        );
        assert_eq!(
            apply_member_edit(
                &mut members,
                &MemberEdit::Move {
                    index: 1,
                    down: true
                }
            ),
            Err("already at the end".to_string())
        );
    }

    #[test]
    fn format_crew_write_message_adds_running_note() {
        assert_eq!(
            format_crew_write_message("review", 4, false),
            "Crew 'review' is now version 4."
        );
        assert_eq!(
            format_crew_write_message("review", 4, true),
            "Crew 'review' is now version 4. Bound running graphs pick it up at their next dispatch."
        );
    }

    #[test]
    fn keep_visible_scrolls_to_selection() {
        assert_eq!(keep_visible(0, 0, 10), 0);
        assert_eq!(keep_visible(3, 0, 10), 0, "visible rows don't scroll");
        assert_eq!(keep_visible(9, 0, 10), 0, "last visible row doesn't scroll");
        assert_eq!(
            keep_visible(10, 0, 10),
            1,
            "one past the window scrolls by 1"
        );
        assert_eq!(keep_visible(2, 5, 10), 2, "above the window jumps to it");
        assert_eq!(keep_visible(2, 5, 0), 5, "a zero-height window is a no-op");
    }
}
