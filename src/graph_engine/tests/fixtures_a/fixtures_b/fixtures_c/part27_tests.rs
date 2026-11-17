//! Part 27 of graph-engine `tests`: a stale round-robin rotation index wraps
//! instead of panicking — a pin equal to the member count, a persisted index
//! larger than the member count, and a crew that shrinks between visits each
//! select a valid member; an empty member list is an error.
use super::*;

/// A 3-member round-robin ensemble's details (`rr-a`, `rr-b`, `rr-c`).
fn three_member_round_robin_details(db: &Database, spec_id: &str) -> EnsembleDetails {
    insert_kind_ensemble(
        db,
        spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-a", "rr-a"), ("rr-b", "rr-b"), ("rr-c", "rr-c")],
    );
    db.get_ensemble_details("ens1")
        .unwrap()
        .expect("the fixture ensemble exists")
}

/// A pin equal to the member count wraps to member 0 and advances to 1.
#[test]
fn round_robin_pin_equal_to_member_count_wraps() {
    let (_dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let details = three_member_round_robin_details(&db, &spec_id);
    let mut pins = std::collections::HashMap::new();
    pins.insert("ens1".to_string(), 3);
    let sessions = std::collections::HashMap::new();
    let (start, resume, next) = engine
        .resolve_round_robin_start(&details, &mut pins, &sessions)
        .unwrap();
    assert_eq!((start, next), (1, 1), "pin 3 of 3 wraps to 0, next is 1");
    assert_eq!(resume, None, "no resumable session means no resume");
    assert_eq!(
        pins.get("ens1"),
        Some(&1),
        "the pin advances to the next index"
    );
}

/// A persisted index larger than the member count wraps on first visit and
/// the corrected next index is persisted.
#[test]
fn round_robin_stale_persisted_index_wraps_and_persists_correction() {
    let (_dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let details = three_member_round_robin_details(&db, &spec_id);
    db.update_ensemble_kind("ens1", None, Some(Some(7)))
        .unwrap();
    let mut pins = std::collections::HashMap::new();
    let sessions = std::collections::HashMap::new();
    let (start, resume, next) = engine
        .resolve_round_robin_start(&details, &mut pins, &sessions)
        .unwrap();
    assert_eq!(
        (start, next),
        (1, 2),
        "persisted 7 of 3 wraps to start 1, next 2"
    );
    assert_eq!(resume, None, "no resumable session means no resume");
    assert_eq!(
        pins.get("ens1"),
        Some(&1),
        "the pin holds the wrapped start"
    );
    let persisted = db
        .get_ensemble("ens1")
        .unwrap()
        .expect("the fixture ensemble exists")
        .round_robin_index;
    assert_eq!(persisted, Some(2), "the corrected next index is persisted");
}

/// A crew that shrinks between the first visit and a bounce still selects a
/// valid member: the stale pin wraps against the current member list.
#[test]
fn round_robin_shrunk_crew_bounce_wraps() {
    let (_dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let mut details = three_member_round_robin_details(&db, &spec_id);
    let mut pins = std::collections::HashMap::new();
    let sessions = std::collections::HashMap::new();
    let (first, _, _) = engine
        .resolve_round_robin_start(&details, &mut pins, &sessions)
        .unwrap();
    assert_eq!(first, 0, "a fresh first visit starts at 0");
    details.members.truncate(2);
    pins.insert("ens1".to_string(), 2);
    let (start, resume, next) = engine
        .resolve_round_robin_start(&details, &mut pins, &sessions)
        .unwrap();
    assert!(start < 2, "the wrapped start is inside the shrunk crew");
    assert_eq!((start, next), (1, 1), "pin 2 of 2 wraps to 0, next is 1");
    assert_eq!(resume, None, "no resumable session means no resume");
    assert_eq!(
        pins.get("ens1"),
        Some(&1),
        "the pin advances to the next index"
    );
}

/// An empty member list is an error, on first visit and on bounce alike.
#[test]
fn round_robin_empty_member_list_errors() {
    let (_dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let mut details = three_member_round_robin_details(&db, &spec_id);
    details.members.clear();
    let mut pins = std::collections::HashMap::new();
    let sessions = std::collections::HashMap::new();
    let first = engine.resolve_round_robin_start(&details, &mut pins, &sessions);
    assert!(
        first
            .unwrap_err()
            .to_string()
            .contains("has no members to run"),
        "an empty first visit errors instead of panicking"
    );
    pins.insert("ens1".to_string(), 0);
    let bounce = engine.resolve_round_robin_start(&details, &mut pins, &sessions);
    assert!(
        bounce
            .unwrap_err()
            .to_string()
            .contains("has no members to run"),
        "an empty bounce errors instead of panicking"
    );
}
