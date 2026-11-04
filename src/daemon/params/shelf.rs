use super::*;

/// `session_send_file`: copy files onto the session shelf.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SessionSendFileParams {
    /// Absolute paths of the files to copy onto the shelf, 1 to 10 per
    /// call. Each path must exist and be a regular file of at most 20 MiB;
    /// any type is accepted, and content sniffing only decides how it is
    /// displayed.
    pub paths: Vec<String>,
    /// Optional caption stored with every file of this call, at most 500
    /// characters.
    pub caption: Option<String>,
    /// Id of the session whose shelf receives the files. Omit to shelve
    /// under your own session; a caller with no session shelves on the
    /// host shelf.
    pub session_id: Option<String>,
}

/// `session_list_files`: list one session shelf.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SessionListFilesParams {
    /// Id of the session whose shelf to list. Omit to list your own
    /// session's shelf; a caller with no session lists the host shelf.
    pub session_id: Option<String>,
}

/// `session_read_file`: read one shelf file by id.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SessionReadFileParams {
    /// The `file_id` of the shelf file to read.
    pub file_id: String,
}
