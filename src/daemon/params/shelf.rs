use super::*;

/// `session_send_file`: copy files onto the session shelf.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SessionSendFileParams {
    /// Absolute paths of the files to copy onto the shelf, 1 to 10 per
    /// call. Each path must exist and be a regular file of a supported
    /// type — PNG, JPEG, WebP, GIF or SVG, sniffed from content, never
    /// the extension — of at most 20 MiB.
    pub paths: Vec<String>,
    /// Optional caption stored with every file of this call, at most 500
    /// characters.
    pub caption: Option<String>,
    /// Id of the session whose shelf receives the files. Omit to shelve
    /// under your own session; a caller with no session shelves on the
    /// host shelf.
    pub session_id: Option<String>,
}
