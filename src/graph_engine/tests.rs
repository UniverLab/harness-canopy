//! Graph-engine tests (split from `src/graph_engine/mod.rs`, pure move).
mod fixtures_a;
// Re-export helpers other modules reached as `graph_engine::tests::{…}`.
pub(crate) use fixtures_a::{graph_fixture, insert_run_row, HomeGuard};
