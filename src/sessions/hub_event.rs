//! `HubEvent`: what the session hub broadcasts to every `/sessions/ws`
//! connection. Lives in its own module so each side of the bus (session
//! lifecycle, shelf arrivals/removals) can grow without pushing `hub.rs`
//! over the file-size cap.

use super::protocol::{SessionItem, ShelfItem};

/// Hub lifecycle event, broadcast to every connection. `Exited` from a
/// natural child exit goes to attached connections only (`to_all: false`);
/// `kill` sends its `Exited` to every connection (`to_all: true`).
/// Shelf events ride the same bus so every client sees arrivals and
/// removals without polling.
#[derive(Debug, Clone)]
pub enum HubEvent {
    SessionAdded(Box<SessionItem>),
    SessionRemoved(String),
    Exited { id: String, code: i32, to_all: bool },
    ShelfAdded(Box<ShelfItem>),
    ShelfRemoved { file_id: String },
}
