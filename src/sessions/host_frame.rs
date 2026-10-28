//! Host frames: the hub's side of session protocol v1, parsed by
//! [`parse_host_frame`]. Split from `protocol.rs`, which keeps the client
//! frames and the frame builders; `protocol` re-exports [`HostFrame`] and
//! [`parse_host_frame`] so existing importers keep working.

use serde::{Deserialize, Deserializer};

use super::catalog::{CatalogCli, CatalogData, CatalogPreset, CatalogProject, CatalogSkill};
use super::protocol::{SessionItem, ShelfItem};

/// One frame sent by the hub, parsed by [`parse_host_frame`].
#[derive(Debug)]
pub enum HostFrame {
    Hello {
        proto: u32,
        host_label: String,
    },
    Sessions {
        req: String,
        items: Vec<SessionItem>,
    },
    Snapshot {
        id: String,
        cols: u16,
        rows: u16,
        seq: u64,
        data: Vec<u8>,
    },
    Out {
        id: String,
        seq: u64,
        data: Vec<u8>,
    },
    Exited {
        id: String,
        code: i32,
    },
    SessionAdded {
        item: Box<SessionItem>,
    },
    SessionRemoved {
        id: String,
    },
    Spawned {
        req: String,
        id: String,
    },
    Ok {
        req: String,
    },
    Error {
        req: Option<String>,
        code: String,
        message: String,
    },
    /// `shelf{req,id?,items}`: one shelf's listing, newest first.
    Shelf {
        req: String,
        id: Option<String>,
        items: Vec<ShelfItem>,
    },
    /// `shelf_added{item}`: broadcast when any shelf gains a file.
    ShelfAdded {
        item: Box<ShelfItem>,
    },
    /// `shelf_removed{file_id}`: broadcast when any shelf loses a file
    /// (delete, prune, retention, orphan sweep).
    ShelfRemoved {
        file_id: String,
    },
    /// `file{req,file_id,part,data,last}`: one base64 chunk of a `shelf_get`
    /// response, at most 12 KiB raw per part.
    File {
        req: String,
        file_id: String,
        part: u32,
        data: Vec<u8>,
        last: bool,
    },
    /// `composed{req,text}`: the answer to `compose`.
    Composed {
        req: String,
        // Remote clients consume the composed text; the TUI parses it for
        // protocol completeness but never sends `compose` itself.
        #[allow(dead_code)]
        text: String,
    },
    /// `catalog{req,clis,projects,presets,skills}`: the answer to `catalog`.
    Catalog {
        req: String,
        // Remote clients consume the catalog; the TUI parses it for
        // protocol completeness but never sends `catalog` itself.
        #[allow(dead_code)]
        data: CatalogData,
    },
    /// Any frame this build does not know: parsed so a newer host can talk
    /// to an older client (and vice versa), then ignored.
    Unknown,
}

impl HostFrame {
    /// The request id this frame answers, when it answers one.
    pub fn req(&self) -> Option<&str> {
        match self {
            HostFrame::Sessions { req, .. }
            | HostFrame::Spawned { req, .. }
            | HostFrame::Shelf { req, .. }
            | HostFrame::File { req, .. }
            | HostFrame::Composed { req, .. }
            | HostFrame::Catalog { req, .. }
            | HostFrame::Ok { req } => Some(req),
            HostFrame::Error { req, .. } => req.as_deref(),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum HostFrameWire {
    Hello {
        proto: u32,
        host_label: String,
    },
    Sessions {
        req: String,
        items: Vec<SessionItem>,
    },
    Snapshot {
        id: String,
        cols: u16,
        rows: u16,
        seq: u64,
        #[serde(deserialize_with = "de_b64")]
        data: Vec<u8>,
    },
    Out {
        id: String,
        seq: u64,
        #[serde(deserialize_with = "de_b64")]
        data: Vec<u8>,
    },
    Exited {
        id: String,
        code: i32,
    },
    SessionAdded {
        item: SessionItem,
    },
    SessionRemoved {
        id: String,
    },
    Spawned {
        req: String,
        id: String,
    },
    Ok {
        req: String,
    },
    Error {
        req: Option<String>,
        code: String,
        message: String,
    },
    Shelf {
        req: String,
        #[serde(default)]
        id: Option<String>,
        items: Vec<ShelfItem>,
    },
    ShelfAdded {
        item: ShelfItem,
    },
    ShelfRemoved {
        file_id: String,
    },
    File {
        req: String,
        file_id: String,
        part: u32,
        #[serde(deserialize_with = "de_b64")]
        data: Vec<u8>,
        last: bool,
    },
    Composed {
        req: String,
        text: String,
    },
    Catalog {
        req: String,
        clis: Vec<CatalogCli>,
        projects: Vec<CatalogProject>,
        presets: Vec<CatalogPreset>,
        skills: Vec<CatalogSkill>,
    },
    /// Any `t` this build does not recognise: matches everything else, so
    /// an unknown frame from a newer peer parses (and is ignored) instead
    /// of failing the whole text.
    #[serde(other)]
    Unknown,
}

fn de_b64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    use base64::Engine as _;
    let encoded = String::deserialize(deserializer)?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(serde::de::Error::custom)
}

/// Parse one hub text frame; `Err` carries the serde message.
pub fn parse_host_frame(text: &str) -> Result<HostFrame, String> {
    let wire: HostFrameWire =
        serde_json::from_str(text).map_err(|e| format!("invalid host frame: {e}"))?;
    Ok(match wire {
        HostFrameWire::Hello { proto, host_label } => HostFrame::Hello { proto, host_label },
        HostFrameWire::Sessions { req, items } => HostFrame::Sessions { req, items },
        HostFrameWire::Snapshot {
            id,
            cols,
            rows,
            seq,
            data,
        } => HostFrame::Snapshot {
            id,
            cols,
            rows,
            seq,
            data,
        },
        HostFrameWire::Out { id, seq, data } => HostFrame::Out { id, seq, data },
        HostFrameWire::Exited { id, code } => HostFrame::Exited { id, code },
        HostFrameWire::SessionAdded { item } => HostFrame::SessionAdded {
            item: Box::new(item),
        },
        HostFrameWire::SessionRemoved { id } => HostFrame::SessionRemoved { id },
        HostFrameWire::Spawned { req, id } => HostFrame::Spawned { req, id },
        HostFrameWire::Ok { req } => HostFrame::Ok { req },
        HostFrameWire::Error { req, code, message } => HostFrame::Error { req, code, message },
        HostFrameWire::Shelf { req, id, items } => HostFrame::Shelf { req, id, items },
        HostFrameWire::ShelfAdded { item } => HostFrame::ShelfAdded {
            item: Box::new(item),
        },
        HostFrameWire::ShelfRemoved { file_id } => HostFrame::ShelfRemoved { file_id },
        HostFrameWire::File {
            req,
            file_id,
            part,
            data,
            last,
        } => HostFrame::File {
            req,
            file_id,
            part,
            data,
            last,
        },
        HostFrameWire::Composed { req, text } => HostFrame::Composed { req, text },
        HostFrameWire::Catalog {
            req,
            clis,
            projects,
            presets,
            skills,
        } => HostFrame::Catalog {
            req,
            data: CatalogData {
                clis,
                projects,
                presets,
                skills,
            },
        },
        HostFrameWire::Unknown => HostFrame::Unknown,
    })
}
