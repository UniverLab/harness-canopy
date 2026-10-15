use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::graphs::GraphSpec;

/// An ordered queue of existing specs — the work, decoupled from any one
/// graph (the team that will eventually run it). A spec can be a member of
/// any number of queues; queue membership never mutates the spec itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// A queue together with its members, resolved and ordered by position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueDetails {
    pub queue: Queue,
    pub members: Vec<GraphSpec>,
    /// RS3 context group label per member spec id. Absent/`None` for ungrouped
    /// members. Kept as a side map so `members` stays a plain `Vec<GraphSpec>`
    /// (queue membership never mutates the spec itself — the group lives on the
    /// membership row).
    pub member_groups: std::collections::HashMap<String, Option<String>>,
}

/// One queue member as the group-contiguity invariant sees it. Slice order is
/// queue order (ascending position).
#[derive(Debug, Clone, PartialEq)]
pub struct QueueMember {
    pub spec_id: String,
    pub group: Option<String>,
    /// `true` for the running spec and every finished member
    /// (`completed`/`failed`/`skipped`/`interrupted`) — they keep their
    /// position and are never repositioned. Only `pending` members move.
    pub locked: bool,
}

/// A context group whose pending members would not be contiguous, together
/// with the 1-based queue positions those pending members would occupy.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupSplit {
    pub group: String,
    pub positions: Vec<usize>,
}

fn pending_entries(members: &[QueueMember]) -> Vec<(String, Option<String>)> {
    members
        .iter()
        .filter(|m| !m.locked)
        .map(|m| (m.spec_id.clone(), m.group.clone()))
        .collect()
}

fn ids(entries: Vec<(String, Option<String>)>) -> Vec<String> {
    entries.into_iter().map(|(id, _)| id).collect()
}

/// Pending member order after adding `spec_id` (not yet a member) with `group`.
/// Joining an existing group places it immediately after that group's last
/// pending member; a brand-new group (or no group) appends at the end. The
/// added spec is always treated as pending by this function — a caller adding
/// a finished spec must not call it (see `add_queue_member_grouped`).
pub fn pending_after_add(
    members: &[QueueMember],
    spec_id: &str,
    group: Option<&str>,
) -> Vec<String> {
    let mut pending = pending_entries(members);
    match group {
        Some(g) => match pending.iter().rposition(|(_, pg)| pg.as_deref() == Some(g)) {
            Some(last) => pending.insert(last + 1, (spec_id.to_string(), Some(g.to_string()))),
            None => pending.push((spec_id.to_string(), Some(g.to_string()))),
        },
        None => pending.push((spec_id.to_string(), None)),
    }
    ids(pending)
}

/// Pending member order after setting `spec_id`'s group to `group` (`None`
/// clears). A locked member never moves (the caller updates only its group
/// column). Joining an existing group moves it after that group's last pending
/// member; a brand-new group leaves it where it is; clearing leaves it where it
/// is unless that would strand it between two members of its old group, in
/// which case it moves to just after that group's last pending member.
pub fn pending_after_set_group(
    members: &[QueueMember],
    spec_id: &str,
    group: Option<&str>,
) -> Vec<String> {
    let current = || ids(pending_entries(members));
    let Some(target) = members.iter().find(|m| m.spec_id == spec_id) else {
        return current();
    };
    if target.locked || target.group.as_deref() == group {
        return current();
    }
    let old_group = target.group.clone();
    let mut pending = pending_entries(members);
    let start = pending
        .iter()
        .position(|(id, _)| id == spec_id)
        .expect("target is a member");
    pending.remove(start);
    match group {
        Some(g) => match pending.iter().rposition(|(_, pg)| pg.as_deref() == Some(g)) {
            Some(last) => pending.insert(last + 1, (spec_id.to_string(), Some(g.to_string()))),
            None => pending.insert(
                start.min(pending.len()),
                (spec_id.to_string(), Some(g.to_string())),
            ),
        },
        None => {
            pending.insert(start.min(pending.len()), (spec_id.to_string(), None));
            if let Some(g) = old_group.as_deref() {
                let at = pending
                    .iter()
                    .position(|(id, _)| id == spec_id)
                    .expect("just inserted");
                let before = pending[..at].iter().any(|(_, pg)| pg.as_deref() == Some(g));
                let after = pending[at + 1..]
                    .iter()
                    .any(|(_, pg)| pg.as_deref() == Some(g));
                if before && after {
                    pending.remove(at);
                    if let Some(last) = pending.iter().rposition(|(_, pg)| pg.as_deref() == Some(g))
                    {
                        pending.insert(last + 1, (spec_id.to_string(), None));
                    }
                }
            }
        }
    }
    ids(pending)
}

/// The first context group (by first pending occurrence) whose pending members
/// are not contiguous within `order`'s pending subsequence. Reported positions
/// are 1-based indices into the full `order` slice. `None` means every group is
/// contiguous.
pub fn split_group(order: &[QueueMember]) -> Option<GroupSplit> {
    use std::collections::HashMap;
    let mut first: HashMap<String, usize> = HashMap::new();
    let mut last: HashMap<String, usize> = HashMap::new();
    let mut count: HashMap<String, usize> = HashMap::new();
    let mut positions: HashMap<String, Vec<usize>> = HashMap::new();
    let mut seq = 0usize;
    for (index, member) in order.iter().enumerate() {
        if member.locked {
            continue;
        }
        if let Some(group) = &member.group {
            first.entry(group.clone()).or_insert(seq);
            last.insert(group.clone(), seq);
            *count.entry(group.clone()).or_insert(0) += 1;
            positions.entry(group.clone()).or_default().push(index + 1);
        }
        seq += 1;
    }
    let mut split: Option<(usize, GroupSplit)> = None;
    for (group, start) in &first {
        if last[group] - start + 1 != count[group] {
            let candidate = (
                *start,
                GroupSplit {
                    group: group.clone(),
                    positions: positions[group].clone(),
                },
            );
            let better = match &split {
                Some((seen, _)) => candidate.0 < *seen,
                None => true,
            };
            if better {
                split = Some(candidate);
            }
        }
    }
    split.map(|(_, s)| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(spec_id: &str, group: Option<&str>, locked: bool) -> QueueMember {
        QueueMember {
            spec_id: spec_id.to_string(),
            group: group.map(str::to_string),
            locked,
        }
    }

    fn pending(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn add_to_existing_group_lands_after_last_sibling() {
        let members = [
            m("a", Some("g"), false),
            m("b", Some("g"), false),
            m("c", None, false),
        ];
        assert_eq!(
            pending_after_add(&members, "x", Some("g")),
            pending(&["a", "b", "x", "c"])
        );
    }

    #[test]
    fn add_new_group_appends() {
        let members = [m("a", None, false), m("b", None, false)];
        assert_eq!(
            pending_after_add(&members, "x", Some("new")),
            pending(&["a", "b", "x"])
        );
    }

    #[test]
    fn add_ungrouped_appends() {
        assert_eq!(
            pending_after_add(&[m("a", None, false)], "x", None),
            pending(&["a", "x"])
        );
    }

    #[test]
    fn set_group_joins_existing_group() {
        let members = [
            m("a", Some("g1"), false),
            m("b", Some("g2"), false),
            m("c", Some("g1"), false),
        ];
        assert_eq!(
            pending_after_set_group(&members, "c", Some("g2")),
            pending(&["a", "b", "c"])
        );
    }

    #[test]
    fn set_group_new_group_leaves_place() {
        let members = [
            m("a", Some("g1"), false),
            m("b", None, false),
            m("c", None, false),
        ];
        assert_eq!(
            pending_after_set_group(&members, "b", Some("g3")),
            pending(&["a", "b", "c"])
        );
    }

    #[test]
    fn clear_group_between_old_group_moves_after() {
        let members = [
            m("a", Some("g"), false),
            m("b", Some("g"), false),
            m("c", Some("g"), false),
        ];
        assert_eq!(
            pending_after_set_group(&members, "b", None),
            pending(&["a", "c", "b"])
        );
    }

    #[test]
    fn clear_group_last_of_group_stays() {
        let members = [m("a", Some("g"), false), m("b", Some("g"), false)];
        assert_eq!(
            pending_after_set_group(&members, "b", None),
            pending(&["a", "b"])
        );
    }

    #[test]
    fn set_group_on_locked_member_is_noop() {
        let members = [m("a", Some("g"), true), m("b", None, false)];
        assert_eq!(
            pending_after_set_group(&members, "a", Some("h")),
            pending(&["b"])
        );
    }

    #[test]
    fn split_group_detects_noncontiguous() {
        let members = [
            m("a", Some("g"), false),
            m("b", Some("h"), false),
            m("c", Some("g"), false),
        ];
        assert_eq!(
            split_group(&members),
            Some(GroupSplit {
                group: "g".into(),
                positions: vec![1, 3],
            })
        );
    }

    #[test]
    fn split_group_none_when_contiguous() {
        let members = [
            m("a", Some("g"), false),
            m("b", Some("g"), false),
            m("c", None, false),
        ];
        assert_eq!(split_group(&members), None);
    }

    #[test]
    fn split_group_ignores_locked_members() {
        let members = [
            m("a", Some("g"), false),
            m("r", Some("g"), true),
            m("b", Some("g"), false),
        ];
        assert_eq!(split_group(&members), None);
    }
}
