use super::*;

// ── Layer headers ─────────────────────────────────────────────────

pub(super) fn layer_label(layer: SidebarLayer) -> &'static str {
    match layer {
        SidebarLayer::Live => "Live",
        SidebarLayer::Automation => "Automation",
        SidebarLayer::Knowledge => "Knowledge",
    }
}

pub(super) fn layer_focused(app: &App, layer: SidebarLayer) -> bool {
    matches!(app.focus, Focus::Home | Focus::Preview)
        && !app.playground_active
        && !app.agents_rag_focused
        && app.sidebar_layer == layer
}

pub(super) const SIDEBAR_TABS: [SidebarLayer; 3] = [
    SidebarLayer::Live,
    SidebarLayer::Automation,
    SidebarLayer::Knowledge,
];

/// Centers `label` in `width` columns, truncating it if the cell is too
/// narrow. Tabs carry the label alone: at the real `SIDEBAR_WIDTH` each cell
/// is 11 columns, which fits every full word only once the item count is
/// gone — and a count that forces `"Automa… (12)"` costs more legibility
/// than it buys, since the tab's own body shows the items anyway.
pub(super) fn tab_cell_text(label: &str, width: usize) -> String {
    let text = truncate_str(label, width);
    let slack = width.saturating_sub(text.chars().count());
    let left_pad = slack / 2;
    let right_pad = slack - left_pad;
    format!("{}{text}{}", " ".repeat(left_pad), " ".repeat(right_pad))
}

/// Draws the sidebar's `Live / Automation / Knowledge` tab strip: one cell
/// per tab, each an equal share of `area.width` (the last cell absorbs the
/// rounding remainder so the three always cover the full row exactly — no
/// gap for the background paint underneath to show through). The active
/// tab's label renders in `theme.header_color` and bold, over the default
/// background; inactive cells are dimmed text with no modifier. Registers
/// each cell's hit box in `sidebar_tab_click_map` for mouse clicks — the
/// hit box always spans the full cell width, regardless of the label's
/// centring.
pub(super) fn draw_sidebar_tab_bar(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let col_w = area.width / SIDEBAR_TABS.len() as u16;
    let mut x = area.x;
    for (i, &layer) in SIDEBAR_TABS.iter().enumerate() {
        let width = if i + 1 == SIDEBAR_TABS.len() {
            area.x + area.width - x
        } else {
            col_w
        };
        if width == 0 {
            break;
        }

        let active = app.sidebar_layer == layer;
        let text = tab_cell_text(layer_label(layer), width as usize);
        let (fg, modifier) = if active {
            (theme.header_color, Modifier::BOLD)
        } else {
            (theme.dim_text, Modifier::empty())
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                text,
                Style::default().fg(fg).add_modifier(modifier),
            )))
            .style(Style::default().bg(Color::Reset)),
            Rect::new(x, area.y, width, 1),
        );
        app.sidebar_tab_click_map
            .push((layer, area.y, x, x + width));
        x += width;
    }
}

// ── Layer bodies ─────────────────────────────────────────────────

pub(super) fn agent_indices_by_kind(app: &App) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
    app.agents.iter().enumerate().fold(
        (Vec::new(), Vec::new(), Vec::new()),
        |mut indices, (index, agent)| {
            match agent {
                AgentEntry::Interactive(_) | AgentEntry::Orphaned(_) => indices.1.push(index),
                AgentEntry::Terminal(_) => indices.2.push(index),
                AgentEntry::Group(_) => {}
                _ => indices.0.push(index),
            }
            indices
        },
    )
}

/// Split `budget` rows across sections with the given `demands` using max-min
/// fair capping. No section is allocated more than it needs; the surplus freed
/// by sections that fit within an equal share is shared among the sections that
/// still want more, proportional to their unsatisfied demand. Sections that end
/// up below their demand scroll internally. Callers use this only when the
/// sections can't all be shown at full height (`sum(demands) > budget`), but the
/// function is correct for any input and always allocates at most `budget` rows.
pub(super) fn fair_section_heights(
    demands: &[u16],
    budget: u16,
    focused: Option<usize>,
    floor: u16,
) -> Vec<u16> {
    let n = demands.len();
    let mut alloc = vec![0u16; n];
    let mut capped = vec![false; n];
    let mut remaining_budget = budget;

    // Guarantee the focused section a floor before fair distribution, if it
    // has any demand at all. This is the fix for "I can navigate them but
    // I can't see them" on short screens: the section with the cursor must
    // always have rows to draw into.
    if let Some(idx) = focused {
        if idx < n && demands[idx] > 0 && floor > 0 && budget > 0 {
            let guaranteed = floor.min(demands[idx]).min(budget);
            alloc[idx] = guaranteed;
            remaining_budget = budget - guaranteed;
            // If the focused section's demand was fully satisfied by the floor,
            // it is done; otherwise the fair pass will allocate any additional
            // share on top of the floor.
            if alloc[idx] >= demands[idx] {
                capped[idx] = true;
            }
        }
    }

    loop {
        let active: Vec<usize> = (0..n).filter(|&i| !capped[i]).collect();
        if active.is_empty() || remaining_budget == 0 {
            break;
        }

        // Cap every section whose full remaining demand fits within an equal
        // share of the budget; the rows they don't take are freed for others.
        let share = remaining_budget / active.len() as u16;
        let mut capped_any = false;
        for &i in &active {
            let want = demands[i] - alloc[i];
            if want <= share {
                alloc[i] = demands[i];
                remaining_budget -= want;
                capped[i] = true;
                capped_any = true;
            }
        }
        if capped_any {
            continue;
        }

        // Nobody fully fits: hand out what's left proportional to each still-
        // hungry section's unsatisfied demand, then place the rounding remainder
        // on the hungriest sections one row at a time.
        let total_want: u32 = active.iter().map(|&i| (demands[i] - alloc[i]) as u32).sum();
        if total_want == 0 {
            break;
        }
        let budget_u32 = remaining_budget as u32;
        let mut distributed = 0u16;
        for &i in &active {
            let want = (demands[i] - alloc[i]) as u32;
            let give = (budget_u32 * want / total_want) as u16;
            alloc[i] += give;
            distributed += give;
        }
        let mut leftover = remaining_budget - distributed;
        while leftover > 0 {
            let Some(&i) = active
                .iter()
                .filter(|&&i| alloc[i] < demands[i])
                .max_by_key(|&&i| demands[i] - alloc[i])
            else {
                break;
            };
            alloc[i] += 1;
            leftover -= 1;
        }
        break;
    }

    alloc
}

/// Rows needed for a card-style sub-list (agent cards, graph cards): 0 when
/// empty, else `count*4+2` (3-row cards + 1-row gap + 2-row border).
pub(super) fn card_list_demand(count: usize) -> u16 {
    if count == 0 {
        0
    } else {
        count as u16 * 4 + 2
    }
}

/// Rows needed for the compact groups list: 0 when empty, else
/// `count*2+2` (1-row entries + 1-row gap + 2-row border, see `draw_groups_list`).
pub(super) fn groups_list_demand(count: usize) -> u16 {
    if count == 0 {
        0
    } else {
        count as u16 * 2 + 2
    }
}

/// Draws the sidebar's tab strip plus the active tab's body, which fills all
/// remaining height — exactly one tab is visible at a time, so there's no
/// more space-sharing between layers (`fair_section_heights` is still used
/// *within* a tab's own sub-sections, e.g. Live's interactive/terminal/
/// groups panels). Returns the rows the active tab did not claim, which the
/// project graph and Brian's Brain share. `fair_section_heights` caps each
/// sub-section at its own demand, so a tab with few agents genuinely leaves
/// rows over; handing back a hardcoded empty rect here is what made the brain
/// unreachable no matter how that leftover was later divided.
pub(super) fn draw_sidebar_tabs(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: &Theme,
) -> Rect {
    let (background_indices, interactive_indices, terminal_indices) = agent_indices_by_kind(app);

    let mut remaining = area;
    if let Some(bar) = take_top(&mut remaining, 1) {
        draw_sidebar_tab_bar(frame, bar, app, theme);
    }
    draw_sidebar_gap(frame, &mut remaining, theme);

    match app.sidebar_layer {
        SidebarLayer::Live => draw_live_body(
            frame,
            remaining,
            app,
            &interactive_indices,
            &terminal_indices,
            theme,
        ),
        SidebarLayer::Automation => {
            draw_automation_body(frame, remaining, app, &background_indices, theme)
        }
        SidebarLayer::Knowledge => draw_knowledge_body(frame, remaining, app, theme),
    }
}
