use super::*;

impl SimplePromptDialog {
    // ── Tabs & Raw mode ─────────────────────────────────────────────────

    /// The Raw tab's free-text buffer (empty string when never touched).
    pub fn raw_text(&self) -> &str {
        self.sections
            .get(RAW_SECTION_ID)
            .map(String::as_str)
            .unwrap_or("")
    }

    /// True when the Raw buffer is blank (whitespace-only counts as empty).
    /// The empty state is what triggers the composed-prompt preview.
    pub fn raw_is_empty(&self) -> bool {
        self.raw_text().trim().is_empty()
    }

    /// Switch to a specific tab. Focus lands on the tab's first editable field
    /// (Raw → the raw buffer; Normal → the first section), never on the send
    /// control. No-op transient state (`raw_preview`) is cleared so the caller
    /// can recompute it when needed.
    pub fn set_tab(&mut self, tab: PromptTab) {
        if self.active_tab == tab {
            return;
        }
        self.active_tab = tab;
        self.raw_preview = None;
        self.raw_edit_scroll = None;
        match tab {
            PromptTab::Raw => {
                // Raw has two focus targets: the buffer (index 1) and the send
                // control (index 0). Land on the buffer, ready to type.
                self.focused_section = 1;
                let end = self.raw_text().chars().count();
                self.section_cursors
                    .entry(RAW_SECTION_ID.to_string())
                    .or_insert(end);
            }
            PromptTab::Normal => self.focus_first_section(),
        }
    }

    /// Toggle between the Normal and Raw tabs.
    pub fn toggle_tab(&mut self) {
        let next = match self.active_tab {
            PromptTab::Normal => PromptTab::Raw,
            PromptTab::Raw => PromptTab::Normal,
        };
        self.set_tab(next);
    }

    /// The exact string a "send" produces given the active tab:
    /// Raw + non-empty buffer → the raw text verbatim (no XML/system wrapping);
    /// otherwise the composed Normal-form prompt (identical to sending from the
    /// Normal tab, and to what the Raw-empty preview shows).
    pub fn resolve_outgoing_prompt(&self, db: &Database, current_workdir: &Path) -> Result<String> {
        if self.active_tab == PromptTab::Raw && !self.raw_is_empty() {
            return Ok(self.raw_text().to_string());
        }
        self.build_prompt_with_resolved_resources(db, current_workdir)
    }

    /// Compute the composed-prompt preview string (best-effort) and cache it in
    /// `raw_preview`. Called when entering the Raw tab; the Normal form can't
    /// change while Raw is shown, so the cache stays faithful.
    pub fn refresh_raw_preview(&mut self, db: &Database, current_workdir: &Path) {
        self.raw_preview = self
            .build_prompt_with_resolved_resources(db, current_workdir)
            .ok();
        self.raw_preview_scroll = 0;
    }

    /// Scroll the read-only raw preview by `delta` lines (negative = up),
    /// clamped to the preview's line count so it can always be scrolled back.
    pub fn scroll_raw_preview(&mut self, delta: isize) {
        let max = self
            .raw_preview
            .as_deref()
            .map(|preview| preview.lines().count().saturating_sub(1))
            .unwrap_or(0);
        let next = self.raw_preview_scroll.saturating_add_signed(delta);
        self.raw_preview_scroll = next.min(max);
    }

    /// Wheel-scroll the editable raw buffer by `delta` lines (negative = up).
    /// The scroll is clamped to `[0, max]` where `max = total_lines - avail_h`,
    /// and stored separately from the cursor so the cursor is never moved by
    /// a wheel event (view-only, satisfying FR 4).
    pub fn scroll_raw_edit(&mut self, delta: isize, total_lines: usize, avail_h: usize) {
        let max_scroll = total_lines.saturating_sub(avail_h);
        let current = self.raw_edit_scroll.unwrap_or(0);
        let next = current.saturating_add_signed(delta);
        self.raw_edit_scroll = Some(next.min(max_scroll));
    }

    /// Hit-boxes for the two tab labels, laid out left-to-right from `(x, y)`.
    /// Pure geometry so the render and the mouse handler agree, and so the
    /// click→tab mapping is unit-testable without any event plumbing.
    pub fn tab_hitboxes(x: u16, y: u16) -> [(PromptTab, Rect); 2] {
        let normal_w = TAB_NORMAL_LABEL.chars().count() as u16;
        let raw_w = TAB_RAW_LABEL.chars().count() as u16;
        [
            (PromptTab::Normal, Rect::new(x, y, normal_w, 1)),
            (PromptTab::Raw, Rect::new(x + normal_w, y, raw_w, 1)),
        ]
    }

    /// Map a click at `(col, row)` to the tab whose hit-box contains it, given
    /// the tab bar origin `(x, y)`. Pure — the unit test for mouse switching.
    pub fn tab_at(x: u16, y: u16, col: u16, row: u16) -> Option<PromptTab> {
        Self::tab_hitboxes(x, y)
            .into_iter()
            .find_map(|(tab, rect)| {
                (row == rect.y && col >= rect.x && col < rect.x + rect.width).then_some(tab)
            })
    }

    /// Pure geometry for the Raw tab's scrollable content region, mirroring
    /// `draw_raw_tab_content`'s `content_area`. The `inner` rect is the
    /// dialog's inner area (border excluded); `list_panel_height` is the
    /// scheduled-sends panel rows (0 when no pending sends). Used by the
    /// mouse-wheel hit test to decide whether a scroll event targets the
    /// Raw content.
    pub fn raw_content_rect(
        inner: ratatui::layout::Rect,
        list_panel_height: u16,
    ) -> ratatui::layout::Rect {
        let content_top = inner.y + 3;
        let content_bottom = inner.y + inner.height.saturating_sub(2 + list_panel_height);
        let avail_h = content_bottom.saturating_sub(content_top).max(1);
        ratatui::layout::Rect {
            x: inner.x + 1,
            y: content_top,
            width: inner.width.saturating_sub(2),
            height: avail_h,
        }
    }

    // ── Scheduled-sends list panel (B33) ────────────────────────────────

    /// Load a queued scheduled send for in-place editing. If `builder_state`
    /// carries valid JSON, the structured view is restored (Normal tab with
    /// its sections); otherwise falls back to the Raw tab with the flat text.
    /// Records the id so re-confirming a schedule replaces that row.
    pub fn load_scheduled_for_edit(
        &mut self,
        id: &str,
        prompt: &str,
        fire_local: chrono::NaiveDateTime,
        builder_state: Option<&str>,
    ) {
        self.raw_preview = None;
        self.raw_preview_scroll = 0;

        let restored =
            builder_state.and_then(|json| serde_json::from_str::<PersistedBuilderState>(json).ok());

        if let Some(state) = restored {
            state.restore_into(self);
            let has_raw_only =
                state.sections.len() == 1 && state.sections.contains_key(RAW_SECTION_ID);
            if has_raw_only {
                self.active_tab = PromptTab::Raw;
            } else {
                self.active_tab = PromptTab::Normal;
            }
        } else {
            self.active_tab = PromptTab::Raw;
            self.sections
                .insert(RAW_SECTION_ID.to_string(), prompt.to_string());
            let end = prompt.chars().count();
            self.section_cursors.insert(RAW_SECTION_ID.to_string(), end);
        }

        // Raw's first focus target is the buffer (index 1), never the send control.
        self.focused_section = 1;
        self.editing_scheduled_id = Some(id.to_string());
        self.send_choice = SendChoice::Date;
        self.send_at = Some(fire_local);
        self.send_edit = None;
        self.send_error = None;
        // Editing takes over from browsing: release the list focus.
        self.scheduled_list_selected = None;
    }

    /// Pure geometry for the scheduled-sends list panel: given the total number
    /// of pending entries, the panel's max visible rows, and the current
    /// selection, return `(visible_rows, scroll_offset)` so the selected row
    /// stays in view. Kept pure (like `tab_hitboxes`) so render and tests agree.
    pub fn scheduled_list_view(
        total: usize,
        max_rows: usize,
        selected: Option<usize>,
    ) -> (usize, usize) {
        if total == 0 || max_rows == 0 {
            return (0, 0);
        }
        let visible = total.min(max_rows);
        let scroll = match selected {
            Some(sel) => {
                let sel = sel.min(total - 1);
                if sel < visible {
                    0
                } else {
                    (sel + 1 - visible).min(total - visible)
                }
            }
            None => 0,
        };
        (visible, scroll)
    }
}
