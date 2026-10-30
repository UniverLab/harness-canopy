use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::context_transfer::{active_split_session_name, resolve_session};
use super::handle_key;
use super::terminal_warp::sync_terminal_warp_buffer_from_pty;
use crate::tui::app::types::{AgentEntry, App, Focus};

// ── Paste handling (bracketed paste) ─────────────────────────────────

/// Normalize all line-ending conventions (`\r\n`, bare `\r`) to `\n`.
/// Bracketed-paste terminals and Windows clipboards routinely deliver line
/// breaks as `\r\n` or bare `\r`; treating anything but `\n` as "not a
/// newline" silently drops line breaks instead of preserving them.
fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Handle pasted text — uses bracketed paste to send text to the PTY without
/// triggering multiple Enter key presses. Preserves newlines for code/YAML/etc.
pub fn handle_paste(app: &mut App, text: &str) {
    match app.focus {
        Focus::Agent => {
            let (vec, idx) = if app.active_split_id.is_some() {
                // Split layouts route paste to whichever panel (session)
                // currently has focus, not the split group itself.
                let Some(name) = active_split_session_name(app) else {
                    return;
                };
                let name = name.to_string();
                resolve_session(app, &name)
            } else {
                match app.selected_agent() {
                    Some(AgentEntry::Interactive(idx)) => ("interactive", *idx),
                    Some(AgentEntry::Terminal(idx)) => ("terminal", *idx),
                    _ => return,
                }
            };
            if idx == usize::MAX {
                return;
            }

            let agent = if vec == "terminal" {
                app.terminal_agents.get_mut(idx)
            } else {
                app.interactive_agents.get_mut(idx)
            };
            if let Some(agent) = agent {
                let bypass = agent.should_bypass_warp_input();
                if agent.warp_mode && !bypass && !agent.warp_passthrough {
                    // Warp prompt editing: insert into input buffer at cursor
                    // (preserves newlines until Enter submits the command).
                    if let Ok(mut buf) = agent.input_buffer.lock() {
                        let pos = agent.warp_cursor.min(buf.len());
                        buf.insert_str(pos, text);
                        agent.warp_cursor = pos + text.len();
                    }
                } else {
                    // Direct to the PTY — wizards, passthrough, and non-warp
                    // sessions alike. Bracketed markers only when the child
                    // program actually enabled bracketed paste mode.
                    let _ = agent.paste_to_pty(text);
                    if agent.warp_mode && agent.warp_passthrough && !bypass {
                        sync_terminal_warp_buffer_from_pty(app, idx, 35);
                    }
                }
            }
        }
        Focus::NewAgentDialog | Focus::PromptTemplateDialog => {
            // Normalize FIRST so every downstream decision (collapse
            // threshold, collapsed-vs-inline branch) sees real newlines
            // regardless of the clipboard's line-ending convention.
            let text = normalize_line_endings(text);
            // A paste of dropped image files attaches them instead of
            // inserting text (drag-and-drop from a file manager arrives as
            // bracketed-paste paths).
            let image_paths = crate::tui::clipboard::parse_image_path_paste(&text);
            if !image_paths.is_empty() && app.simple_prompt_dialog.is_some() {
                attach_dropped_images(app, &image_paths);
            } else {
                insert_builder_paste(app, &text);
            }
            // New-agent dialog has its own prompt input box; route pasted text
            // there too. Newlines are preserved as hard breaks (the renderer
            // wraps with `prompt_visual_line_count` math).
            if let Some(dialog) = &mut app.new_agent_dialog {
                super::new_agent_dialog::insert_prompt_text(dialog, &text);
            }
        }
        _ => {
            // For other contexts, simulate typing each char (no newlines)
            let clean = text.replace('\n', " ").replace('\r', "");
            for c in clean.chars() {
                let _ = handle_key(app, KeyCode::Char(c), KeyModifiers::NONE);
            }
        }
    }
}

/// Insert pasted text into the builder's focused section. Multi-line pastes
/// are collapsed to a placeholder while keeping the real text.
fn insert_builder_paste(app: &mut App, text: &str) {
    let field_width = super::prompt_template::prompt_field_width(app);
    if let Some(dialog) = &mut app.simple_prompt_dialog {
        // Resolve the focused section through the offset-aware mapper
        // (focus index 0 is the send control, sections start at 1).
        // Indexing enabled_sections[focused_section] directly pasted
        // into the NEXT section — never where the cursor was.
        if let Some(section_name) = dialog.focused_section_name().map(str::to_string) {
            // "Multi-line" means more than one logical line — a lone
            // trailing newline stays inline (as a space), it should
            // not produce a collapse placeholder.
            if text.lines().count() > 1 || text.chars().count() > 200 {
                // Preserve newlines for collapsed multi-line paste
                dialog.insert_collapsed_paste_at_cursor(&section_name, text, field_width);
            } else {
                let clean = text.replace('\n', " ");
                dialog.insert_text_at_cursor(&section_name, &clean, field_width);
            }
        }
    }
}

/// Attach dropped image files to the open builder. The first failure
/// (unreadable file, over the cap, shelf rejection) stops the batch with an
/// inline error.
fn attach_dropped_images(app: &mut App, paths: &[std::path::PathBuf]) {
    let db = app.db.clone();
    let shelf_dir = app.data_dir.join("shelf");
    let session_key = app.current_prompt_session_key();
    let Some(dialog) = app.simple_prompt_dialog.as_mut() else {
        return;
    };
    dialog.attach_error = None;
    for path in paths {
        // The parse guarantees an image extension; the fallback is unreachable.
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        // Refuse an over-cap file before reading it: `ShelfStore::insert`
        // checks the length first for the same reason, and a dropped file of
        // any size must never be pulled into memory whole just to reject it.
        if let Ok(len) = std::fs::metadata(path).map(|meta| meta.len()) {
            if len > crate::sessions::shelf::MAX_FILE_BYTES {
                dialog.attach_error = Some(format!(
                    "{} is {len} bytes; the shelf accepts at most {}",
                    path.display(),
                    crate::sessions::shelf::MAX_FILE_BYTES
                ));
                break;
            }
        }
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                dialog.attach_error = Some(format!("{}: {e}", path.display()));
                break;
            }
        };
        if let Err(error) = crate::tui::app::dialog::attach_image_bytes(
            &db,
            &shelf_dir,
            &session_key,
            &mut dialog.attachments,
            &bytes,
            &ext,
        ) {
            dialog.attach_error = Some(error);
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::models::{SplitGroup, SplitOrientation};
    use crate::tui::agent::InteractiveAgent;
    use chrono::Utc;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    /// `cat` is used as a stand-in shell: it never touches the alternate
    /// screen or looks like a sensitive prompt, so pasted text lands in the
    /// warp input buffer instead of going straight to the PTY.
    fn spawn_test_terminal(name: &str) -> InteractiveAgent {
        InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some(name),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn terminal")
    }

    fn app_with_split(session_a: &str, session_b: &str, right_focused: bool) -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.terminal_agents.push(spawn_test_terminal(session_a));
        app.terminal_agents.push(spawn_test_terminal(session_b));
        app.split_groups.push(SplitGroup {
            id: "split-1".to_string(),
            orientation: SplitOrientation::Horizontal,
            session_a: session_a.to_string(),
            session_b: session_b.to_string(),
            created_at: Utc::now(),
        });
        app.active_split_id = Some("split-1".to_string());
        app.split_right_focused = right_focused;
        app.focus = Focus::Agent;
        app
    }

    fn input_buffer_text(agent: &InteractiveAgent) -> String {
        agent
            .input_buffer
            .lock()
            .expect("lock input buffer")
            .clone()
    }

    #[test]
    fn bracketed_paste_routes_to_left_panel_when_left_focused() {
        let mut app = app_with_split("left-term", "right-term", false);

        handle_paste(&mut app, "hello");

        assert_eq!(input_buffer_text(&app.terminal_agents[0]), "hello");
        assert_eq!(input_buffer_text(&app.terminal_agents[1]), "");
    }

    #[test]
    fn bracketed_paste_routes_to_right_panel_when_right_focused() {
        let mut app = app_with_split("left-term", "right-term", true);

        handle_paste(&mut app, "world");

        assert_eq!(input_buffer_text(&app.terminal_agents[0]), "");
        assert_eq!(input_buffer_text(&app.terminal_agents[1]), "world");
    }

    #[test]
    fn bracketed_paste_in_split_is_a_noop_when_session_is_stale() {
        // Regression guard: previously this branch called
        // `resolve_session(app, split_id)`, treating the split's own ID as a
        // session name — it never matched, so paste silently no-opped for
        // every split. Confirm the still-broken/missing-session case stays a
        // clean no-op (not a panic) now that resolution goes through the
        // focused session name.
        let mut app = app_with_split("left-term", "right-term", false);
        app.split_groups[0].session_a = "gone".to_string();

        handle_paste(&mut app, "text");

        assert_eq!(input_buffer_text(&app.terminal_agents[0]), "");
        assert_eq!(input_buffer_text(&app.terminal_agents[1]), "");
    }

    /// Regression guard: paste must land in the FOCUSED section. The old
    /// code indexed `enabled_sections[focused_section]` without the +1
    /// send-control offset, so pasting while editing the first section
    /// dumped the text into the section below it.
    #[test]
    fn prompt_builder_paste_lands_in_the_focused_section() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        let mut dialog = crate::tui::app::dialog::SimplePromptDialog::new();
        dialog.add_section_with_content("context", String::new());
        dialog.focused_section = 1; // first section (instruction_1)
        app.simple_prompt_dialog = Some(dialog);
        app.focus = Focus::PromptTemplateDialog;

        handle_paste(&mut app, "pasted-here");

        let dialog = app.simple_prompt_dialog.as_ref().unwrap();
        assert_eq!(dialog.get_section_content("instruction_1"), "pasted-here");
        assert_eq!(dialog.get_section_content("context_1"), "");
    }

    /// A large paste still collapses to a placeholder, in the focused
    /// section, at the cursor.
    #[test]
    fn prompt_builder_large_paste_collapses_in_focused_section() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        let mut dialog = crate::tui::app::dialog::SimplePromptDialog::new();
        dialog.add_section_with_content("context", String::new());
        dialog.focused_section = 1;
        app.simple_prompt_dialog = Some(dialog);
        app.focus = Focus::PromptTemplateDialog;

        let big = "line one\nline two\nline three\n";
        handle_paste(&mut app, big);

        let dialog = app.simple_prompt_dialog.as_ref().unwrap();
        assert!(
            dialog
                .get_section_content("instruction_1")
                .contains("[Pasted ~"),
            "multi-line paste should collapse to a placeholder"
        );
        assert_eq!(dialog.get_section_content("context_1"), "");
    }

    /// B30 regression: a paste whose line breaks arrive as bare `\r` (common
    /// with bracketed paste) or `\r\n` (Windows clipboards) must preserve
    /// newlines identically to a `\n`-delimited paste — not have them
    /// silently deleted.
    #[test]
    fn prompt_builder_paste_preserves_line_breaks_regardless_of_line_ending() {
        let variants: [(&str, &str); 3] = [
            ("lf", "line one\nline two\nline three"),
            ("crlf", "line one\r\nline two\r\nline three"),
            ("cr", "line one\rline two\rline three"),
        ];

        let mut resolved_contents = Vec::new();
        let mut placeholders = Vec::new();

        for (_label, text) in variants {
            let db = test_db();
            let data_dir = tempdir().expect("create data dir");
            let mut app = App::new(
                Arc::clone(&db),
                data_dir.path(),
                &crate::domain::canopy_config::CanopyConfig::default(),
            )
            .expect("create app");
            app.focus = Focus::PromptTemplateDialog;
            app.simple_prompt_dialog = Some(crate::tui::app::dialog::SimplePromptDialog::new());

            handle_paste(&mut app, text);

            let dialog = app.simple_prompt_dialog.as_ref().unwrap();
            let placeholder = dialog.get_section_content("instruction_1");
            assert!(
                placeholder.contains("[Pasted ~3 lines]"),
                "expected a 3-line collapse placeholder, got {placeholder:?}"
            );
            let resolved = dialog
                .section_content_for_build("instruction_1")
                .unwrap()
                .to_string();
            assert_eq!(
                resolved, "line one\nline two\nline three",
                "resolved content should have normalized newlines"
            );

            let composed = dialog
                .build_prompt_with_resolved_resources(&db, data_dir.path())
                .expect("build prompt");
            // push_xml_item indents every content line with four spaces.
            assert!(
                composed.contains("line one\n    line two\n    line three"),
                "composed prompt should preserve line breaks"
            );

            placeholders.push(placeholder);
            resolved_contents.push(composed);
        }

        assert_eq!(placeholders[0], placeholders[1]);
        assert_eq!(placeholders[1], placeholders[2]);
        assert_eq!(resolved_contents[0], resolved_contents[1]);
        assert_eq!(resolved_contents[1], resolved_contents[2]);
    }

    /// B30 regression: a single-line paste with a trailing `\r` (or `\r\n`)
    /// must not collapse and must not retain the stray carriage return.
    #[test]
    fn single_line_paste_with_cr_stays_inline_and_clean() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.focus = Focus::PromptTemplateDialog;
        app.simple_prompt_dialog = Some(crate::tui::app::dialog::SimplePromptDialog::new());

        handle_paste(&mut app, "single line\r");

        let dialog = app.simple_prompt_dialog.as_ref().unwrap();
        assert_eq!(dialog.get_section_content("instruction_1"), "single line ");
    }

    /// A paste of dropped image files attaches them instead of inserting
    /// text (drag-and-drop from a file manager arrives as bracketed-paste
    /// paths).
    #[test]
    fn prompt_builder_path_paste_attaches_images_not_text() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.focus = Focus::PromptTemplateDialog;
        app.simple_prompt_dialog = Some(crate::tui::app::dialog::SimplePromptDialog::new());

        let shot = data_dir.path().join("shot.png");
        std::fs::write(
            &shot,
            [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x42],
        )
        .expect("write image fixture");

        handle_paste(&mut app, &shot.to_string_lossy());

        let dialog = app.simple_prompt_dialog.as_ref().unwrap();
        assert_eq!(dialog.attachments.len(), 1);
        assert_eq!(dialog.get_section_content("instruction_1"), "");
        assert!(dialog.attach_error.is_none());
        // …and a later text paste still inserts text, leaving chips alone.
        handle_paste(&mut app, "hello");

        let dialog = app.simple_prompt_dialog.as_ref().unwrap();
        assert_eq!(dialog.get_section_content("instruction_1"), "hello");
        assert_eq!(dialog.attachments.len(), 1);
    }

    /// An over-cap dropped image is refused with an inline error: nothing
    /// attaches and nothing is inserted as text either (the paste was an
    /// image drop, not text).
    #[test]
    fn prompt_builder_oversize_drop_is_rejected_with_an_inline_error() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.focus = Focus::PromptTemplateDialog;
        app.simple_prompt_dialog = Some(crate::tui::app::dialog::SimplePromptDialog::new());

        // Sparse: the length trips the cap without writing 20 MiB.
        let big = data_dir.path().join("big.png");
        let file = std::fs::File::create(&big).expect("create fixture");
        file.set_len(crate::sessions::shelf::MAX_FILE_BYTES + 1)
            .expect("stretch fixture");
        drop(file);

        handle_paste(&mut app, &big.to_string_lossy());

        let dialog = app.simple_prompt_dialog.as_ref().unwrap();
        assert!(dialog.attachments.is_empty());
        assert_eq!(dialog.get_section_content("instruction_1"), "");
        let error = dialog.attach_error.as_deref().expect("inline error");
        assert!(
            error.contains(&crate::sessions::shelf::MAX_FILE_BYTES.to_string()),
            "names the cap: {error}"
        );
    }

    /// A paste of non-image paths stays text: only all-image pastes attach.
    #[test]
    fn prompt_builder_non_image_path_paste_stays_text() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.focus = Focus::PromptTemplateDialog;
        app.simple_prompt_dialog = Some(crate::tui::app::dialog::SimplePromptDialog::new());

        let notes = data_dir.path().join("notes.txt");
        std::fs::write(&notes, b"not an image").expect("write fixture");

        handle_paste(&mut app, &notes.to_string_lossy());

        let dialog = app.simple_prompt_dialog.as_ref().unwrap();
        assert!(dialog.attachments.is_empty());
        assert!(
            dialog
                .get_section_content("instruction_1")
                .contains("notes.txt"),
            "the path should be inserted as text"
        );
    }

    /// B30 regression: the new-agent dialog's paste path must apply the same
    /// normalization instead of stripping bare `\r` line breaks outright.
    #[test]
    fn new_agent_dialog_paste_preserves_cr_only_line_breaks() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.focus = Focus::NewAgentDialog;
        app.new_agent_dialog = Some(crate::tui::app::dialog::NewAgentDialog::new(None));

        handle_paste(&mut app, "first line\rsecond line");

        let dialog = app.new_agent_dialog.as_ref().unwrap();
        assert_eq!(dialog.prompt, "first line\nsecond line");
    }
}
