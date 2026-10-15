pub(crate) fn args_contain_flag(args: &str, flag: &str) -> bool {
    args.split_whitespace().any(|arg| arg == flag)
}

pub(crate) fn append_flag_if_missing(
    base_args: Option<&str>,
    yolo_flag: Option<&str>,
    should_include_yolo: bool,
) -> Option<String> {
    let base = base_args.map(str::trim).filter(|args| !args.is_empty());

    match (base, yolo_flag, should_include_yolo) {
        (Some(args), Some(flag), true) if !args_contain_flag(args, flag) => {
            Some(format!("{args} {flag}"))
        }
        (Some(args), _, _) => Some(args.to_string()),
        (None, Some(flag), true) => Some(flag.to_string()),
        (None, _, _) => None,
    }
}

fn join_args(base_args: Option<&str>, extra_args: Option<&str>) -> Option<String> {
    match (
        base_args.map(str::trim).filter(|args| !args.is_empty()),
        extra_args.map(str::trim).filter(|args| !args.is_empty()),
    ) {
        (Some(base), Some(extra)) => Some(format!("{base} {extra}")),
        (Some(base), None) => Some(base.to_string()),
        (None, Some(extra)) => Some(extra.to_string()),
        (None, None) => None,
    }
}

fn args_contain_sequence(args: &str, sequence: &str) -> bool {
    let args_tokens: Vec<_> = args.split_whitespace().collect();
    let sequence_tokens: Vec<_> = sequence.split_whitespace().collect();
    !sequence_tokens.is_empty()
        && args_tokens
            .windows(sequence_tokens.len())
            .any(|window| window == sequence_tokens.as_slice())
}

use crate::db::session::InteractiveSession;

/// Metadata-only view of a resumable session for the picker: enough to tell
/// rows apart (name, harness, recency) and enough to actually relaunch it
/// (working_dir, original args) without reading a session's full history.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResumableSession {
    pub id: String,
    pub name: String,
    pub cli: String,
    pub last_active: String,
    pub working_dir: String,
    pub args: Option<String>,
}

impl From<&InteractiveSession> for ResumableSession {
    fn from(session: &InteractiveSession) -> Self {
        Self {
            id: session.id.clone(),
            name: session.name.clone(),
            cli: session.cli.clone(),
            last_active: session.started_at.clone(),
            working_dir: session.working_dir.clone(),
            args: session.args.clone(),
        }
    }
}

/// Navigation state for the session-resume picker. Index/scroll math is
/// delegated to the shared selection model (`tui::selection`) rather than
/// reimplemented here.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionResumePicker {
    pub sessions: Vec<ResumableSession>,
    pub index: usize,
    pub scroll: usize,
}

impl SessionResumePicker {
    pub fn new(sessions: Vec<ResumableSession>) -> Self {
        Self {
            sessions,
            index: 0,
            scroll: 0,
        }
    }

    pub fn move_selection(&mut self, forward: bool, visible_rows: usize) {
        let (index, scroll) = crate::tui::selection::move_selection(
            self.index,
            self.scroll,
            self.sessions.len(),
            visible_rows,
            forward,
        );
        self.index = index;
        self.scroll = scroll;
    }

    pub fn selected(&self) -> Option<&ResumableSession> {
        self.sessions.get(self.index)
    }
}

pub(crate) fn build_resumed_session_args(
    original_args: Option<&str>,
    interactive_args: Option<&str>,
    resume_args: Option<&str>,
    session_resume_cmd: Option<&str>,
    yolo_flag: Option<&str>,
) -> Option<String> {
    let original_args = original_args.map(str::trim).filter(|args| !args.is_empty());
    let inter_args = interactive_args
        .map(str::trim)
        .filter(|args| !args.is_empty());
    let resume_args = resume_args.map(str::trim).filter(|args| !args.is_empty());
    let session_resume_cmd = session_resume_cmd
        .map(str::trim)
        .filter(|args| !args.is_empty());
    let had_yolo = yolo_flag
        .is_some_and(|flag| original_args.is_some_and(|args| args_contain_flag(args, flag)));

    let already_resume_args = original_args.is_some_and(|args| {
        resume_args.is_some_and(|resume| args_contain_sequence(args, resume))
            || session_resume_cmd.is_some_and(|cmd| args_contain_sequence(args, cmd))
    });

    let base_args = if already_resume_args {
        original_args.map(str::to_string)
    } else if resume_args.is_some() {
        join_args(original_args.or(inter_args), resume_args)
    } else {
        original_args.or(inter_args).map(str::to_string)
    };

    append_flag_if_missing(base_args.as_deref(), yolo_flag, had_yolo)
}

#[cfg(test)]
mod resume_picker_tests {
    use super::*;

    // ── ResumableSession::from ──────────────────────────────────

    fn session(id: &str, name: &str, cli: &str, started_at: &str) -> InteractiveSession {
        InteractiveSession {
            id: id.to_string(),
            name: name.to_string(),
            cli: cli.to_string(),
            working_dir: "/tmp".to_string(),
            args: None,
            started_at: started_at.to_string(),
            status: "orphaned".to_string(),
            session_type: "interactive".to_string(),
            pid: None,
            boot_id: None,
        }
    }

    #[test]
    fn resumable_from_interactive_session_carries_identity_and_args() {
        let mut raw = session("s1", "alpha", "claude", "2026-08-19T10:00:00Z");
        raw.args = Some("--resume-me".into());

        let view = ResumableSession::from(&raw);

        assert_eq!(view.id, "s1");
        assert_eq!(view.name, "alpha");
        assert_eq!(view.cli, "claude");
        assert_eq!(view.last_active, "2026-08-19T10:00:00Z");
        assert_eq!(view.working_dir, "/tmp");
        assert_eq!(view.args.as_deref(), Some("--resume-me"));
    }

    // ── SessionResumePicker ─────────────────────────────────────

    #[test]
    fn picker_selecting_second_row_yields_its_session_id() {
        let rows = vec![
            ResumableSession {
                id: "s1".into(),
                name: "alpha".into(),
                cli: "claude".into(),
                last_active: "2026-08-19T12:00:00Z".into(),
                working_dir: "/tmp".into(),
                args: None,
            },
            ResumableSession {
                id: "s2".into(),
                name: "beta".into(),
                cli: "codex".into(),
                last_active: "2026-08-19T11:00:00Z".into(),
                working_dir: "/tmp".into(),
                args: None,
            },
        ];
        let mut picker = SessionResumePicker::new(rows);

        picker.move_selection(true, 6);

        assert_eq!(picker.selected().map(|s| s.id.as_str()), Some("s2"));
    }

    #[test]
    fn picker_navigation_wraps_and_never_panics() {
        let rows = vec![
            ResumableSession {
                id: "s1".into(),
                name: "alpha".into(),
                cli: "claude".into(),
                last_active: "2026-08-19T12:00:00Z".into(),
                working_dir: "/tmp".into(),
                args: None,
            },
            ResumableSession {
                id: "s2".into(),
                name: "beta".into(),
                cli: "codex".into(),
                last_active: "2026-08-19T11:00:00Z".into(),
                working_dir: "/tmp".into(),
                args: None,
            },
        ];
        let mut picker = SessionResumePicker::new(rows);

        picker.move_selection(false, 6);
        assert_eq!(picker.selected().map(|s| s.id.as_str()), Some("s2"));

        picker.move_selection(true, 6);
        assert_eq!(picker.selected().map(|s| s.id.as_str()), Some("s1"));
    }
}
