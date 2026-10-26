//! Part 01 of `tests` (split from `src/tui/app/dialog/prompt.rs`, pure move).
use super::*;

#[test]
fn preset_is_discoverable_via_the_add_section_list_like_tools() {
    let dialog = SimplePromptDialog::new();
    let names: Vec<&str> = dialog
        .get_addable_sections()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(names.contains(&"preset"));
    assert!(names.contains(&"tools"));
}

#[test]
fn collect_presets_for_picker_lists_md_files_sorted_with_preview() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("reviewer.md"),
        "You are the reviewer.\n\nMore text.",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("implementer.md"),
        "You are the implementer.",
    )
    .unwrap();
    std::fs::write(dir.path().join("notes.txt"), "ignored, not .md").unwrap();

    let entries = SimplePromptDialog::collect_presets_for_picker(dir.path());

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, "implementer");
    assert_eq!(entries[0].1, "You are the implementer.");
    assert_eq!(entries[0].2, "You are the implementer.");
    assert_eq!(entries[1].0, "reviewer");
    assert_eq!(entries[1].1, "You are the reviewer.");
}

#[test]
fn collect_presets_for_picker_returns_empty_for_missing_directory() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");

    assert!(SimplePromptDialog::collect_presets_for_picker(&missing).is_empty());
}

#[test]
fn filtered_preset_indices_matches_case_insensitive_substring() {
    let entries = vec![
        (
            "implementer".to_string(),
            "preview".to_string(),
            "content".to_string(),
        ),
        (
            "reviewer".to_string(),
            "preview".to_string(),
            "content".to_string(),
        ),
        (
            "resilience".to_string(),
            "preview".to_string(),
            "content".to_string(),
        ),
    ];

    let filtered = SimplePromptDialog::filtered_preset_indices(&entries, "RE");
    let names: Vec<&str> = filtered.iter().map(|&i| entries[i].0.as_str()).collect();
    assert_eq!(names, vec!["reviewer", "resilience"]);

    assert_eq!(
        SimplePromptDialog::filtered_preset_indices(&entries, "").len(),
        3
    );
    assert!(SimplePromptDialog::filtered_preset_indices(&entries, "zzz").is_empty());
}

#[test]
fn filtered_project_indices_matches_name_or_path_case_insensitive() {
    let entries = vec![
        project_entry("harness-canopy", "/home/user/harness-canopy"),
        project_entry("otherproj", "/home/user/OTHERPROJ"),
        project_entry("unrelated", "/srv/unrelated"),
    ];

    let by_name = SimplePromptDialog::filtered_project_indices(&entries, "HARNESS");
    assert_eq!(by_name, vec![0]);

    let by_path = SimplePromptDialog::filtered_project_indices(&entries, "otherproj");
    assert_eq!(by_path, vec![1]);

    assert_eq!(
        SimplePromptDialog::filtered_project_indices(&entries, "").len(),
        3
    );
    assert!(SimplePromptDialog::filtered_project_indices(&entries, "zzz").is_empty());
}

#[test]
fn project_picker_visible_rows_caps_at_thirteen() {
    assert_eq!(SimplePromptDialog::project_picker_visible_rows(1), 3);
    assert_eq!(SimplePromptDialog::project_picker_visible_rows(11), 13);
    assert_eq!(SimplePromptDialog::project_picker_visible_rows(100), 13);
}

#[test]
fn skills_picker_visible_rows_caps_at_thirteen() {
    assert_eq!(SimplePromptDialog::skills_picker_visible_rows(1), 3);
    assert_eq!(SimplePromptDialog::skills_picker_visible_rows(11), 13);
    assert_eq!(SimplePromptDialog::skills_picker_visible_rows(100), 13);
}

#[test]
fn remove_section_visible_rows_caps_at_twelve() {
    assert_eq!(SimplePromptDialog::remove_section_visible_rows(1), 2);
    assert_eq!(SimplePromptDialog::remove_section_visible_rows(11), 12);
    assert_eq!(SimplePromptDialog::remove_section_visible_rows(100), 12);
}

#[test]
fn build_prompt_resolves_project_context_and_file_resources() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("canopy.db");
    let db = Database::new(&db_path).unwrap();

    let project_dir = temp.path().join("sample-project");
    std::fs::create_dir(&project_dir).unwrap();
    let project = db.register_project_path(&project_dir).unwrap();

    let resource = project_dir.join("guide.txt");
    std::fs::write(&resource, "hello from resource").unwrap();

    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction", "do the thing".to_string());
    dialog.add_section_with_content("project_context", project.path.clone());
    dialog.add_section_with_content("resources", resource.display().to_string());

    let prompt = dialog
        .build_prompt_with_resolved_resources(&db, &project_dir)
        .unwrap();

    assert!(prompt.contains("# [PROJECT CONTEXT]: Registered Project Metadata"));
    assert!(prompt.contains(&project.hash));
    assert!(prompt.contains("kind: file"));
    assert!(prompt.contains("guide.txt"));
}

#[test]
fn build_prompt_resolves_project_directory_in_resources() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("canopy.db");
    let db = Database::new(&db_path).unwrap();

    let project_dir = temp.path().join("dir-project");
    std::fs::create_dir(&project_dir).unwrap();
    let project = db.register_project_path(&project_dir).unwrap();

    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction", "summarize".to_string());
    dialog.add_section_with_content("resources", project.path.clone());

    let prompt = dialog
        .build_prompt_with_resolved_resources(&db, &project_dir)
        .unwrap();

    assert!(prompt.contains("workdir_hash:"));
    assert!(prompt.contains(&project.hash));
}

#[test]
fn build_prompt_auto_injects_project_context_for_resource_descendant() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("canopy.db");
    let db = Database::new(&db_path).unwrap();

    let project_dir = temp.path().join("linked-project");
    std::fs::create_dir(&project_dir).unwrap();
    let project = db.register_project_path(&project_dir).unwrap();

    let nested_dir = project_dir.join("src").join("module");
    std::fs::create_dir_all(&nested_dir).unwrap();
    let file_path = nested_dir.join("lib.rs");
    std::fs::write(&file_path, "pub fn demo() {}").unwrap();

    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "summarize".to_string());
    dialog.add_section_with_content("resources", file_path.display().to_string());

    let prompt = dialog
        .build_prompt_with_resolved_resources(&db, temp.path())
        .unwrap();

    assert!(prompt.contains("# [PROJECT CONTEXT]: Registered Project Metadata"));
    assert!(prompt.contains(&project.hash));
    assert!(prompt.contains("kind: file"));
    assert!(prompt.contains("lib.rs"));
}

#[test]
fn system_block_leads_the_prompt() {
    let temp = tempdir().unwrap();
    let db_path = temp.path().join("canopy.db");
    let db = Database::new(&db_path).unwrap();
    let project_dir = temp.path().join("proj");
    std::fs::create_dir(&project_dir).unwrap();

    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "do it".to_string());
    dialog.system_content = Some("[CANOPY] call get_tools".to_string());

    let prompt = dialog
        .build_prompt_with_resolved_resources(&db, &project_dir)
        .unwrap();

    assert!(prompt.starts_with("<system>\n"));
    let system_end = prompt.find("</system>").expect("system block present");
    let instructions = prompt
        .find("# [INSTRUCTIONS]")
        .expect("instructions present");
    // The whole system block must come before the task instructions.
    assert!(system_end < instructions);
}

#[test]
fn migrate_legacy_sections_replaces_memory_with_project_context() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section_with_content("memory_context", "legacy".to_string());
    dialog.add_section_with_content("examples", "old".to_string());

    dialog.migrate_legacy_sections(Some("/tmp/project"));

    assert!(dialog
        .enabled_sections
        .iter()
        .all(|section_id| !section_id.starts_with("memory_context_")
            && !section_id.starts_with("examples_")));
    assert!(dialog
        .enabled_sections
        .iter()
        .any(|section_id| section_id.starts_with("project_context_")));
}

#[test]
fn move_cursor_vertical_respects_hard_newlines() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "abc\ndefgh\nij".to_string());
    // Cursor on "defgh" line, column 4 (after 'g': indices a=0..c=2,\n=3,d=4..h=8)
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 8);

    dialog.move_cursor_up("instruction_1", 40);
    // First line only has 3 columns; cursor clamps to its end (index 3, col 3).
    assert_eq!(dialog.cursor("instruction_1"), 3);

    dialog.move_cursor_down("instruction_1", 40);
    // Back down to "defgh" at column 3 → index 7.
    assert_eq!(dialog.cursor("instruction_1"), 7);

    dialog.move_cursor_down("instruction_1", 40);
    // "ij" line, column 2 max → index 12 (end of text).
    assert_eq!(dialog.cursor("instruction_1"), 12);

    // Down on the last line jumps to end; up from the first line jumps to 0.
    dialog.move_cursor_down("instruction_1", 40);
    assert_eq!(dialog.cursor("instruction_1"), 12);
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 1);
    dialog.move_cursor_up("instruction_1", 40);
    assert_eq!(dialog.cursor("instruction_1"), 0);
}

#[test]
fn move_cursor_vertical_handles_soft_wrap() {
    let mut dialog = SimplePromptDialog::new();
    // width 5: "aaaaa" | "bbbbb" as two visual lines, no '\n' present
    dialog.set_section_content("instruction_1", "aaaaabbbbb".to_string());
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 8);

    dialog.move_cursor_up("instruction_1", 5);
    // Same column (3) on the first visual line → index 3.
    assert_eq!(dialog.cursor("instruction_1"), 3);

    dialog.move_cursor_down("instruction_1", 5);
    assert_eq!(dialog.cursor("instruction_1"), 8);
}

#[test]
fn test_non_ascii_handling_does_not_panic() {
    // Typing past the field width must never mutate the stored content:
    // soft wrapping is a render-time concern only.
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "áéíóú ".to_string());
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 6);
    for ch in "world".chars() {
        dialog.insert_char_at_cursor("instruction_1", ch, 10);
    }
    assert_eq!(dialog.get_section_content("instruction_1"), "áéíóú world");
    assert_eq!(dialog.cursor("instruction_1"), 11);
}

#[test]
fn test_collapsed_placeholder_non_ascii_indices() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "áéíóú [Pasted ~5 lines] extra".to_string());
    dialog.collapsed_pastes.insert(
        "instruction_1".to_string(),
        "original content\nwith multiple lines".to_string(),
    );

    // Character indices:
    // "áéíóú " is 6 characters.
    // "[Pasted ~5 lines]" is 18 characters.
    // "start" char of placeholder is 6.
    // "end" char of placeholder is 6 + 18 = 24.

    // Let's test cursor_in_collapsed_placeholder
    // Cursor at 5 (on the space) -> false
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 5);
    assert!(!dialog.cursor_in_collapsed_placeholder("instruction_1"));

    // Cursor at 7 (inside placeholder) -> true
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 7);
    assert!(dialog.cursor_in_collapsed_placeholder("instruction_1"));

    // Cursor at 23 (on ']') -> true
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 23);
    assert!(dialog.cursor_in_collapsed_placeholder("instruction_1"));

    // Cursor at 24 (after placeholder) -> false
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 24);
    assert!(!dialog.cursor_in_collapsed_placeholder("instruction_1"));

    // Test backspace_collapsed_paste with cursor inside placeholder
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 15);
    dialog.backspace_collapsed_paste("instruction_1", 80);
    // It should delete the placeholder "[Pasted ~5 lines]" and leave "áéíóú  extra"
    assert_eq!(dialog.get_section_content("instruction_1"), "áéíóú  extra");
    // Cursor should be at 6 (the start of deleted placeholder)
    assert_eq!(dialog.cursor("instruction_1"), 6);
}

#[test]
fn insert_collapsed_paste_at_cursor_collapses_multiline_paste() {
    let mut dialog = SimplePromptDialog::new();
    let pasted = "line one\nline two\nline three";

    dialog.insert_collapsed_paste_at_cursor("instruction_1", pasted, 80);

    let displayed = dialog.get_section_content("instruction_1");
    assert!(displayed.contains("[Pasted ~3 lines]"));
    assert!(!displayed.contains("line one"));
    // The full pasted text (all 3 lines) is still what gets sent to the CLI.
    assert_eq!(
        dialog.section_content_for_build("instruction_1"),
        Some(pasted)
    );
}

#[test]
fn insert_at_completion_removes_leftover_manual_at_before_trigger() {
    let temp = tempdir().unwrap();
    let workdir = temp.path().to_path_buf();

    let mut dialog = SimplePromptDialog::new();
    // Simulate: user typed "@" (left over from a dismissed picker), then
    // typed "@" again right after it — trigger_pos points at the second "@".
    dialog.set_section_content("instruction_1", "look @@".to_string());
    dialog
        .section_cursors
        .insert("instruction_1".to_string(), 7);
    dialog.at_picker = Some(AtPicker::new(workdir, 6));

    dialog.insert_at_completion("instruction_1", "src/lib.rs", "/abs/src/lib.rs", 80);

    assert_eq!(
        dialog.get_section_content("instruction_1"),
        "look @src/lib.rs"
    );
}

#[test]
fn is_empty_true_for_fresh_dialog_and_false_once_filled() {
    let mut dialog = SimplePromptDialog::new();
    assert!(dialog.is_empty());

    dialog.set_section_content("instruction_1", "do the thing".to_string());
    assert!(!dialog.is_empty());
}

#[test]
fn is_empty_true_when_sections_are_only_whitespace() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "   \n  ".to_string());
    dialog.add_section_with_content("context", "  ".to_string());
    assert!(dialog.is_empty());
}

#[test]
fn load_flat_text_replaces_all_content_with_a_single_instruction() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "stale draft".to_string());
    dialog.add_section_with_content("context", "stale context".to_string());
    dialog.lock_section("context_1");

    dialog.load_flat_text("recovered prompt text");

    assert_eq!(dialog.enabled_sections, vec!["instruction_1".to_string()]);
    assert_eq!(
        dialog.get_section_content("instruction_1"),
        "recovered prompt text"
    );
    assert!(dialog.locked_sections.is_empty());
    assert_eq!(
        dialog.cursor("instruction_1"),
        "recovered prompt text".chars().count()
    );
}

#[test]
fn persisted_builder_state_round_trips_through_json() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "ship the feature".to_string());
    dialog.add_section_with_content("tools", "skill:code-engineering".to_string());
    dialog.send_choice = SendChoice::Date;
    dialog.send_at = Some(
        chrono::NaiveDate::from_ymd_opt(2026, 7, 20)
            .unwrap()
            .and_hms_opt(14, 30, 0)
            .unwrap(),
    );

    let snapshot = PersistedBuilderState::from_dialog(&dialog);
    let json = serde_json::to_string(&snapshot).expect("serialize");
    let restored: PersistedBuilderState = serde_json::from_str(&json).expect("deserialize");

    let mut target = SimplePromptDialog::new();
    restored.restore_into(&mut target);

    assert_eq!(
        target.get_section_content("instruction_1"),
        "ship the feature"
    );
    assert_eq!(
        target.get_section_content("tools_1"),
        "skill:code-engineering"
    );
    // send_at is intentionally not part of the snapshot — recall must not
    // resurrect a stale schedule.
    assert!(target.send_at.is_none());
}

#[test]
fn for_instruction_prompt_restores_into_a_normal_tab_dialog() {
    let prompt = "Graph nightly failed: build broke";
    let state = PersistedBuilderState::for_instruction_prompt(prompt);
    let json = serde_json::to_string(&state).expect("serialize");
    let restored: PersistedBuilderState = serde_json::from_str(&json).expect("deserialize");

    let mut target = SimplePromptDialog::new();
    restored.restore_into(&mut target);

    assert_eq!(target.get_section_content("instruction_1"), prompt);
    assert_eq!(target.enabled_sections, vec!["instruction_1".to_string()]);
    // Normal-tab structure: no schedule re-armed, empty collapse/lock maps.
    assert!(target.send_at.is_none());
    assert!(target.collapsed_pastes.is_empty());
    assert!(target.locked_sections.is_empty());
    assert_eq!(target.active_tab, PromptTab::Normal);
}

#[test]
fn add_section_focuses_the_new_section_with_cursor_at_start() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("goal");

    // Focus must land IN the newly created section, ready to type.
    let focused = dialog
        .focused_section_name()
        .expect("a section is focused, not the send control")
        .to_string();
    assert!(
        focused.starts_with("goal"),
        "expected focus on the new goal section, got {focused}"
    );
    // Cursor sits at position 0 of the empty new section.
    assert_eq!(dialog.cursor(&focused), 0);

    // A second added section also grabs focus (not the previous one).
    dialog.add_section("constraints");
    let focused = dialog.focused_section_name().unwrap().to_string();
    assert!(focused.starts_with("constraints"));
    assert_eq!(dialog.cursor(&focused), 0);
}

#[test]
fn new_dialog_focuses_the_first_section() {
    let dialog = SimplePromptDialog::new();
    assert_eq!(dialog.focused_section, 1);
    assert_eq!(dialog.focused_section_name(), Some("instruction_1"));
}

#[test]
fn restored_session_focuses_first_section_regardless_of_persisted_focus() {
    let mut source = SimplePromptDialog::new();
    source.add_section("goal");
    // Pretend the user left focus parked on the send control (index 0).
    source.focused_section = 0;
    let session = PromptBuilderSession::from_dialog(&source);

    let mut target = SimplePromptDialog::new();
    session.restore_into(&mut target);
    assert_eq!(target.focused_section, 1);
    assert!(target.focused_section_name().is_some());
}

#[test]
fn recalled_persisted_state_focuses_first_section_regardless_of_persisted_focus() {
    let mut source = SimplePromptDialog::new();
    source.focused_section = 0; // persisted on the send control
    let snapshot = PersistedBuilderState::from_dialog(&source);

    let mut target = SimplePromptDialog::new();
    target.focused_section = 0;
    snapshot.restore_into(&mut target);
    assert_eq!(target.focused_section, 1);
    assert_eq!(target.focused_section_name(), Some("instruction_1"));
}

// ── Tabs & Raw mode ─────────────────────────────────────────────────

#[test]
fn toggle_tab_flips_active_tab_and_focus() {
    let mut dialog = SimplePromptDialog::new();
    assert_eq!(dialog.active_tab, PromptTab::Normal);
    assert_eq!(dialog.focused_section, 1);

    dialog.toggle_tab();
    assert_eq!(dialog.active_tab, PromptTab::Raw);
    // Raw lands focus on the raw buffer (index 1), not the send control.
    assert_eq!(dialog.focused_section, 1);

    dialog.toggle_tab();
    assert_eq!(dialog.active_tab, PromptTab::Normal);
    assert_eq!(dialog.focused_section, 1);
}

#[test]
fn tab_at_maps_clicks_to_the_right_tab() {
    // Tab bar origin at (2, 1). " Normal " spans cols 2..9, " Raw " 10..14.
    let x = 2;
    let y = 1;
    assert_eq!(
        SimplePromptDialog::tab_at(x, y, 3, 1),
        Some(PromptTab::Normal)
    );
    assert_eq!(
        SimplePromptDialog::tab_at(x, y, 9, 1),
        Some(PromptTab::Normal)
    );
    assert_eq!(
        SimplePromptDialog::tab_at(x, y, 10, 1),
        Some(PromptTab::Raw)
    );
    assert_eq!(
        SimplePromptDialog::tab_at(x, y, 13, 1),
        Some(PromptTab::Raw)
    );
    // Left of the bar, right of the bar, and a different row all miss.
    assert_eq!(SimplePromptDialog::tab_at(x, y, 1, 1), None);
    assert_eq!(SimplePromptDialog::tab_at(x, y, 20, 1), None);
    assert_eq!(SimplePromptDialog::tab_at(x, y, 3, 2), None);
}

#[test]
fn raw_non_empty_send_produces_exactly_the_raw_text() {
    let temp = tempdir().unwrap();
    let db = Database::new(&temp.path().join("canopy.db")).unwrap();
    let workdir = temp.path().to_path_buf();

    let mut dialog = SimplePromptDialog::new();
    // Non-raw content that WOULD be composed if we were on the Normal tab —
    // proves the raw path bypasses XML/system composition entirely.
    dialog.set_section_content("instruction_1", "compose me".to_string());
    dialog.system_content = Some("SYSTEM PROTOCOL".to_string());
    dialog.set_tab(PromptTab::Raw);
    dialog
        .sections
        .insert(RAW_SECTION_ID.to_string(), "/compact".to_string());

    let out = dialog.resolve_outgoing_prompt(&db, &workdir).unwrap();
    assert_eq!(out, "/compact");
    assert!(!out.contains("SYSTEM"));
    assert!(!out.contains("[INSTRUCTIONS]"));
}

#[test]
fn raw_empty_preview_and_send_equal_the_composed_prompt() {
    let temp = tempdir().unwrap();
    let db = Database::new(&temp.path().join("canopy.db")).unwrap();
    let workdir = temp.path().to_path_buf();

    let mut dialog = SimplePromptDialog::new();
    dialog.set_section_content("instruction_1", "do the thing".to_string());
    let composed = dialog
        .build_prompt_with_resolved_resources(&db, &workdir)
        .unwrap();

    dialog.set_tab(PromptTab::Raw);
    assert!(dialog.raw_is_empty());

    // Sending from the empty Raw tab sends the composed prompt verbatim.
    let out = dialog.resolve_outgoing_prompt(&db, &workdir).unwrap();
    assert_eq!(out, composed);

    // The cached preview shows exactly that same composed string.
    dialog.refresh_raw_preview(&db, &workdir);
    assert_eq!(dialog.raw_preview.as_deref(), Some(composed.as_str()));
}

// ── Scheduled-sends list panel (B33) ────────────────────────────────

#[test]
fn scheduled_list_view_no_scroll_when_all_rows_fit() {
    // 3 entries, panel shows up to 4 → everything visible, never scrolled.
    assert_eq!(
        SimplePromptDialog::scheduled_list_view(3, 4, Some(2)),
        (3, 0)
    );
    assert_eq!(SimplePromptDialog::scheduled_list_view(3, 4, None), (3, 0));
}

#[test]
fn scheduled_list_view_scrolls_to_keep_selection_in_view() {
    // 6 entries, 3 visible rows. Selecting row 4 scrolls so it is the last
    // visible row (offset 2 → rows 2,3,4).
    assert_eq!(
        SimplePromptDialog::scheduled_list_view(6, 3, Some(4)),
        (3, 2)
    );
    // The last row never scrolls past the end (offset clamps to total-visible).
    assert_eq!(
        SimplePromptDialog::scheduled_list_view(6, 3, Some(5)),
        (3, 3)
    );
    // Early rows keep the panel pinned to the top.
    assert_eq!(
        SimplePromptDialog::scheduled_list_view(6, 3, Some(1)),
        (3, 0)
    );
}

#[test]
fn scheduled_list_view_empty_is_zero() {
    assert_eq!(SimplePromptDialog::scheduled_list_view(0, 4, None), (0, 0));
    assert_eq!(
        SimplePromptDialog::scheduled_list_view(5, 0, Some(1)),
        (0, 0)
    );
}

#[test]
fn load_scheduled_for_edit_loads_raw_and_records_editing_id() {
    let mut dialog = SimplePromptDialog::new();
    // Some pre-existing Normal-form content that must be bypassed.
    dialog.set_section_content("instruction_1", "compose me".to_string());
    dialog.scheduled_list_selected = Some(2);

    let fire = chrono::NaiveDate::from_ymd_opt(2030, 1, 2)
        .unwrap()
        .and_hms_opt(9, 15, 0)
        .unwrap();
    dialog.load_scheduled_for_edit("ss-42", "deliver this later", fire, None);

    assert_eq!(dialog.active_tab, PromptTab::Raw);
    assert_eq!(dialog.raw_text(), "deliver this later");
    assert_eq!(
        dialog.cursor(RAW_SECTION_ID),
        "deliver this later".chars().count()
    );
    assert_eq!(dialog.editing_scheduled_id.as_deref(), Some("ss-42"));
    assert_eq!(dialog.send_choice, SendChoice::Date);
    assert_eq!(dialog.send_at, Some(fire));
    // Focus lands on the raw buffer, and the list focus is released.
    assert_eq!(dialog.focused_section, 1);
    assert!(dialog.scheduled_list_selected.is_none());
}

#[test]
fn load_scheduled_for_edit_with_builder_state_restores_structured_view() {
    let mut source = SimplePromptDialog::new();
    source.set_tab(PromptTab::Normal);
    source.set_section_content("instruction_1", "do the thing".to_string());
    source.set_section_content("context_1", "some context".to_string());

    let snapshot = PersistedBuilderState::from_dialog(&source);
    let json = serde_json::to_string(&snapshot).unwrap();

    let mut dialog = SimplePromptDialog::new();
    let fire = chrono::NaiveDate::from_ymd_opt(2030, 6, 15)
        .unwrap()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    dialog.load_scheduled_for_edit("ss-struct", "ignored when state present", fire, Some(&json));

    assert_eq!(dialog.active_tab, PromptTab::Normal);
    assert_eq!(
        dialog.sections.get("instruction_1").map(|s| s.as_str()),
        Some("do the thing")
    );
    assert_eq!(
        dialog.sections.get("context_1").map(|s| s.as_str()),
        Some("some context")
    );
    assert_eq!(dialog.editing_scheduled_id.as_deref(), Some("ss-struct"));
    assert_eq!(dialog.send_choice, SendChoice::Date);
    assert_eq!(dialog.send_at, Some(fire));
    assert!(dialog.scheduled_list_selected.is_none());
}

#[test]
fn load_scheduled_for_edit_with_none_builder_state_falls_back_to_raw() {
    let mut dialog = SimplePromptDialog::new();
    let fire = chrono::NaiveDate::from_ymd_opt(2030, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    dialog.load_scheduled_for_edit("ss-old", "plain text prompt", fire, None);

    assert_eq!(dialog.active_tab, PromptTab::Raw);
    assert_eq!(dialog.raw_text(), "plain text prompt");
    assert_eq!(dialog.editing_scheduled_id.as_deref(), Some("ss-old"));
}

#[test]
fn load_scheduled_for_edit_with_invalid_json_falls_back_to_raw() {
    let mut dialog = SimplePromptDialog::new();
    let fire = chrono::NaiveDate::from_ymd_opt(2030, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    dialog.load_scheduled_for_edit("ss-bad", "fallback text", fire, Some("not valid json{{{"));

    assert_eq!(dialog.active_tab, PromptTab::Raw);
    assert_eq!(dialog.raw_text(), "fallback text");
    assert_eq!(dialog.editing_scheduled_id.as_deref(), Some("ss-bad"));
}

#[test]
fn raw_buffer_and_active_tab_survive_session_round_trip() {
    let mut source = SimplePromptDialog::new();
    source.set_tab(PromptTab::Raw);
    source.sections.insert(
        RAW_SECTION_ID.to_string(),
        "/compact when quota back".to_string(),
    );

    let session = PromptBuilderSession::from_dialog(&source);
    let mut target = SimplePromptDialog::new();
    session.restore_into(&mut target);

    assert_eq!(target.active_tab, PromptTab::Raw);
    assert_eq!(target.raw_text(), "/compact when quota back");
    // Restoring the Raw tab lands focus on the buffer, not the send control.
    assert_eq!(target.focused_section, 1);
}

// ── Raw-tab mouse-wheel scrolling (U13) ────────────────────────────────

#[test]
fn raw_content_rect_matches_draw_geometry() {
    // A typical inner rect (border excluded). The content area should
    // start 3 rows below inner.y (tab bar + hint + gap) and end 2 rows
    // above inner.bottom (send line + gap), minus the list panel.
    let inner = ratatui::layout::Rect::new(5, 3, 60, 30);
    let rect = SimplePromptDialog::raw_content_rect(inner, 0);
    assert_eq!(rect.x, 6); // inner.x + 1
    assert_eq!(rect.y, 6); // inner.y + 3
    assert_eq!(rect.width, 58); // inner.width - 2
                                // height = inner.height - 2 (send+gap) - 3 (top) = 30 - 5 = 25
    assert_eq!(rect.height, 25);
}

#[test]
fn raw_content_rect_with_list_panel() {
    let inner = ratatui::layout::Rect::new(0, 0, 60, 20);
    let rect = SimplePromptDialog::raw_content_rect(inner, 3);
    // bottom = 20 - (2 + 3) = 15; top = 3; height = 15 - 3 = 12
    assert_eq!(rect.height, 12);
}

#[test]
fn raw_content_rect_small_dialog() {
    // Minimum-size dialog: inner height 6 (borders + tab + hint + gap + content + gap + send).
    let inner = ratatui::layout::Rect::new(0, 0, 40, 6);
    let rect = SimplePromptDialog::raw_content_rect(inner, 0);
    // bottom = 6 - 2 = 4; top = 3; height = max(4-3, 1) = 1
    assert_eq!(rect.height, 1);
    assert!(rect.width > 0);
}

#[test]
fn scroll_raw_edit_clamps_at_zero() {
    let mut dialog = SimplePromptDialog::new();
    assert!(dialog.raw_edit_scroll.is_none());
    dialog.scroll_raw_edit(-5, 100, 20);
    // Clamped to 0 (cannot scroll above the first line).
    assert_eq!(dialog.raw_edit_scroll, Some(0));
}

#[test]
fn scroll_raw_edit_clamps_at_max() {
    let mut dialog = SimplePromptDialog::new();
    // total_lines = 50, avail_h = 10 → max_scroll = 40
    dialog.scroll_raw_edit(100, 50, 10);
    assert_eq!(dialog.raw_edit_scroll, Some(40));
}

#[test]
fn scroll_raw_edit_accumulates() {
    let mut dialog = SimplePromptDialog::new();
    dialog.scroll_raw_edit(3, 100, 20);
    assert_eq!(dialog.raw_edit_scroll, Some(3));
    dialog.scroll_raw_edit(2, 100, 20);
    assert_eq!(dialog.raw_edit_scroll, Some(5));
    dialog.scroll_raw_edit(-1, 100, 20);
    assert_eq!(dialog.raw_edit_scroll, Some(4));
}

#[test]
fn raw_edit_scroll_cleared_on_tab_switch() {
    let mut dialog = SimplePromptDialog::new();
    dialog.set_tab(PromptTab::Raw);
    dialog.raw_edit_scroll = Some(5);
    dialog.set_tab(PromptTab::Normal);
    assert!(dialog.raw_edit_scroll.is_none());
    dialog.set_tab(PromptTab::Raw);
    assert!(dialog.raw_edit_scroll.is_none());
}

#[test]
fn raw_edit_scroll_initially_none() {
    let dialog = SimplePromptDialog::new();
    assert!(dialog.raw_edit_scroll.is_none());
}

// ── field_digit_width ────────────────────────────────────────

#[test]
fn field_digit_width_year_is_4() {
    assert_eq!(field_digit_width(0), 4);
}

#[test]
fn field_digit_width_month_is_2() {
    assert_eq!(field_digit_width(1), 2);
}

#[test]
fn field_digit_width_day_is_2() {
    assert_eq!(field_digit_width(2), 2);
}

#[test]
fn field_digit_width_hour_is_2() {
    assert_eq!(field_digit_width(3), 2);
}

#[test]
fn field_digit_width_minute_is_2() {
    assert_eq!(field_digit_width(4), 2);
}

// ── days_in_month ────────────────────────────────────────────

#[test]
fn days_in_month_january() {
    assert_eq!(days_in_month(2024, 1), 31);
}

#[test]
fn days_in_month_february_leap_year() {
    assert_eq!(days_in_month(2024, 2), 29);
}

#[test]
fn days_in_month_february_non_leap() {
    assert_eq!(days_in_month(2023, 2), 28);
}

#[test]
fn days_in_month_april() {
    assert_eq!(days_in_month(2024, 4), 30);
}

#[test]
fn days_in_month_december() {
    assert_eq!(days_in_month(2024, 12), 31);
}

// ── add_months ───────────────────────────────────────────────

#[test]
fn add_months_forward() {
    let dt = chrono::NaiveDate::from_ymd_opt(2024, 1, 15)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap();
    let result = add_months(dt, 3).unwrap();
    assert_eq!(result.month(), 4);
    assert_eq!(result.year(), 2024);
}

#[test]
fn add_months_backward() {
    let dt = chrono::NaiveDate::from_ymd_opt(2024, 3, 15)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap();
    let result = add_months(dt, -1).unwrap();
    assert_eq!(result.month(), 2);
}

#[test]
fn add_months_year_rollover() {
    let dt = chrono::NaiveDate::from_ymd_opt(2024, 11, 15)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap();
    let result = add_months(dt, 2).unwrap();
    assert_eq!(result.month(), 1);
    assert_eq!(result.year(), 2025);
}

#[test]
fn add_months_day_clamping() {
    // Jan 31 + 1 month = Feb 29 (2024 is leap)
    let dt = chrono::NaiveDate::from_ymd_opt(2024, 1, 31)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    let result = add_months(dt, 1).unwrap();
    assert_eq!(result.month(), 2);
    assert_eq!(result.day(), 29);
}

// ── with_field ───────────────────────────────────────────────

#[test]
fn with_field_year() {
    let dt = chrono::NaiveDate::from_ymd_opt(2024, 6, 15)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap();
    let result = with_field(dt, 0, 2030).unwrap();
    assert_eq!(result.year(), 2030);
    assert_eq!(result.month(), 6);
    assert_eq!(result.day(), 15);
}

#[test]
fn with_field_month_clamps_day() {
    let dt = chrono::NaiveDate::from_ymd_opt(2024, 1, 31)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap();
    let result = with_field(dt, 1, 2).unwrap();
    assert_eq!(result.month(), 2);
    assert_eq!(result.day(), 29); // Clamped to Feb 29 (leap year)
}

#[test]
fn with_field_day_clamped_to_month_length() {
    let dt = chrono::NaiveDate::from_ymd_opt(2024, 2, 15)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap();
    let result = with_field(dt, 2, 31).unwrap();
    assert_eq!(result.day(), 29); // Feb 2024 has 29 days
}

#[test]
fn with_field_hour_clamped() {
    let dt = chrono::NaiveDate::from_ymd_opt(2024, 6, 15)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap();
    let result = with_field(dt, 3, 25).unwrap();
    assert_eq!(result.hour(), 23);
}

#[test]
fn with_field_minute_clamped() {
    let dt = chrono::NaiveDate::from_ymd_opt(2024, 6, 15)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap();
    let result = with_field(dt, 4, 65).unwrap();
    assert_eq!(result.minute(), 59);
}

// ── strip_resources_section ──────────────────────────────────

#[test]
fn strip_resources_section_removes_present_section() {
    let input = "before\n# [RESOURCES]: Knowledge Base & Data\n<resources>\n  <resource>\n    path: /foo\n  </resource>\n</resources>\n\nafter";
    let result = strip_resources_section(input);
    assert!(result.starts_with("before"));
    assert!(result.ends_with("after"));
    assert!(!result.contains("RESOURCES"));
}

#[test]
fn strip_resources_section_no_section_unchanged() {
    let input = "no resources section here";
    assert_eq!(strip_resources_section(input), input);
}

#[test]
fn strip_resources_section_missing_closing_tag() {
    let input = "# [RESOURCES]: Knowledge Base & Data\n<resources>\n  content\n";
    assert_eq!(strip_resources_section(input), input);
}

// ── format_indexed_xml_section ────────────────────────────────

#[test]
fn format_indexed_xml_section_empty_items() {
    assert!(format_indexed_xml_section("header", "outer", "item", &[]).is_none());
}

#[test]
fn format_indexed_xml_section_with_items() {
    let items = vec!["item1".to_string(), "item2".to_string()];
    let result = format_indexed_xml_section("# Header\n", "outer", "item", &items).unwrap();
    assert!(result.contains("# Header"));
    assert!(result.contains("<outer>"));
    assert!(result.contains("</outer>"));
    assert!(result.contains("<item>"));
    assert!(result.contains("item1"));
    assert!(result.contains("item2"));
}

// ── push_xml_item ────────────────────────────────────────────

#[test]
fn push_xml_item_basic() {
    let mut result = String::new();
    push_xml_item(&mut result, "tag", "content here");
    assert!(result.contains("<tag>"));
    assert!(result.contains("content here"));
    assert!(result.contains("</tag>"));
}

#[test]
fn push_xml_item_multiline() {
    let mut result = String::new();
    push_xml_item(&mut result, "tag", "line1\nline2");
    assert!(result.contains("line1"));
    assert!(result.contains("line2"));
}

// ── append_tool_skill ────────────────────────────────────────

#[test]
fn append_tool_skill_basic() {
    let mut result = String::new();
    append_tool_skill(&mut result, "skill:code-engineering");
    assert!(result.contains("<skill>"));
    assert!(result.contains("skill:code-engineering"));
    assert!(result.contains("</skill>"));
}

#[test]
fn append_tool_skill_canopy_formats_with_skill_get() {
    let mut result = String::new();
    append_tool_skill(&mut result, "canopy:canopy-graph-design");
    assert!(result.contains("canopy skill canopy-graph-design"));
    assert!(result.contains("skill_get(name=\"canopy-graph-design\")"));
}

#[test]
fn collect_skills_for_picker_includes_canopy_and_disk() {
    use crate::dynamic_skills::SkillStore;
    use tempfile::tempdir;
    let temp = tempdir().unwrap();
    let workdir = temp.path();
    // Create disk skill
    let disk_skills = workdir.join(".agents").join("skills");
    std::fs::create_dir_all(disk_skills.join("texforge")).unwrap();
    std::fs::write(
        disk_skills.join("texforge").join("SKILL.md"),
        "---\nname: texforge\ndescription: TeX forge\n---\n# texforge\nbody\n",
    )
    .unwrap();

    // Create store with canopy skill installed
    let store_dir = tempdir().unwrap();
    std::fs::create_dir_all(store_dir.path().join("canopy-graph-design")).unwrap();
    std::fs::write(
        store_dir
            .path()
            .join("canopy-graph-design")
            .join("SKILL.md"),
        "---\nname: canopy-graph-design\ndescription: Design\n---\n# canopy-graph-design\nbody\n",
    )
    .unwrap();
    let store = SkillStore::new(store_dir.path().to_path_buf(), vec![], 15);
    let entries = SimplePromptDialog::collect_skills_for_picker(workdir, Some(&store));
    // Find entries
    let has_texforge = entries.iter().any(|(label, name, prefix)| {
        label == "skill:texforge" && name == "texforge" && prefix == "skill"
    });
    let has_canopy = entries.iter().any(|(label, name, prefix)| {
        label == "canopy:canopy-graph-design" && name == "canopy-graph-design" && prefix == "canopy"
    });
    assert!(has_texforge, "should have texforge");
    assert!(has_canopy, "should have canopy skill");
    // Canopy rows after disk? Check positions
    let texforge_idx = entries.iter().position(|(l, _, _)| l == "skill:texforge");
    let canopy_idx = entries
        .iter()
        .position(|(l, _, _)| l == "canopy:canopy-graph-design");
    assert!(texforge_idx.is_some() && canopy_idx.is_some());
    assert!(canopy_idx.unwrap() > texforge_idx.unwrap());
}

#[test]
fn collect_skills_for_picker_no_canopy_when_none() {
    use tempfile::tempdir;
    let temp = tempdir().unwrap();
    let workdir = temp.path();
    let disk_skills = workdir.join(".agents").join("skills");
    std::fs::create_dir_all(disk_skills.join("texforge")).unwrap();
    std::fs::write(
        disk_skills.join("texforge").join("SKILL.md"),
        "---\nname: texforge\ndescription: TeX forge\n---\n# texforge\nbody\n",
    )
    .unwrap();
    let entries = SimplePromptDialog::collect_skills_for_picker(workdir, None);
    assert!(entries.iter().any(|(l, _, _)| l == "skill:texforge"));
    assert!(!entries.iter().any(|(_, _, p)| p == "canopy"));
}

/// FR5: a store that cannot be read (missing dir, unreachable source,
/// no cached catalog) contributes no canopy rows — the disk skills
/// still show up on their own.
#[test]
fn collect_skills_for_picker_unreadable_store_yields_only_disk_entries() {
    use crate::dynamic_skills::{GitSource, SkillStore};
    use tempfile::tempdir;
    let temp = tempdir().unwrap();
    let workdir = temp.path();
    let disk_skills = workdir.join(".agents").join("skills");
    std::fs::create_dir_all(disk_skills.join("texforge")).unwrap();
    std::fs::write(
        disk_skills.join("texforge").join("SKILL.md"),
        "---\nname: texforge\ndescription: TeX forge\n---\n# texforge\nbody\n",
    )
    .unwrap();

    // Store dir does not exist and the source (a path that was never a
    // repo) has no on-disk catalog cache, so nothing canopy-side is
    // listable.
    let missing = temp.path().join("no-such-store");
    let store = SkillStore::new(
        missing,
        vec![GitSource::new(
            temp.path()
                .join("no-such-source")
                .to_string_lossy()
                .to_string(),
            None,
        )],
        15,
    );
    let entries = SimplePromptDialog::collect_skills_for_picker(workdir, Some(&store));
    assert!(
        entries
            .iter()
            .any(|(l, _, p)| l == "skill:texforge" && p == "skill"),
        "disk entries must survive an unreadable store; got: {entries:?}"
    );
    assert!(
        !entries.iter().any(|(_, _, p)| p == "canopy"),
        "unreadable store must yield no canopy rows; got: {entries:?}"
    );
}

/// FR3: a name that exists both on disk and in the canopy store yields
/// one row per prefix, so the user picks which one to reference.
#[test]
fn collect_skills_for_picker_same_name_in_both_prefixes() {
    use crate::dynamic_skills::SkillStore;
    use tempfile::tempdir;
    let temp = tempdir().unwrap();
    let workdir = temp.path();
    let disk_skills = workdir.join(".agents").join("skills");
    std::fs::create_dir_all(disk_skills.join("texforge")).unwrap();
    std::fs::write(
        disk_skills.join("texforge").join("SKILL.md"),
        "---\nname: texforge\ndescription: TeX forge\n---\n# texforge\nbody\n",
    )
    .unwrap();

    let store_dir = tempdir().unwrap();
    std::fs::create_dir_all(store_dir.path().join("texforge")).unwrap();
    std::fs::write(
        store_dir.path().join("texforge").join("SKILL.md"),
        "---\nname: texforge\ndescription: Canopy texforge\n---\n# texforge\nbody\n",
    )
    .unwrap();
    let store = SkillStore::new(store_dir.path().to_path_buf(), vec![], 15);
    let entries = SimplePromptDialog::collect_skills_for_picker(workdir, Some(&store));
    assert!(
        entries
            .iter()
            .any(|(l, _, p)| l == "skill:texforge" && p == "skill"),
        "the disk row must stay; got: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|(l, _, p)| l == "canopy:texforge" && p == "canopy"),
        "the canopy row must appear alongside the disk row; got: {entries:?}"
    );
}

#[test]
fn build_body_renders_canopy_skill_with_skill_get_instruction() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section_with_content("tools", "canopy:canopy-graph-design".to_string());
    let body = dialog.build_body();
    let expected = "canopy skill canopy-graph-design \u{2014} load it with skill_get(name=\"canopy-graph-design\")";
    assert!(
        body.contains(expected),
        "build_body should render the canopy skill as a skill_get fetch instruction; got: {body}"
    );
}

// ── visual_line_count edge cases ─────────────────────────────

#[test]
fn visual_line_count_empty_string() {
    assert_eq!(SimplePromptDialog::visual_line_count("", 40), 1);
}

#[test]
fn visual_line_count_single_line() {
    assert_eq!(SimplePromptDialog::visual_line_count("hello", 40), 1);
}

#[test]
fn visual_line_count_zero_width() {
    assert_eq!(SimplePromptDialog::visual_line_count("hello", 0), 1);
}

#[test]
fn visual_line_count_newlines() {
    assert_eq!(SimplePromptDialog::visual_line_count("a\nb\nc", 40), 3);
}

#[test]
fn visual_line_count_soft_wrap() {
    // 10 chars in a field_width of 5 → 2 visual lines
    assert_eq!(SimplePromptDialog::visual_line_count("abcdefghij", 5), 2);
}

#[test]
fn visual_line_count_tabs() {
    // Tab at col 0 takes 4 cols, 'x' at col 4 within width 5 → 1 line total
    assert_eq!(SimplePromptDialog::visual_line_count("\tx", 5), 1);
}

#[test]
fn visual_line_count_tab_no_wrap() {
    // Tab at col 0 takes 4 cols, then "ab" (2 chars) → col 6, within width 10
    assert_eq!(SimplePromptDialog::visual_line_count("\tab", 10), 1);
}

// ── max_visible_lines ────────────────────────────────────────

#[test]
fn max_visible_lines_instruction_section() {
    assert_eq!(SimplePromptDialog::max_visible_lines("instruction_1"), 5);
}

#[test]
fn max_visible_lines_other_section() {
    assert_eq!(SimplePromptDialog::max_visible_lines("context_1"), 3);
    assert_eq!(SimplePromptDialog::max_visible_lines("goal_1"), 3);
    assert_eq!(SimplePromptDialog::max_visible_lines("tools_1"), 3);
}

// ── section_type ─────────────────────────────────────────────

#[test]
fn section_type_instruction() {
    assert_eq!(
        SimplePromptDialog::section_type("instruction_1"),
        "instruction"
    );
    assert_eq!(
        SimplePromptDialog::section_type("instruction"),
        "instruction"
    );
}

#[test]
fn section_type_context() {
    assert_eq!(SimplePromptDialog::section_type("context_2"), "context");
}

#[test]
fn section_type_unknown() {
    assert_eq!(
        SimplePromptDialog::section_type("custom_thing"),
        "custom_thing"
    );
}

// ── section_matches_prefix ───────────────────────────────────

#[test]
fn section_matches_prefix_exact() {
    assert!(SimplePromptDialog::section_matches_prefix(
        "instruction",
        "instruction"
    ));
}

#[test]
fn section_matches_prefix_with_suffix() {
    assert!(SimplePromptDialog::section_matches_prefix(
        "instruction_1",
        "instruction"
    ));
}

#[test]
fn section_matches_prefix_no_match() {
    assert!(!SimplePromptDialog::section_matches_prefix(
        "context_1",
        "instruction"
    ));
}

// ── is_tools_section ─────────────────────────────────────────

#[test]
fn is_tools_section_exact() {
    assert!(SimplePromptDialog::is_tools_section("tools"));
}

#[test]
fn is_tools_section_with_suffix() {
    assert!(SimplePromptDialog::is_tools_section("tools_1"));
}

#[test]
fn is_tools_section_no_match() {
    assert!(!SimplePromptDialog::is_tools_section("context"));
    assert!(!SimplePromptDialog::is_tools_section("tool"));
}
