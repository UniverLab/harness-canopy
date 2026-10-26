//! Part 02 of `tests` (split from `src/tui/app/dialog/prompt.rs`, pure move).
use super::*;

// ── is_file_reference ────────────────────────────────────────

#[test]
fn is_file_reference_valid() {
    assert!(SimplePromptDialog::is_file_reference("@src/lib.rs"));
    assert!(SimplePromptDialog::is_file_reference("@file.txt"));
    assert!(SimplePromptDialog::is_file_reference("@a"));
}

#[test]
fn is_file_reference_single_at() {
    assert!(!SimplePromptDialog::is_file_reference("@"));
}

#[test]
fn is_file_reference_special_chars() {
    assert!(!SimplePromptDialog::is_file_reference("@file with spaces"));
    assert!(SimplePromptDialog::is_file_reference("@file_name-1.0.rs"));
}

// ── resolve_rag_scope ────────────────────────────────────────

#[test]
fn resolve_rag_scope_global_prefix() {
    let (scope, query) = SimplePromptDialog::resolve_rag_scope("global:my query", None);
    assert!(matches!(scope, RagScope::Global));
    assert_eq!(query, "my query");
}

#[test]
fn resolve_rag_scope_project_prefix() {
    let (scope, query) = SimplePromptDialog::resolve_rag_scope("project:abc123:my query", None);
    assert!(matches!(scope, RagScope::Project("abc123")));
    assert_eq!(query, "my query");
}

#[test]
fn resolve_rag_scope_no_prefix_with_default() {
    let (scope, query) = SimplePromptDialog::resolve_rag_scope("my query", Some("hash1"));
    assert!(matches!(scope, RagScope::Project("hash1")));
    assert_eq!(query, "my query");
}

#[test]
fn resolve_rag_scope_no_prefix_no_default() {
    let (scope, query) = SimplePromptDialog::resolve_rag_scope("my query", None);
    assert!(matches!(scope, RagScope::Global));
    assert_eq!(query, "my query");
}

// ── next_file_reference ──────────────────────────────────────

#[test]
fn next_file_reference_finds_at() {
    let text = "look at @src/lib.rs for details";
    let result = SimplePromptDialog::next_file_reference(text, 0);
    assert!(result.is_some());
    let (pos, _ref, next) = result.unwrap();
    assert_eq!(pos, 8); // "look at @" — @ is at index 8
    assert_eq!(_ref, "@src/lib.rs");
    assert!(next > pos);
}

#[test]
fn next_file_reference_no_at() {
    let text = "no references here";
    assert!(SimplePromptDialog::next_file_reference(text, 0).is_none());
}

#[test]
fn next_file_reference_with_offset() {
    let text = "first @one then @two";
    let (pos1, ref1, next1) = SimplePromptDialog::next_file_reference(text, 0).unwrap();
    assert_eq!(pos1, 6);
    assert_eq!(ref1, "@one");
    let (pos2, ref2, _) = SimplePromptDialog::next_file_reference(text, next1).unwrap();
    assert_eq!(pos2, 16);
    assert_eq!(ref2, "@two");
}

// ── focus navigation ─────────────────────────────────────────

#[test]
fn focus_next_wraps_from_last_to_send() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("goal");
    let last = dialog.enabled_sections.len();
    dialog.focused_section = last; // last section
    dialog.focus_next();
    assert_eq!(dialog.focused_section, 0); // send control
}

#[test]
fn focus_next_wraps_from_send_to_first() {
    let mut dialog = SimplePromptDialog::new();
    dialog.focused_section = 0;
    dialog.focus_next();
    assert_eq!(dialog.focused_section, 1);
}

#[test]
fn focus_prev_from_send_goes_to_last() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("goal");
    dialog.focused_section = 0;
    dialog.focus_prev();
    assert_eq!(dialog.focused_section, dialog.enabled_sections.len());
}

#[test]
fn focus_prev_from_first_goes_to_send() {
    let mut dialog = SimplePromptDialog::new();
    dialog.focused_section = 1;
    dialog.focus_prev();
    assert_eq!(dialog.focused_section, 0);
}

#[test]
fn focused_section_index_send_returns_none() {
    let mut dialog = SimplePromptDialog::new();
    dialog.focused_section = 0;
    assert!(dialog.focused_section_index().is_none());
}

#[test]
fn focused_section_index_section_returns_some() {
    let dialog = SimplePromptDialog::new();
    assert_eq!(dialog.focused_section_index(), Some(0));
}

// ── send display ─────────────────────────────────────────────

#[test]
fn send_display_now() {
    let dialog = SimplePromptDialog::new();
    assert_eq!(dialog.send_display("%Y-%m-%d %H:%M"), "now");
}

#[test]
fn send_display_date_no_time() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_choice = SendChoice::Date;
    assert_eq!(dialog.send_display("%Y-%m-%d %H:%M"), "date");
}

#[test]
fn send_display_date_with_time() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_choice = SendChoice::Date;
    dialog.send_at = Some(
        chrono::NaiveDate::from_ymd_opt(2026, 7, 20)
            .unwrap()
            .and_hms_opt(14, 30, 0)
            .unwrap(),
    );
    assert_eq!(dialog.send_display("%Y-%m-%d %H:%M"), "2026-07-20 14:30");
}

// ── send toggle and clear ────────────────────────────────────

#[test]
fn send_toggle_now_to_date() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_toggle();
    assert_eq!(dialog.send_choice, SendChoice::Date);
}

#[test]
fn send_toggle_date_to_now_clears() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_choice = SendChoice::Date;
    dialog.send_at = Some(chrono::Local::now().naive_local());
    dialog.send_edit = Some(SendAtEdit {
        value: chrono::Local::now().naive_local(),
        field: 0,
        typed: 0,
        typed_len: 0,
    });
    dialog.send_toggle();
    assert_eq!(dialog.send_choice, SendChoice::Now);
    assert!(dialog.send_at.is_none());
    assert!(dialog.send_edit.is_none());
}

#[test]
fn clear_send_at() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_choice = SendChoice::Date;
    dialog.send_at = Some(chrono::Local::now().naive_local());
    dialog.send_edit = Some(SendAtEdit {
        value: chrono::Local::now().naive_local(),
        field: 0,
        typed: 0,
        typed_len: 0,
    });
    dialog.clear_send_at();
    assert_eq!(dialog.send_choice, SendChoice::Now);
    assert!(dialog.send_at.is_none());
    assert!(dialog.send_edit.is_none());
}

// ── send_edit operations ─────────────────────────────────────

#[test]
fn send_edit_move_clamps_field() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_begin_edit();
    dialog.send_edit_move(10);
    let edit = dialog.send_edit.unwrap();
    assert_eq!(edit.field, 4); // clamped to minute
    dialog.send_edit_move(-100);
    let edit = dialog.send_edit.unwrap();
    assert_eq!(edit.field, 0); // clamped to year
}

#[test]
fn send_edit_adjust_day() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_begin_edit();
    let before = dialog.send_edit.as_ref().unwrap().value;
    dialog.send_edit.as_mut().unwrap().field = 2; // day
    dialog.send_edit_adjust(1);
    let edit = dialog.send_edit.unwrap();
    let expected = (before + chrono::Duration::days(1)).day();
    assert_eq!(edit.value.day(), expected);
}

#[test]
fn send_edit_type_digit_auto_advances_field() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_begin_edit();
    dialog.send_edit.as_mut().unwrap().field = 3; // hour (width 2)
    dialog.send_edit_type_digit(1);
    // After 1 digit (not yet full), typed=1, typed_len=1
    assert_eq!(dialog.send_edit.as_ref().unwrap().typed, 1);
    assert_eq!(dialog.send_edit.as_ref().unwrap().typed_len, 1);
    dialog.send_edit_type_digit(4);
    // After 2 digits (field full), auto-advances to minute, typed resets
    assert_eq!(dialog.send_edit.as_ref().unwrap().field, 4);
    assert_eq!(dialog.send_edit.as_ref().unwrap().typed, 0);
}

#[test]
fn send_edit_confirm_past_rejects() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_begin_edit();
    dialog.send_edit.as_mut().unwrap().value = chrono::NaiveDateTime::default(); // epoch
    assert!(!dialog.send_edit_confirm());
    assert!(dialog.send_error.is_some());
}

#[test]
fn send_edit_cancel_no_prior_send_returns_now() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_begin_edit();
    dialog.send_edit_cancel();
    assert!(dialog.send_edit.is_none());
    assert_eq!(dialog.send_choice, SendChoice::Now);
}

#[test]
fn send_edit_cancel_with_prior_send_stays_date() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_choice = SendChoice::Date;
    dialog.send_at = Some(chrono::Local::now().naive_local());
    dialog.send_begin_edit();
    dialog.send_edit_cancel();
    assert!(dialog.send_edit.is_none());
    assert_eq!(dialog.send_choice, SendChoice::Date);
}

// ── is_send_at_focused ───────────────────────────────────────

#[test]
fn is_send_at_focused_true() {
    let mut dialog = SimplePromptDialog::new();
    dialog.focused_section = 0;
    assert!(dialog.is_send_at_focused());
}

#[test]
fn is_send_at_focused_false_on_section() {
    let dialog = SimplePromptDialog::new();
    assert!(!dialog.is_send_at_focused());
}

// ── total_focusable ──────────────────────────────────────────

#[test]
fn total_focusable_empty_sections() {
    let mut dialog = SimplePromptDialog::new();
    dialog.enabled_sections.clear();
    assert_eq!(dialog.total_focusable(), 1); // just send control
}

#[test]
fn total_focusable_with_sections() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("goal");
    dialog.add_section("context");
    // instruction_1 + goal_1 + context_1 = 3 sections + send = 4
    assert_eq!(dialog.total_focusable(), 4);
}

// ── section_entries and section_lines ─────────────────────────

#[test]
fn section_entries_filters_empty() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "  ".to_string());
    assert!(dialog.section_entries("instruction").is_empty());
}

#[test]
fn section_entries_includes_non_empty() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "do something".to_string());
    assert_eq!(dialog.section_entries("instruction").len(), 1);
}

#[test]
fn section_lines_splits_multiline() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "line1\nline2\nline3".to_string());
    let lines = dialog.section_lines("instruction");
    assert_eq!(lines.len(), 3);
}

// ── build_body ───────────────────────────────────────────────

#[test]
fn build_body_empty_dialog() {
    let dialog = SimplePromptDialog::new();
    let body = dialog.build_body();
    assert!(!body.contains("GOAL"));
    assert!(!body.contains("CONTEXT"));
    assert!(body.contains("INSTRUCTIONS"));
}

#[test]
fn build_body_with_goal() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "do it".to_string());
    dialog.add_section_with_content("goal", "ship the feature".to_string());
    let body = dialog.build_body();
    assert!(body.contains("GOAL"));
    assert!(body.contains("ship the feature"));
}

// ── collect_tool_lines ───────────────────────────────────────

#[test]
fn collect_tool_lines_empty() {
    let dialog = SimplePromptDialog::new();
    assert!(dialog.collect_tool_lines().is_empty());
}

#[test]
fn collect_tool_lines_with_content() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section_with_content(
        "tools",
        "skill:code-engineering\nskill:rust-idiomatic".to_string(),
    );
    let lines = dialog.collect_tool_lines();
    assert_eq!(lines.len(), 2);
}

// ── PromptBuilderSession round-trip ──────────────────────────

#[test]
fn prompt_builder_session_from_dialog_copies_all_fields() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "test".to_string());
    dialog.add_section_with_content("context", "ctx".to_string());
    dialog.send_choice = SendChoice::Date;
    dialog.send_at = Some(chrono::Local::now().naive_local());

    let session = PromptBuilderSession::from_dialog(&dialog);
    assert_eq!(
        session.sections.get("instruction_1").map(String::as_str),
        Some("test")
    );
    // Find the context section (it will have a generated name like "context_1" or "context_2")
    let ctx_val = session
        .sections
        .iter()
        .find(|(k, _)| k.starts_with("context"))
        .map(|(_, v)| v.as_str());
    assert_eq!(ctx_val, Some("ctx"));
    // send_at is persisted in PromptBuilderSession
    assert!(session.send_at.is_some());
}

// ── PersistedBuilderState round-trip ─────────────────────────

#[test]
fn persisted_builder_state_excludes_send_at() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "test".to_string());
    dialog.send_choice = SendChoice::Date;
    dialog.send_at = Some(chrono::Local::now().naive_local());

    let snapshot = PersistedBuilderState::from_dialog(&dialog);
    let json = serde_json::to_string(&snapshot).unwrap();
    let restored: PersistedBuilderState = serde_json::from_str(&json).unwrap();

    let mut target = SimplePromptDialog::new();
    restored.restore_into(&mut target);
    // send_at must NOT be restored from persisted state
    assert!(target.send_at.is_none());
}

// ── get_removable_sections ───────────────────────────────────

#[test]
fn get_removable_sections_single_instruction_not_removable() {
    let dialog = SimplePromptDialog::new();
    let removable = dialog.get_removable_sections();
    assert!(removable.is_empty());
}

#[test]
fn get_removable_sections_multiple_instructions_all_removable() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("instruction");
    let removable = dialog.get_removable_sections();
    assert_eq!(removable.len(), 2);
}

#[test]
fn get_removable_sections_non_instruction_always_removable() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("context");
    let removable = dialog.get_removable_sections();
    assert!(removable.iter().any(|(id, _)| id.starts_with("context")));
}

// ── remove_section protection ────────────────────────────────

#[test]
fn remove_section_last_instruction_protected() {
    let mut dialog = SimplePromptDialog::new();
    let id = dialog.enabled_sections[0].clone();
    dialog.remove_section(&id);
    // Should not have removed it
    assert_eq!(dialog.enabled_sections.len(), 1);
}

#[test]
fn remove_section_non_instruction_removable() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("context");
    let ctx_id = dialog
        .enabled_sections
        .iter()
        .find(|id| id.starts_with("context"))
        .cloned()
        .unwrap();
    let len_before = dialog.enabled_sections.len();
    dialog.remove_section(&ctx_id);
    assert_eq!(dialog.enabled_sections.len(), len_before - 1);
}

// ── get_section_content / set_section_content ─────────────────

#[test]
fn get_section_content_missing_returns_empty() {
    let dialog = SimplePromptDialog::new();
    assert!(dialog.get_section_content("nonexistent").is_empty());
}

#[test]
fn set_section_content_round_trip() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "hello".to_string());
    assert_eq!(dialog.get_section_content("instruction_1"), "hello");
}

// ── section_content_for_build with collapsed paste ────────────

#[test]
fn section_content_for_build_returns_collapsed_paste() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "[Pasted ~3 lines]".to_string());
    dialog.collapsed_pastes.insert(
        "instruction_1".to_string(),
        "real content\nmore lines\nfinal".to_string(),
    );
    assert_eq!(
        dialog.section_content_for_build("instruction_1"),
        Some("real content\nmore lines\nfinal")
    );
}

// ── should_collapse_paste ────────────────────────────────────

#[test]
fn should_collapse_paste_single_short_line() {
    assert!(!SimplePromptDialog::should_collapse_paste("hello"));
}

#[test]
fn should_collapse_paste_multiline() {
    assert!(SimplePromptDialog::should_collapse_paste("line1\nline2"));
}

#[test]
fn should_collapse_paste_long_line() {
    assert!(SimplePromptDialog::should_collapse_paste(&"a".repeat(201)));
}

#[test]
fn should_collapse_paste_exactly_200_chars() {
    assert!(!SimplePromptDialog::should_collapse_paste(&"a".repeat(200)));
}

// ── expand_collapsed_paste ───────────────────────────────────

#[test]
fn expand_collapsed_paste_restores_content() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "[Pasted ~2 lines]".to_string());
    dialog
        .collapsed_pastes
        .insert("instruction_1".to_string(), "real\ncontent".to_string());
    dialog.expand_collapsed_paste("instruction_1");
    assert_eq!(dialog.get_section_content("instruction_1"), "real\ncontent");
    assert!(!dialog.has_collapsed_paste("instruction_1"));
}

#[test]
fn expand_collapsed_paste_noop_without_paste() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "normal".to_string());
    dialog.expand_collapsed_paste("instruction_1");
    assert_eq!(dialog.get_section_content("instruction_1"), "normal");
}

// ── cursor_in_collapsed_placeholder ───────────────────────────

#[test]
fn cursor_in_collapsed_placeholder_no_paste() {
    let dialog = SimplePromptDialog::new();
    assert!(!dialog.cursor_in_collapsed_placeholder("instruction_1"));
}

// ── scroll_raw_preview ───────────────────────────────────────

#[test]
fn scroll_raw_preview_clamps() {
    let mut dialog = SimplePromptDialog::new();
    dialog.raw_preview = Some("line1\nline2\nline3".to_string());
    dialog.scroll_raw_preview(100);
    assert_eq!(dialog.raw_preview_scroll, 2); // max = 3 lines - 1 = 2
}

#[test]
fn scroll_raw_preview_negative_clamps_to_zero() {
    let mut dialog = SimplePromptDialog::new();
    dialog.raw_preview = Some("line1\nline2".to_string());
    dialog.raw_preview_scroll = 0;
    dialog.scroll_raw_preview(-5);
    assert_eq!(dialog.raw_preview_scroll, 0);
}

// ── load_flat_text ───────────────────────────────────────────

#[test]
fn load_flat_text_resets_counters() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("goal");
    dialog.add_section("context");
    dialog.load_flat_text("recovered");
    assert_eq!(dialog.enabled_sections.len(), 1);
    assert_eq!(dialog.get_section_content("instruction_1"), "recovered");
}

// ── insert_collapsed_paste_at_cursor short text ──────────────

#[test]
fn insert_collapsed_paste_short_text_not_collapsed() {
    let mut dialog = SimplePromptDialog::new();
    dialog.insert_collapsed_paste_at_cursor("instruction_1", "short", 80);
    assert_eq!(dialog.get_section_content("instruction_1"), "short");
    assert!(!dialog.has_collapsed_paste("instruction_1"));
}

// ── backspace_collapsed_paste without paste ───────────────────

#[test]
fn backspace_collapsed_paste_noop_without_paste() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "normal text".to_string());
    dialog.backspace_collapsed_paste("instruction_1", 80);
    assert_eq!(dialog.get_section_content("instruction_1"), "normal text");
}

// ── add_resource_reference ───────────────────────────────────

#[test]
fn add_resource_reference_no_existing_section() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_resource_reference("/path/to/file");
    assert!(dialog
        .enabled_sections
        .iter()
        .any(|id| id.starts_with("resources")));
    assert_eq!(
        dialog.get_section_content(&dialog.resources_section_id().unwrap()),
        "/path/to/file"
    );
}

#[test]
fn add_resource_reference_appends_to_existing() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section_with_content("resources", "existing".to_string());
    dialog.add_resource_reference("/new/path");
    let content = dialog.get_section_content(&dialog.resources_section_id().unwrap());
    assert!(content.contains("existing"));
    assert!(content.contains("/new/path"));
}

// ── resources_section_id ─────────────────────────────────────

#[test]
fn resources_section_id_none_when_empty() {
    let dialog = SimplePromptDialog::new();
    assert!(dialog.resources_section_id().is_none());
}

#[test]
fn resources_section_id_found() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("resources");
    assert!(dialog.resources_section_id().is_some());
}

// ── display name ─────────────────────────────────────────────

#[test]
fn section_display_name_with_number() {
    let name = SimplePromptDialog::section_display_name("instruction_1");
    assert!(name.contains("Instruction"));
    assert!(name.contains("1"));
}

#[test]
fn section_display_name_without_number() {
    let name = SimplePromptDialog::section_display_name("tools");
    assert!(name.contains("Tools"));
}

// ── format_file_resource ────────────────────────────────────

#[test]
fn format_file_resource_shows_path_and_kind() {
    let result = SimplePromptDialog::format_file_resource(Path::new("/tmp/test.rs"));
    assert!(result.contains("path: /tmp/test.rs"));
    assert!(result.contains("kind: file"));
}

// ── format_project_block ────────────────────────────────────

#[test]
fn format_project_block_required_fields() {
    use crate::domain::project::Project;
    let project = Project {
        hash: "abc123".to_string(),
        name: "test-project".to_string(),
        path: "/tmp/project".to_string(),
        description: None,
        tags: None,
        indexed_at: None,
        created_at: 0,
    };
    let result = SimplePromptDialog::format_project_block(&project);
    assert!(result.contains("name: test-project"));
    assert!(result.contains("workdir_hash: abc123"));
    assert!(result.contains("path: /tmp/project"));
    assert!(!result.contains("description:"));
    assert!(!result.contains("tags:"));
    assert!(!result.contains("indexed_at:"));
}

#[test]
fn format_project_block_with_description() {
    use crate::domain::project::Project;
    let project = Project {
        hash: "def456".to_string(),
        name: "proj".to_string(),
        path: "/p".to_string(),
        description: Some("a project".to_string()),
        tags: None,
        indexed_at: None,
        created_at: 0,
    };
    let result = SimplePromptDialog::format_project_block(&project);
    assert!(result.contains("description: a project"));
}

#[test]
fn format_project_block_with_tags() {
    use crate::domain::project::Project;
    let project = Project {
        hash: "ghi".to_string(),
        name: "p".to_string(),
        path: "/p".to_string(),
        description: None,
        tags: Some("rust,tui".to_string()),
        indexed_at: None,
        created_at: 0,
    };
    let result = SimplePromptDialog::format_project_block(&project);
    assert!(result.contains("tags: rust,tui"));
}

#[test]
fn format_project_block_with_indexed_at() {
    use crate::domain::project::Project;
    let project = Project {
        hash: "jkl".to_string(),
        name: "p".to_string(),
        path: "/p".to_string(),
        description: None,
        tags: None,
        indexed_at: Some(1700000000),
        created_at: 0,
    };
    let result = SimplePromptDialog::format_project_block(&project);
    assert!(result.contains("indexed_at:"));
}

// ── is_locked / lock_section ────────────────────────────────

#[test]
fn is_locked_false_for_new_section() {
    let dialog = SimplePromptDialog::new();
    assert!(!dialog.is_locked("instruction_1"));
}

#[test]
fn is_locked_true_after_lock() {
    let mut dialog = SimplePromptDialog::new();
    dialog.lock_section("instruction_1");
    assert!(dialog.is_locked("instruction_1"));
}

#[test]
fn is_locked_false_for_other_section() {
    let mut dialog = SimplePromptDialog::new();
    dialog.lock_section("instruction_1");
    assert!(!dialog.is_locked("context_1"));
}

// ── set_tools_section_skill ─────────────────────────────────

#[test]
fn set_tools_section_skill_sets_content() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("tools");
    let tools_id = dialog
        .enabled_sections
        .iter()
        .find(|id| id.starts_with("tools"))
        .cloned()
        .unwrap();
    dialog.set_tools_section_skill(&tools_id, "skill:code-engineering");
    assert_eq!(
        dialog.get_section_content(&tools_id),
        "skill:code-engineering"
    );
}

#[test]
fn set_tools_section_skill_replaces_existing() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section_with_content("tools", "old-skill".to_string());
    let tools_id = dialog
        .enabled_sections
        .iter()
        .find(|id| id.starts_with("tools"))
        .cloned()
        .unwrap();
    dialog.set_tools_section_skill(&tools_id, "new-skill");
    assert_eq!(dialog.get_section_content(&tools_id), "new-skill");
}

// ── generate_section_id ─────────────────────────────────────

#[test]
fn generate_section_id_increments_counter() {
    let mut dialog = SimplePromptDialog::new();
    let id1 = dialog.generate_section_id("goal");
    let id2 = dialog.generate_section_id("goal");
    let id3 = dialog.generate_section_id("goal");
    assert_eq!(id1, "goal_1");
    assert_eq!(id2, "goal_2");
    assert_eq!(id3, "goal_3");
}

#[test]
fn generate_section_id_independent_per_type() {
    let mut dialog = SimplePromptDialog::new();
    let g = dialog.generate_section_id("goal");
    let c = dialog.generate_section_id("context");
    assert_eq!(g, "goal_1");
    // context counter starts at 2 from new()
    assert_eq!(c, "context_2");
}

// ── instruction_count ───────────────────────────────────────

#[test]
fn instruction_count_single() {
    let dialog = SimplePromptDialog::new();
    assert_eq!(dialog.instruction_count(), 1);
}

#[test]
fn instruction_count_after_add() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("instruction");
    assert_eq!(dialog.instruction_count(), 2);
}

#[test]
fn instruction_count_after_remove() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("instruction");
    assert_eq!(dialog.instruction_count(), 2);
    let extra = dialog
        .enabled_sections
        .iter()
        .find(|id| id.starts_with("instruction") && *id != "instruction_1")
        .cloned()
        .unwrap();
    dialog.remove_section(&extra);
    assert_eq!(dialog.instruction_count(), 1);
}

// ── cursor / scroll edge cases ──────────────────────────────

#[test]
fn cursor_default_for_missing_section() {
    let dialog = SimplePromptDialog::new();
    assert_eq!(dialog.cursor("nonexistent"), 0);
}

#[test]
fn scroll_default_for_missing_section() {
    let dialog = SimplePromptDialog::new();
    assert_eq!(dialog.scroll("nonexistent"), 0);
}

#[test]
fn cursor_returns_stored_value() {
    let mut dialog = SimplePromptDialog::new();
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 5);
    assert_eq!(dialog.cursor("instruction_1"), 5);
}

#[test]
fn scroll_returns_stored_value() {
    let mut dialog = SimplePromptDialog::new();
    dialog
        .section_scrolls
        .insert("instruction_1".to_string(), 3);
    assert_eq!(dialog.scroll("instruction_1"), 3);
}

// ── is_file_reference more edge cases ───────────────────────

#[test]
fn is_file_reference_with_path_separator() {
    assert!(SimplePromptDialog::is_file_reference("@src/main.rs"));
    assert!(SimplePromptDialog::is_file_reference("@a/b/c"));
}

#[test]
fn is_file_reference_with_underscore() {
    assert!(SimplePromptDialog::is_file_reference("@my_file"));
}

#[test]
fn is_file_reference_with_hyphen() {
    assert!(SimplePromptDialog::is_file_reference("@my-file"));
}

#[test]
fn is_file_reference_with_dot() {
    assert!(SimplePromptDialog::is_file_reference("@file.txt"));
    assert!(SimplePromptDialog::is_file_reference("@.hidden"));
}

// ── next_file_reference edge cases ──────────────────────────

#[test]
fn next_file_reference_at_end_of_string() {
    let text = "hello @";
    assert!(SimplePromptDialog::next_file_reference(text, 0).is_some());
}

#[test]
fn next_file_reference_at_only_char() {
    let text = "@";
    let result = SimplePromptDialog::next_file_reference(text, 0);
    assert!(result.is_some());
}

// ── visual_line_count more edge cases ───────────────────────

#[test]
fn visual_line_count_mixed_newlines_and_wrap() {
    // "ab\ncdefghij" with width 5 → "ab" (1 line) + newline → "cdef" + "ghij" = 3 lines
    assert_eq!(SimplePromptDialog::visual_line_count("ab\ncdefghij", 5), 3);
}

#[test]
fn visual_line_count_tab_at_end_of_line() {
    // Tab at col 2 with width 5: tab takes 2 cols (4 - 2%4 = 2), total col=4 ≤ 5 → 1 line
    assert_eq!(SimplePromptDialog::visual_line_count("ab\t", 5), 1);
}

#[test]
fn visual_line_count_multiple_tabs() {
    assert_eq!(SimplePromptDialog::visual_line_count("\t\t", 10), 1);
}

// ── get_available_sections ──────────────────────────────────

#[test]
fn get_available_sections_has_core_types() {
    let sections = SimplePromptDialog::get_available_sections();
    let names: Vec<&str> = sections.iter().map(|(name, _)| *name).collect();
    assert!(names.contains(&"instruction"));
    assert!(names.contains(&"goal"));
    assert!(names.contains(&"context"));
    assert!(names.contains(&"resources"));
    assert!(names.contains(&"constraints"));
    assert!(names.contains(&"tools"));
    assert!(names.contains(&"preset"));
}

// ── focused_section_name edge cases ─────────────────────────

#[test]
fn focused_section_name_send_control_returns_none() {
    let mut dialog = SimplePromptDialog::new();
    dialog.focused_section = 0;
    assert!(dialog.focused_section_name().is_none());
}

#[test]
fn focused_section_name_first_section() {
    let dialog = SimplePromptDialog::new();
    assert_eq!(dialog.focused_section_name(), Some("instruction_1"));
}

#[test]
fn focused_section_name_out_of_bounds_returns_none() {
    let mut dialog = SimplePromptDialog::new();
    dialog.focused_section = 99;
    assert!(dialog.focused_section_name().is_none());
}

// ── add_resource_reference: existing content is empty ───────

#[test]
fn add_resource_reference_empty_existing_content() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section_with_content("resources", String::new());
    dialog.add_resource_reference("/new/path");
    let rid = dialog.resources_section_id().unwrap();
    assert_eq!(dialog.get_section_content(&rid), "/new/path");
}

// ── strip_resources_section edge cases ───────────────────────

#[test]
fn strip_resources_section_empty_string() {
    assert_eq!(strip_resources_section(""), "");
}

#[test]
fn strip_resources_section_only_header_no_closing() {
    let input = "# [RESOURCES]: Knowledge Base & Data\n<resources>\n";
    assert_eq!(strip_resources_section(input), input);
}

// ── remove_section: second instruction removable ────────────

#[test]
fn remove_section_second_instruction_removable() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("instruction");
    let extra_id = dialog
        .enabled_sections
        .iter()
        .find(|id| id.starts_with("instruction") && *id != "instruction_1")
        .cloned()
        .unwrap();
    let len_before = dialog.enabled_sections.len();
    dialog.remove_section(&extra_id);
    assert_eq!(dialog.enabled_sections.len(), len_before - 1);
}

// ── set_tab no-op when already active ───────────────────────

#[test]
fn set_tab_same_tab_is_noop() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_tab(PromptTab::Normal);
    assert_eq!(dialog.active_tab, PromptTab::Normal);
    assert_eq!(dialog.focused_section, 1);
}

#[test]
fn set_tab_raw_to_normal_focuses_first_section() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_tab(PromptTab::Raw);
    assert_eq!(dialog.active_tab, PromptTab::Raw);
    dialog.set_tab(PromptTab::Normal);
    assert_eq!(dialog.active_tab, PromptTab::Normal);
    assert_eq!(dialog.focused_section, 1);
}

// ── send_edit_type_digit: restart on full field ─────────────

#[test]
fn send_edit_type_digit_restarts_full_field() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_begin_edit();
    dialog.send_edit.as_mut().unwrap().field = 3; // hour (width 2)
    dialog.send_edit_type_digit(1);
    dialog.send_edit_type_digit(4);
    // Now on minute, auto-advanced
    assert_eq!(dialog.send_edit.as_ref().unwrap().field, 4);
    // Type a digit that fills the field
    dialog.send_edit_type_digit(5);
    dialog.send_edit_type_digit(9);
    // Minute full, auto-advanced would go to field 5 (clamped to 4)
    // But if we type again on a full field, it restarts
    dialog.send_edit_type_digit(3);
    let edit = dialog.send_edit.as_ref().unwrap();
    assert_eq!(edit.typed, 3);
    assert_eq!(edit.typed_len, 1);
}

// ── send_edit_adjust: all fields ────────────────────────────

#[test]
fn send_edit_adjust_year() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_begin_edit();
    dialog.send_edit.as_mut().unwrap().field = 0;
    dialog.send_edit_adjust(2);
    let year = dialog.send_edit.unwrap().value.year();
    let now_year = chrono::Local::now().naive_local().year();
    assert_eq!(year, now_year + 2);
}

#[test]
fn send_edit_adjust_month() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_begin_edit();
    dialog.send_edit.as_mut().unwrap().field = 1;
    dialog.send_edit_adjust(1);
    let month = dialog.send_edit.unwrap().value.month();
    let now_month = chrono::Local::now().naive_local().month();
    assert_eq!(month, (now_month % 12) + 1);
}

#[test]
fn send_edit_adjust_hour() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_begin_edit();
    dialog.send_edit.as_mut().unwrap().field = 3;
    dialog.send_edit_adjust(1);
    let hour = dialog.send_edit.unwrap().value.hour();
    let now_hour = chrono::Local::now().naive_local().hour();
    assert_eq!(hour, (now_hour + 1) % 24);
}

#[test]
fn send_edit_adjust_minute() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_begin_edit();
    dialog.send_edit.as_mut().unwrap().field = 4;
    dialog.send_edit_adjust(5);
    let minute = dialog.send_edit.unwrap().value.minute();
    let now_minute = chrono::Local::now().naive_local().minute();
    assert_eq!(minute, (now_minute + 5) % 60);
}

// ── send_edit_adjust_no_edit_is_noop ────────────────────────

#[test]
fn send_edit_adjust_no_edit_is_noop() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_edit_adjust(5);
    assert!(dialog.send_edit.is_none());
}

#[test]
fn send_edit_type_digit_no_edit_is_noop() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_edit_type_digit(5);
    assert!(dialog.send_edit.is_none());
}

// ── visual_positions ────────────────────────────────────────

#[test]
fn visual_positions_empty_string() {
    let positions = SimplePromptDialog::visual_positions("", 10);
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0], (0, 0));
}

#[test]
fn visual_positions_single_char() {
    let positions = SimplePromptDialog::visual_positions("a", 10);
    assert_eq!(positions.len(), 2);
    assert_eq!(positions[0], (0, 0));
    assert_eq!(positions[1], (0, 1));
}

#[test]
fn visual_positions_wrap_at_width() {
    let positions = SimplePromptDialog::visual_positions("abcde", 3);
    assert_eq!(positions.len(), 6);
    // 'a' col 0→1, 'b' col 1→2, 'c' col 2→3, 'd' col 3 wraps to (1,1), 'e' col 1→2
    assert_eq!(positions[4], (1, 1));
    assert_eq!(positions[5], (1, 2));
}

// ── build_xml_block ─────────────────────────────────────────

#[test]
fn build_xml_block_empty_sections() {
    let mut result = String::new();
    build_xml_block(
        &mut result,
        &[],
        |_| true,
        |_| Some("content"),
        "# Header\n",
        "outer",
        "item",
    );
    assert!(result.is_empty());
}

#[test]
fn build_xml_block_no_matching_sections() {
    let mut result = String::new();
    let sections = vec!["goal_1".to_string()];
    build_xml_block(
        &mut result,
        &sections,
        |id| id.starts_with("context"),
        |_| Some("content"),
        "# Header\n",
        "outer",
        "item",
    );
    assert!(result.is_empty());
}

#[test]
fn build_xml_block_matching_empty_content_skipped() {
    let mut result = String::new();
    let sections = vec!["context_1".to_string()];
    build_xml_block(
        &mut result,
        &sections,
        |id| id.starts_with("context"),
        |_| Some("  "),
        "# Header\n",
        "outer",
        "item",
    );
    assert!(result.is_empty());
}

// ── get_file_reference_with_styling ─────────────────────────

#[test]
fn get_file_reference_with_styling_no_refs() {
    let dialog = SimplePromptDialog::new();
    let result = dialog.get_file_reference_with_styling("hello world", Color::Blue);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, "hello world");
    assert!(result[0].1.is_none());
}

#[test]
fn get_file_reference_with_styling_with_ref() {
    let dialog = SimplePromptDialog::new();
    let result = dialog.get_file_reference_with_styling("look @src/lib.rs here", Color::Blue);
    // "look " + "@src/lib.rs" + " here"
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].0, "look ");
    assert!(result[0].1.is_none());
    assert_eq!(result[1].0, "@src/lib.rs");
    assert_eq!(result[1].1, Some(Color::Blue));
    assert_eq!(result[2].0, " here");
    assert!(result[2].1.is_none());
}

#[test]
fn get_file_reference_with_styling_invalid_ref() {
    let dialog = SimplePromptDialog::new();
    let result = dialog.get_file_reference_with_styling("@ invalid", Color::Blue);
    // "@" is a valid ref start but "invalid" is part of " @ invalid"
    // Actually: next_file_reference finds @ at position 0, ref = "@", which has
    // len 1 and starts with @, so is_file_reference returns false.
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].0, "@");
    assert!(result[0].1.is_none());
}

// ── insert_at_completion ────────────────────────────────────

#[test]
fn insert_at_completion_no_picker_is_noop() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "hello @".to_string());
    dialog.at_picker = None;
    dialog.insert_at_completion("instruction_1", "file.rs", "/abs/file.rs", 80);
    // Content unchanged
    assert_eq!(dialog.get_section_content("instruction_1"), "hello @");
}

// ── replace_char_range ──────────────────────────────────────

#[test]
fn replace_char_range_basic() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "hello world".to_string());
    dialog.replace_char_range("instruction_1", 5, 6, "@", 80);
    assert_eq!(dialog.get_section_content("instruction_1"), "hello@world");
}

#[test]
fn replace_char_range_clamps_end() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "abc".to_string());
    dialog.replace_char_range("instruction_1", 1, 100, "X", 80);
    assert_eq!(dialog.get_section_content("instruction_1"), "aX");
}

#[test]
fn replace_char_range_empty_replacement() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "abc".to_string());
    dialog.replace_char_range("instruction_1", 1, 2, "", 80);
    assert_eq!(dialog.get_section_content("instruction_1"), "ac");
}

// ── insert_section: multiple types ──────────────────────────

#[test]
fn insert_section_adds_to_enabled_and_focuses() {
    let mut dialog = SimplePromptDialog::new();
    let id = dialog.insert_section("constraint", "be careful".to_string());
    assert!(id.starts_with("constraint"));
    assert!(dialog.enabled_sections.contains(&id));
    assert_eq!(dialog.get_section_content(&id), "be careful");
    // Focus should be on the new section
    assert_eq!(dialog.focused_section, dialog.enabled_sections.len());
}

// ── resolve_resource_entry: URL ─────────────────────────────

#[test]
fn resolve_resource_entry_url() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("canopy.db");
    let db = Database::new(&db_path).unwrap();
    let result = SimplePromptDialog::resolve_resource_entry(&db, "https://example.com/doc");
    assert!(result.contains("https://example.com/doc"));
    assert!(result.contains("kind: url"));
}

#[test]
fn resolve_resource_entry_raw_text() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("canopy.db");
    let db = Database::new(&db_path).unwrap();
    let result = SimplePromptDialog::resolve_resource_entry(&db, "just some text");
    assert!(result.contains("kind: raw"));
    assert!(result.contains("just some text"));
}

// ── section_content_for_build: no paste returns section ──────

#[test]
fn section_content_for_build_no_paste() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "real".to_string());
    assert_eq!(
        dialog.section_content_for_build("instruction_1"),
        Some("real")
    );
}

#[test]
fn section_content_for_build_missing_section() {
    let dialog = SimplePromptDialog::new();
    assert!(dialog.section_content_for_build("nonexistent").is_none());
}
