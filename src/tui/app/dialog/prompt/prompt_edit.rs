use super::*;

impl SimplePromptDialog {
    pub(super) fn section_entries<'a>(&'a self, prefix: &str) -> Vec<&'a str> {
        self.enabled_sections
            .iter()
            .filter(|section_id| Self::section_matches_prefix(section_id, prefix))
            .filter_map(|section_id| self.section_content_for_build(section_id))
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .collect()
    }

    pub(super) fn section_lines(&self, prefix: &str) -> Vec<String> {
        self.section_entries(prefix)
            .into_iter()
            .flat_map(str::lines)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub(super) fn set_content_and_cursor(
        &mut self,
        section_id: &str,
        content: String,
        cursor: usize,
        field_width: usize,
    ) {
        self.set_section_content(section_id, content);
        self.section_cursors.insert(section_id.to_string(), cursor);
        self.update_section_scroll(section_id, field_width);
    }

    pub(super) fn split_content_at_cursor(&self, section_id: &str) -> (String, String, usize) {
        let content = self.get_section_content(section_id);
        let chars: Vec<char> = content.chars().collect();
        let cursor = self.cursor(section_id).min(chars.len());
        let before = chars[..cursor].iter().collect();
        let after = chars[cursor..].iter().collect();
        (before, after, cursor)
    }

    pub(super) fn replace_char_range(
        &mut self,
        section_id: &str,
        start: usize,
        end: usize,
        replacement: &str,
        field_width: usize,
    ) {
        let content = self.get_section_content(section_id);
        let chars: Vec<char> = content.chars().collect();
        let start = start.min(chars.len());
        let end = end.min(chars.len());

        let mut new_content = String::new();
        new_content.extend(chars[..start].iter().copied());
        new_content.push_str(replacement);
        new_content.extend(chars[end..].iter().copied());
        self.set_content_and_cursor(
            section_id,
            new_content,
            start + replacement.chars().count(),
            field_width,
        );
    }

    pub(super) fn resources_section_id(&self) -> Option<String> {
        self.enabled_sections
            .iter()
            .find(|section_id| Self::section_matches_prefix(section_id, "resources"))
            .cloned()
    }

    pub(super) fn add_resource_reference(&mut self, full_path: &str) {
        let Some(section_id) = self.resources_section_id() else {
            self.add_section_with_content("resources", full_path.to_string());
            return;
        };

        let content = self.get_section_content(&section_id);
        let updated = if content.is_empty() {
            full_path.to_string()
        } else {
            format!("{content}\n{full_path}")
        };
        self.set_section_content(&section_id, updated);
    }

    /// Replace the `@`-trigger with `@rel_path` in the section text and add the full path
    /// to the resources section (creating one if needed).
    /// Skills are treated as normal file resources — no special content injection.
    pub fn insert_at_completion(
        &mut self,
        section_id: &str,
        rel_path: &str,
        full_path: &str,
        field_width: usize,
    ) {
        let Some(trigger_pos) = self.at_picker.as_ref().map(|picker| picker.trigger_pos) else {
            return;
        };

        // The `@` is at trigger_pos; cursor is currently at trigger_pos + 1
        // (we never insert query chars into the text, only into picker.query).
        // A leftover manual `@` can sit right before it (e.g. the user typed
        // `@`, dismissed the picker with Esc, then typed `@` again) — absorb
        // that one too so the result is `@path`, not `@@path`.
        let chars: Vec<char> = self.get_section_content(section_id).chars().collect();
        let start = if trigger_pos > 0 && chars.get(trigger_pos - 1) == Some(&'@') {
            trigger_pos - 1
        } else {
            trigger_pos
        };
        self.replace_char_range(
            section_id,
            start,
            trigger_pos + 1,
            &format!("@{rel_path}"),
            field_width,
        );
        self.add_resource_reference(full_path);
        // NOTE: focused_section is intentionally NOT restored here.
        // The caller (event handler) owns that responsibility and restores it
        // explicitly after this function returns.
    }

    pub(super) fn next_file_reference(
        text: &str,
        current_pos: usize,
    ) -> Option<(usize, &str, usize)> {
        let at_pos = text[current_pos..].find('@')?;
        let absolute_pos = current_pos + at_pos;
        let remaining = &text[absolute_pos..];
        let ref_end = remaining
            .find(|c: char| c.is_whitespace() || c == ',' || c == '!' || c == '?' || c == '│')
            .unwrap_or(remaining.len());
        Some((absolute_pos, &remaining[..ref_end], absolute_pos + ref_end))
    }

    pub(super) fn is_file_reference(file_ref: &str) -> bool {
        file_ref.len() > 1
            && file_ref[1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/')
    }

    /// Colorize `@word` tokens in rendered section text with a custom accent color.
    pub fn get_file_reference_with_styling(
        &self,
        text: &str,
        accent: Color,
    ) -> Vec<(String, Option<Color>)> {
        let mut result = Vec::new();
        let mut current_pos = 0;

        while let Some((absolute_pos, file_ref, next_pos)) =
            Self::next_file_reference(text, current_pos)
        {
            if absolute_pos > current_pos {
                result.push((text[current_pos..absolute_pos].to_string(), None));
            }

            let color = Self::is_file_reference(file_ref).then_some(accent);
            result.push((file_ref.to_string(), color));
            current_pos = next_pos;
        }

        if current_pos < text.len() {
            result.push((text[current_pos..].to_string(), None));
        }
        result
    }

    /// Count visual (wrapped) lines for a text given a field width
    pub fn visual_line_count(text: &str, field_width: usize) -> usize {
        if field_width == 0 {
            return 1;
        }

        // Keep wrapping math aligned with the rendered paragraph, including tabs
        // and hard line breaks, so box height grows when visible text does.
        let mut lines = 1usize;
        let mut col = 0usize;
        for ch in text.chars() {
            match ch {
                '\n' => {
                    lines += 1;
                    col = 0;
                }
                '\t' => {
                    let tab = 4 - (col % 4);
                    if col + tab > field_width {
                        lines += 1;
                        col = tab;
                    } else {
                        col += tab;
                    }
                }
                _ => {
                    if col + 1 > field_width {
                        lines += 1;
                        col = 1;
                    } else {
                        col += 1;
                    }
                }
            }
        }

        lines.max(1)
    }

    /// Visual lines occupied by the first `char_idx` chars of text.
    pub(super) fn visual_lines_to_cursor(text: &str, char_idx: usize, field_width: usize) -> usize {
        let prefix: String = text.chars().take(char_idx).collect();
        Self::visual_line_count(&prefix, field_width).max(1)
    }

    /// Max visible lines for a section type (instruction=5, others=3)
    pub fn max_visible_lines(section_id: &str) -> usize {
        if Self::section_matches_prefix(section_id, "instruction") {
            5
        } else {
            3
        }
    }

    /// Update scroll for a section so the cursor stays visible.
    pub fn update_section_scroll(&mut self, section_id: &str, field_width: usize) {
        let max_vis = Self::max_visible_lines(section_id);
        let text = self.section_content_for_build(section_id).unwrap_or("");
        let cur = self.cursor(section_id);
        let cursor_visual_line =
            Self::visual_lines_to_cursor(text, cur, field_width).saturating_sub(1);

        let scroll = self
            .section_scrolls
            .entry(section_id.to_string())
            .or_insert(0);
        if cursor_visual_line < *scroll {
            *scroll = cursor_visual_line;
        } else if cursor_visual_line >= *scroll + max_vis {
            *scroll = cursor_visual_line + 1 - max_vis;
        }
    }

    /// Move cursor left one char in the given section.
    pub fn move_cursor_left(&mut self, section_id: &str, field_width: usize) {
        let cur = self.cursor(section_id);
        if cur > 0 {
            self.section_cursors.insert(section_id.to_string(), cur - 1);
            self.update_section_scroll(section_id, field_width);
        }
    }

    /// Move cursor right one char in the given section.
    pub fn move_cursor_right(&mut self, section_id: &str, field_width: usize) {
        let len = self
            .sections
            .get(section_id)
            .map(|s| s.chars().count())
            .unwrap_or(0);
        let cur = self.cursor(section_id);
        if cur < len {
            self.section_cursors.insert(section_id.to_string(), cur + 1);
            self.update_section_scroll(section_id, field_width);
        }
    }

    /// (visual line, column) for every cursor position 0..=len, using the same
    /// wrapping math as `visual_line_count` so movement matches what is drawn.
    pub(super) fn visual_positions(text: &str, field_width: usize) -> Vec<(usize, usize)> {
        let field_width = field_width.max(1);
        let mut positions = Vec::with_capacity(text.chars().count() + 1);
        let mut line = 0usize;
        let mut col = 0usize;
        positions.push((line, col));
        for ch in text.chars() {
            match ch {
                '\n' => {
                    line += 1;
                    col = 0;
                }
                '\t' => {
                    let tab = 4 - (col % 4);
                    if col + tab > field_width {
                        line += 1;
                        col = tab;
                    } else {
                        col += tab;
                    }
                }
                _ => {
                    if col + 1 > field_width {
                        line += 1;
                        col = 1;
                    } else {
                        col += 1;
                    }
                }
            }
            positions.push((line, col));
        }
        positions
    }

    /// Move the cursor one visual line vertically, keeping the column as close
    /// as possible. `delta` is -1 (up) or +1 (down).
    pub(super) fn move_cursor_vertical(
        &mut self,
        section_id: &str,
        field_width: usize,
        delta: isize,
    ) {
        let text = self.get_section_content(section_id);
        let positions = Self::visual_positions(&text, field_width);
        let cur = self.cursor(section_id).min(positions.len() - 1);
        let (cur_line, cur_col) = positions[cur];

        let target = if delta < 0 {
            match cur_line.checked_sub(1) {
                Some(line) => line,
                None => {
                    // Already on the first visual line: jump to start.
                    self.section_cursors.insert(section_id.to_string(), 0);
                    self.update_section_scroll(section_id, field_width);
                    return;
                }
            }
        } else {
            let last_line = positions.last().map(|&(line, _)| line).unwrap_or(0);
            if cur_line >= last_line {
                // Already on the last visual line: jump to end.
                self.section_cursors
                    .insert(section_id.to_string(), positions.len() - 1);
                self.update_section_scroll(section_id, field_width);
                return;
            }
            cur_line + 1
        };

        // Best index on the target line: largest column that doesn't pass cur_col,
        // falling back to the line's last position.
        let mut best = None;
        for (idx, &(line, col)) in positions.iter().enumerate() {
            if line != target {
                continue;
            }
            if col <= cur_col || best.is_none() {
                best = Some(idx);
            }
        }
        if let Some(idx) = best {
            self.section_cursors.insert(section_id.to_string(), idx);
            self.update_section_scroll(section_id, field_width);
        }
    }

    /// Move cursor up one visual line in the given section.
    pub fn move_cursor_up(&mut self, section_id: &str, field_width: usize) {
        self.move_cursor_vertical(section_id, field_width, -1);
    }

    /// Move cursor down one visual line in the given section.
    pub fn move_cursor_down(&mut self, section_id: &str, field_width: usize) {
        self.move_cursor_vertical(section_id, field_width, 1);
    }

    /// Insert a character at cursor position in any section.
    /// Content is stored exactly as typed; soft wrapping happens at render time.
    pub fn insert_char_at_cursor(&mut self, section_id: &str, ch: char, field_width: usize) {
        let content = self.get_section_content(section_id);
        let cur = self.cursor(section_id).min(content.chars().count());

        let mut new_chars: Vec<char> = content.chars().collect();
        new_chars.insert(cur, ch);
        let new_content: String = new_chars.into_iter().collect();
        self.set_content_and_cursor(section_id, new_content, cur + 1, field_width);
    }

    /// Delete the character before cursor in any section.
    pub fn backspace_at_cursor(&mut self, section_id: &str, field_width: usize) {
        let content = self.get_section_content(section_id);
        let chars: Vec<char> = content.chars().collect();
        let cur = self.cursor(section_id);
        if cur > 0 && cur <= chars.len() {
            let mut new_chars = chars;
            new_chars.remove(cur - 1);
            let new_content: String = new_chars.into_iter().collect();
            self.set_content_and_cursor(section_id, new_content, cur - 1, field_width);
        }
    }

    /// Insert a newline at cursor position in any section.
    pub fn insert_newline_at_cursor(&mut self, section_id: &str, field_width: usize) {
        let (before, after, cursor) = self.split_content_at_cursor(section_id);
        self.set_content_and_cursor(
            section_id,
            format!("{before}\n{after}"),
            cursor + 1,
            field_width,
        );
    }

    /// Insert text at cursor position in any section.
    pub fn insert_text_at_cursor(&mut self, section_id: &str, text: &str, field_width: usize) {
        let (before, after, cursor) = self.split_content_at_cursor(section_id);
        self.set_content_and_cursor(
            section_id,
            format!("{before}{text}{after}"),
            cursor + text.chars().count(),
            field_width,
        );
    }

    /// Whether the `send_at` field is currently focused (virtual section at index 0).
    #[allow(dead_code)]
    pub fn is_send_at_focused(&self) -> bool {
        self.focused_section == 0 && !self.enabled_sections.is_empty()
    }

    /// Total focusable items: send_at (1) + enabled_sections + the chips row
    /// when attachments are showing.
    pub fn total_focusable(&self) -> usize {
        1 + self.enabled_sections.len() + usize::from(!self.attachments.is_empty())
    }

    /// The chips row's focus stop (`enabled_sections.len() + 1`), or `None`
    /// when there are no attachments to focus.
    pub fn chips_stop(&self) -> Option<usize> {
        (!self.attachments.is_empty()).then(|| self.enabled_sections.len() + 1)
    }

    /// Whether the attachment chips row currently holds focus.
    pub fn is_chips_focused(&self) -> bool {
        self.chips_stop()
            .is_some_and(|stop| self.focused_section == stop)
    }

    /// Move the chip highlight laterally, clamping to the row.
    pub fn chips_move(&mut self, delta: isize) {
        if self.attachments.is_empty() {
            self.attachment_focus = None;
            return;
        }
        let current = self.attachment_focus.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.attachments.len() as isize - 1);
        self.attachment_focus = Some(next as usize);
    }

    /// Remove the focused chip (Backspace/Delete on the chips row). The shelf
    /// row and bytes stay — removal only detaches from this draft. Focus
    /// stays on the row while chips remain, else returns to the last section.
    pub fn remove_focused_chip(&mut self) {
        let Some(index) = self.attachment_focus else {
            return;
        };
        if !remove_attachment(&mut self.attachments, index) {
            return;
        }
        if self.attachments.is_empty() {
            self.attachment_focus = None;
            self.focused_section = self.enabled_sections.len();
        } else {
            self.attachment_focus = Some(index.min(self.attachments.len() - 1));
        }
    }

    /// Park the chip highlight after a focus move lands on (or leaves) the
    /// chips row: a valid chip when focused, untouched otherwise.
    fn sync_chips_focus(&mut self) {
        if self.is_chips_focused() {
            let last = self.attachments.len().saturating_sub(1);
            let current = self.attachment_focus.unwrap_or(0).min(last);
            self.attachment_focus = Some(current);
        }
    }

    /// Focus the first enabled section, ready to type. The send control lives
    /// at focus index 0 and is the LAST stop of the navigation cycle — never
    /// the first — so every open path (fresh, reopen, restored session) starts
    /// here (focus index 1 = first section).
    pub fn focus_first_section(&mut self) {
        self.focused_section = usize::from(!self.enabled_sections.is_empty());
    }

    /// Move focus to the next field in VISUAL order (U11): sections top to
    /// bottom, then the attachment chips row when attachments are showing,
    /// then the send control at the bottom, then wrap to the first section.
    /// The send control keeps focus index 0 internally.
    pub fn focus_next(&mut self) {
        let sections = self.enabled_sections.len();
        let top = self.total_focusable() - 1;
        self.focused_section = match self.focused_section {
            0 if sections > 0 => 1,
            0 => 0,
            i if i >= top => 0,
            i => i + 1,
        };
        self.sync_chips_focus();
    }

    /// Move focus to the previous field in VISUAL order (see
    /// [`Self::focus_next`]): from the send control up to the last section
    /// (or the chips row above it), from the first section wrap down to the
    /// send control.
    pub fn focus_prev(&mut self) {
        let top = self.total_focusable() - 1;
        self.focused_section = match self.focused_section {
            0 => top,
            1 => 0,
            i => i - 1,
        };
        self.sync_chips_focus();
    }

    /// Map the focus index to an `enabled_sections` index.
    /// `None` when send_at (focus 0) is selected.
    pub fn focused_section_index(&self) -> Option<usize> {
        if self.focused_section == 0 {
            None
        } else {
            Some(self.focused_section - 1)
        }
    }

    /// Resolve the currently focused section name, if any (not send_at).
    pub fn focused_section_name(&self) -> Option<&str> {
        self.focused_section_index()
            .and_then(|idx| self.enabled_sections.get(idx))
            .map(String::as_str)
    }

    /// Toggle the send selector between `now` and `date` (lateral arrows,
    /// U11). Leaving `date` discards any picked time and open picker.
    pub fn send_toggle(&mut self) {
        self.send_error = None;
        self.send_choice = match self.send_choice {
            SendChoice::Now => SendChoice::Date,
            SendChoice::Date => {
                self.send_at = None;
                self.send_edit = None;
                SendChoice::Now
            }
        };
    }

    /// Open the inline date-time picker (Enter on `send: date`), preseeded
    /// with the already-picked time or the current local time (U11).
    pub fn send_begin_edit(&mut self) {
        self.send_error = None;
        let seed = self.send_at.unwrap_or_else(|| {
            let now = chrono::Local::now().naive_local();
            now.with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now)
        });
        self.send_edit = Some(SendAtEdit {
            value: seed,
            field: 0,
            typed: 0,
            typed_len: 0,
        });
    }

    /// Move the picker's focused field (0=year … 4=minute). Changing field
    /// clears any half-typed number so the next digit starts fresh.
    pub fn send_edit_move(&mut self, delta: isize) {
        if let Some(edit) = self.send_edit.as_mut() {
            let next = edit.field as isize + delta;
            edit.field = next.clamp(0, 4) as usize;
            edit.typed = 0;
            edit.typed_len = 0;
        }
    }

    /// Adjust the picker's focused field by `delta` with real calendar math
    /// (months/days carry correctly). Arrows and typing coexist: an arrow
    /// clears the digit accumulator so a following digit starts a new number.
    pub fn send_edit_adjust(&mut self, delta: i64) {
        let Some(edit) = self.send_edit.as_mut() else {
            return;
        };
        self.send_error = None;
        edit.typed = 0;
        edit.typed_len = 0;
        let value = edit.value;
        let adjusted = match edit.field {
            0 => add_months(value, delta * 12),
            1 => add_months(value, delta),
            2 => Some(value + chrono::Duration::days(delta)),
            3 => Some(value + chrono::Duration::hours(delta)),
            _ => Some(value + chrono::Duration::minutes(delta)),
        };
        if let Some(adjusted) = adjusted {
            edit.value = adjusted;
        }
    }

    /// Type a digit into the picker's focused field (U11). Digits accumulate
    /// within the field (year 4 wide, others 2); when the field fills it
    /// auto-advances to the next field, and a digit typed into an already-full
    /// field restarts that field. The resulting component is clamped into its
    /// valid range on the fly (e.g. month `13` → `12`), mirroring the clamping
    /// the arrow-based `send_edit_adjust` already performs.
    pub fn send_edit_type_digit(&mut self, digit: u32) {
        self.send_error = None;
        let Some(edit) = self.send_edit.as_mut() else {
            return;
        };
        let field = edit.field;
        let width = field_digit_width(field);
        // A digit landing on an already-full field starts the number over.
        if edit.typed_len >= width {
            edit.typed = 0;
            edit.typed_len = 0;
        }
        edit.typed = edit.typed * 10 + digit;
        edit.typed_len += 1;
        if let Some(updated) = with_field(edit.value, field, edit.typed) {
            edit.value = updated;
        }
        // Field full → auto-advance to the next field (clamped at minute).
        if edit.typed_len >= width {
            edit.field = (field + 1).min(4);
            edit.typed = 0;
            edit.typed_len = 0;
        }
    }

    /// Confirm the picker (Enter): a future time is stored and displayed
    /// inline; a past time is rejected with an inline hint (U11).
    pub fn send_edit_confirm(&mut self) -> bool {
        let Some(edit) = self.send_edit else {
            return false;
        };
        if edit.value <= chrono::Local::now().naive_local() {
            self.send_error = Some("picked time is in the past".to_string());
            return false;
        }
        self.send_at = Some(edit.value);
        self.send_choice = SendChoice::Date;
        self.send_edit = None;
        self.send_error = None;
        true
    }

    /// Cancel the picker (Esc): back to `now` unless a time was already
    /// confirmed earlier.
    pub fn send_edit_cancel(&mut self) {
        self.send_edit = None;
        self.send_error = None;
        if self.send_at.is_none() {
            self.send_choice = SendChoice::Now;
        }
    }

    /// Clear the schedule entirely (Backspace): send immediately.
    pub fn clear_send_at(&mut self) {
        self.send_choice = SendChoice::Now;
        self.send_at = None;
        self.send_edit = None;
        self.send_error = None;
    }

    /// The send control's inline value text (U11). `datetime_format` is
    /// `tui.datetime_format`; the inline date editor's field layout stays
    /// fixed (it drives a fixed-format parse), only this display follows the
    /// setting.
    pub fn send_display(&self, datetime_format: &str) -> String {
        match (self.send_choice, self.send_at) {
            (SendChoice::Now, _) => "now".to_string(),
            (SendChoice::Date, Some(at)) => at.format(datetime_format).to_string(),
            (SendChoice::Date, None) => "date".to_string(),
        }
    }

    pub(super) fn should_collapse_paste(text: &str) -> bool {
        text.lines().count() > 1 || text.chars().count() > 200
    }

    /// Insert pasted text. If it spans multiple lines, collapse it to a
    /// `[Pasted ~N lines]` placeholder while keeping the real text for `build_prompt`.
    pub fn insert_collapsed_paste_at_cursor(
        &mut self,
        section_id: &str,
        text: &str,
        field_width: usize,
    ) {
        if !Self::should_collapse_paste(text) {
            self.insert_text_at_cursor(section_id, text, field_width);
            return;
        }

        self.expand_collapsed_paste(section_id);
        let (before, after, cursor) = self.split_content_at_cursor(section_id);
        let placeholder = format!("[Pasted ~{} lines]", text.lines().count().max(1));
        self.collapsed_pastes
            .insert(section_id.to_string(), format!("{before}{text}{after}"));
        self.set_content_and_cursor(
            section_id,
            format!("{before}{placeholder}{after}"),
            cursor + placeholder.chars().count(),
            field_width,
        );
    }

    /// Expand a collapsed paste for the given section, restoring real content.
    pub fn expand_collapsed_paste(&mut self, section_id: &str) {
        if let Some(real) = self.collapsed_pastes.remove(section_id) {
            self.set_section_content(section_id, real);
        }
    }

    /// Check if section has a collapsed paste.
    pub fn has_collapsed_paste(&self, section_id: &str) -> bool {
        self.collapsed_pastes.contains_key(section_id)
    }

    /// Check if cursor is positioned inside a collapsed placeholder text.
    /// Returns true if the section has a collapsed paste and cursor is within the placeholder.
    pub fn cursor_in_collapsed_placeholder(&self, section_id: &str) -> bool {
        if !self.has_collapsed_paste(section_id) {
            return false;
        }

        let content = self.get_section_content(section_id);
        let cursor_pos = self.cursor(section_id);

        // Find the collapsed placeholder pattern: "[Pasted ~N lines]"
        if let Some(start_byte) = content.find("[Pasted ~") {
            if let Some(end_byte) = content[start_byte..].find(']') {
                let start_char = content[..start_byte].chars().count();
                let end_char = start_char
                    + content[start_byte..start_byte + end_byte + 1]
                        .chars()
                        .count();
                return cursor_pos > start_char && cursor_pos <= end_char;
            }
        }
        false
    }

    /// Delete the entire collapsed paste block and restore cursor position.
    /// Called when backspace is pressed while cursor is inside the placeholder.
    pub fn backspace_collapsed_paste(&mut self, section_id: &str, field_width: usize) {
        if !self.has_collapsed_paste(section_id) {
            return;
        }

        let content = self.get_section_content(section_id);

        // Find and remove the collapsed placeholder
        if let Some(start_byte) = content.find("[Pasted ~") {
            if let Some(end_byte) = content[start_byte..].find(']') {
                let placeholder_end_byte = start_byte + end_byte + 1;
                let start_char = content[..start_byte].chars().count();
                let mut new_content = String::with_capacity(content.len());
                new_content.push_str(&content[..start_byte]);
                new_content.push_str(&content[placeholder_end_byte..]);

                self.collapsed_pastes.remove(section_id);
                self.set_content_and_cursor(section_id, new_content, start_char, field_width);
            }
        }
    }
}
