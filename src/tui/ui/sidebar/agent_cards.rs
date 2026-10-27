use super::*;
use crate::tui::agent::InteractiveAgent;

pub(super) fn agent_section_border_style(
    app: &App,
    section: AgentSectionFocus,
    theme: &Theme,
) -> Style {
    let focused = if matches!(app.focus, Focus::Home | Focus::Preview) {
        layer_focused(app, SidebarLayer::Live) && app.agent_section_focus == section
    } else if app.focus == Focus::Agent {
        match app.agents.get(app.selected) {
            Some(AgentEntry::Interactive(_) | AgentEntry::Orphaned(_)) => {
                section == AgentSectionFocus::Interactive
            }
            Some(AgentEntry::Terminal(_)) => section == AgentSectionFocus::Terminal,
            Some(AgentEntry::Group(_)) => section == AgentSectionFocus::Groups,
            _ => false,
        }
    } else {
        false
    };

    Style::default().fg(if focused {
        theme.header_color
    } else {
        theme.border_color
    })
}

pub(super) fn render_agent_list_panel(
    frame: &mut Frame,
    area: Option<Rect>,
    title: &str,
    indices: &[usize],
    app: &mut App,
    border_style: Style,
    theme: &Theme,
) {
    let Some(area) = area else {
        return;
    };
    render_titled_panel(
        frame,
        area,
        title,
        Style::default().fg(theme.dim_text),
        border_style,
        theme,
        |frame, inner| draw_agent_list(frame, inner, indices, app, theme),
    );
}

pub(super) fn render_groups_panel(
    frame: &mut Frame,
    area: Option<Rect>,
    app: &mut App,
    section: AgentSectionFocus,
    theme: &Theme,
) {
    let Some(area) = area else {
        return;
    };
    render_titled_panel(
        frame,
        area,
        " groups ",
        Style::default().fg(theme.dim_text),
        agent_section_border_style(app, section, theme),
        theme,
        |frame, inner| draw_groups_list(frame, inner, app, theme),
    );
}

// ── Live/Automation agent cards (shared card renderer) ──────────────

pub(super) fn draw_agent_list(
    frame: &mut Frame,
    area: Rect,
    indices: &[usize],
    app: &mut App,
    theme: &Theme,
) {
    let card_h = 3u16;
    let row_h = 4u16;

    if area.height < card_h || indices.is_empty() {
        return;
    }

    let max_visible = ((area.height.saturating_sub(card_h)) / row_h + 1) as usize;
    let selected_local = indices.iter().position(|&idx| idx == app.selected);
    let scroll = scroll_state_with_offset(
        indices.len(),
        selected_local,
        max_visible,
        app.sidebar_scroll_offset,
    );
    app.sidebar_visible_capacity += scroll.max_visible;
    let mut y = area.y;
    let end = indices.len().min(scroll.start + scroll.max_visible + 1);

    for (rel_i, &idx) in indices[scroll.start..end].iter().enumerate() {
        if y + card_h > area.y + area.height {
            break;
        }

        let card_area = Rect::new(area.x, y, area.width, card_h);
        let selected = idx == app.selected && !app.agents_rag_focused;
        let hovered = app.hovered_row == Some(idx) && !selected;
        draw_sidebar_card(
            frame,
            card_area,
            &app.agents[idx],
            app,
            selected,
            hovered,
            theme,
        );
        app.sidebar_click_map.push((idx, y, y + card_h));

        let is_last_visible = scroll.start + rel_i >= indices.len() - 1;
        y += if is_last_visible { card_h } else { row_h };
    }

    draw_scroll_indicators(frame, area, scroll.has_up, scroll.has_down, theme);
}

#[derive(Clone, Copy)]
pub(super) struct AgentCardMeta<'a> {
    accent: Color,
    status_color: Color,
    agent_type: &'static str,
    type_detail: &'a str,
    work_dir: Option<&'a str>,
}

pub(super) fn draw_sidebar_card(
    frame: &mut Frame,
    area: Rect,
    agent: &AgentEntry,
    app: &App,
    selected: bool,
    hovered: bool,
    theme: &Theme,
) {
    let meta = agent_card_meta(agent, app, theme);
    let status_color = effective_status_color(meta.status_color, agent, app, selected);
    let bg = if selected {
        theme.selected_bg
    } else if hovered {
        BG_HOVER
    } else {
        Color::Reset
    };
    let name = agent.id(app);

    let mut name_spans = vec![Span::styled(
        name,
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(if selected {
                meta.accent
            } else {
                theme.text_primary
            }),
    )];
    if is_agent_in_group(name, app) {
        name_spans.push(Span::styled(" [▣]", Style::default().fg(theme.dim_text)));
    }
    render_sidebar_card_line(frame, area, 0, bg, status_color, name_spans);

    let type_detail = format!(
        "{} · {}",
        meta.agent_type,
        truncate_str(meta.type_detail, area.width.saturating_sub(6) as usize)
    );
    render_sidebar_card_line(
        frame,
        area,
        1,
        bg,
        status_color,
        vec![Span::styled(
            type_detail,
            Style::default().fg(theme.dim_text),
        )],
    );

    let dir_text = meta
        .work_dir
        .filter(|dir| !dir.is_empty())
        .map(last_two_segments)
        .unwrap_or_else(|| "/".to_string());
    render_sidebar_card_line(
        frame,
        area,
        2,
        bg,
        status_color,
        vec![Span::styled(dir_text, Style::default().fg(theme.dim_text))],
    );
}

pub(super) fn agent_card_meta<'a>(
    agent: &'a AgentEntry,
    app: &'a App,
    theme: &Theme,
) -> AgentCardMeta<'a> {
    match agent {
        AgentEntry::Agent(agent) => AgentCardMeta {
            accent: theme.header_color,
            status_color: if !agent.enabled {
                STATUS_DISABLED
            } else if app.active_runs.contains_key(&agent.id) {
                STATUS_RUNNING
            } else if agent.last_run_ok == Some(false) {
                STATUS_FAIL
            } else {
                STATUS_OK
            },
            agent_type: agent.trigger_type_label(),
            // A send_to_session agent has no CLI (CM53) — name the target
            // session instead of rendering an empty harness slot.
            type_detail: if agent.is_send_to_session() {
                agent
                    .action
                    .target_session_name()
                    .or(agent.action.target_session_id())
                    .unwrap_or("session")
            } else {
                agent.cli.as_str()
            },
            work_dir: agent.working_dir.as_deref().or_else(|| agent.watch_path()),
        },
        AgentEntry::Interactive(index) => {
            let agent = &app.interactive_agents[*index];
            AgentCardMeta {
                accent: agent.accent_color,
                // Interactive agents pulse on recent output activity (unchanged).
                status_color: hub_session_status_color(
                    agent,
                    pty_session_status_color(
                        false,
                        &agent.status,
                        agent.has_recent_activity(),
                        false,
                        app.animation_tick,
                    ),
                ),
                agent_type: "pty",
                type_detail: agent.cli.as_str(),
                work_dir: Some(agent.working_dir.as_str()),
            }
        }
        AgentEntry::Terminal(index) => {
            let agent = &app.terminal_agents[*index];
            AgentCardMeta {
                accent: agent.accent_color,
                // Terminal sessions pulse while a foreground command executes
                // and go solid green at the prompt — output activity is
                // deliberately ignored (a scrolled-by finished command must
                // not keep pulsing; a `watch`/`tail -f` still counts as
                // executing). Same source of truth warp uses to gate input.
                status_color: hub_session_status_color(
                    agent,
                    pty_session_status_color(
                        true,
                        &agent.status,
                        false,
                        agent.foreground_app_active(),
                        app.animation_tick,
                    ),
                ),
                agent_type: "term",
                type_detail: agent.shell.as_str(),
                work_dir: Some(agent.working_dir.as_str()),
            }
        }
        AgentEntry::Orphaned(index) => {
            let session = &app.orphaned_sessions[*index];
            AgentCardMeta {
                accent: theme.muted_text,
                status_color: STATUS_FAIL,
                agent_type: "orphan",
                type_detail: session.cli.as_str(),
                work_dir: Some(session.working_dir.as_str()),
            }
        }
        AgentEntry::Group(_) => AgentCardMeta {
            accent: theme.header_color,
            status_color: STATUS_OK,
            agent_type: "group",
            type_detail: "",
            work_dir: None,
        },
        AgentEntry::Corrupt(_) => AgentCardMeta {
            accent: theme.error,
            status_color: STATUS_FAIL,
            agent_type: "corrupt config",
            type_detail: "",
            work_dir: None,
        },
    }
}

/// Status color for an interactive/terminal session card. A PTY is either
/// alive (green) or dead — and dead now splits into two: a session that
/// exited 0 reads as successful (`STATUS_OK`, blue), any other exit code
/// reads as failed (`STATUS_FAIL`, red). A running session pulses between
/// dim and bright green while it's registering activity (see
/// `ACTIVITY_IDLE_THRESHOLD_MS` and `pulse_active` — never blank, B21) and
/// holds solid green — "healthy, available" — once output has been quiet
/// for a while.
///
/// "Exit 0 means success" is scoped to *this* rendering decision only. It's
/// a safe read for a user-ended interactive session, but it is not a
/// general-purpose success signal — this project has been bitten before by
/// CLIs that print an error and still exit 0. Do not lift this assumption
/// into exit-code handling elsewhere without re-litigating it there.
pub(super) fn session_status_color(
    status: &AgentStatus,
    pulsing: bool,
    animation_tick: u32,
) -> Color {
    match status {
        AgentStatus::Running if pulsing => pulse_active(animation_tick),
        AgentStatus::Running => STATUS_RUNNING,
        AgentStatus::Exited(0) => STATUS_OK,
        AgentStatus::Exited(_) => STATUS_FAIL,
    }
}

/// A running session whose hub stream dropped reads as disconnected
/// (dim gray) instead of its live color; an exited session keeps its exit
/// color — that is the terminal truth about it.
fn hub_session_status_color(agent: &InteractiveAgent, live: Color) -> Color {
    if !agent.is_connected() && agent.status == AgentStatus::Running {
        STATUS_DISABLED
    } else {
        live
    }
}

/// Pick which signal drives a PTY-backed session's pulse, then defer to
/// [`session_status_color`]. The two session kinds pulse on different truths:
///
/// * Interactive agents pulse on **recent output activity** (`recent_activity`)
///   — the long-standing B21 behavior, kept exactly as is.
/// * Terminal (plain shell) sessions pulse on **command execution**
///   (`command_executing`, from `InteractiveAgent::foreground_app_active`) and
///   go solid green the moment the shell returns to its prompt, *regardless*
///   of output activity. A finished command whose output just scrolled by stops
///   pulsing immediately; a still-running `watch`/`tail -f` keeps pulsing.
///   Since the hub took the PTYs over (protocol v1 exposes no foreground
///   process group), `foreground_app_active` is a constant `false`, so this
///   signal stays off until the hub can report it.
///
/// Whichever signal isn't selected for a kind is ignored, so callers pass
/// `false` for it.
pub(super) fn pty_session_status_color(
    is_terminal: bool,
    status: &AgentStatus,
    recent_activity: bool,
    command_executing: bool,
    animation_tick: u32,
) -> Color {
    let pulsing = if is_terminal {
        command_executing
    } else {
        recent_activity
    };
    session_status_color(status, pulsing, animation_tick)
}

/// Alternate between `on` and the shared "off" tone on the TUI's existing
/// animation tick — the same cadence pulsing yellow uses — so a blinking
/// indicator never needs its own timer.
pub(super) fn pulse(on: Color, animation_tick: u32) -> Color {
    if (animation_tick / 10).is_multiple_of(2) {
        on
    } else {
        super::STATUS_WAIT_OFF
    }
}

/// Working-session heartbeat (B21): alternate between an illuminated green
/// and a muted gray-green on the shared animation tick. Unlike [`pulse`],
/// neither phase is blank — the indicator breathes, it never disappears.
pub(super) fn pulse_active(animation_tick: u32) -> Color {
    if (animation_tick / 10).is_multiple_of(2) {
        super::STATUS_RUNNING_BRIGHT
    } else {
        super::STATUS_RUNNING_DIM
    }
}

pub(super) fn effective_status_color(
    base: Color,
    agent: &AgentEntry,
    app: &App,
    selected: bool,
) -> Color {
    if !agent_is_waiting(agent, app, selected) {
        return base;
    }

    pulse(super::STATUS_WAIT_ON, app.animation_tick)
}

pub(super) fn agent_is_waiting(agent: &AgentEntry, app: &App, selected: bool) -> bool {
    if selected && matches!(app.focus, Focus::Agent | Focus::Preview) {
        return false;
    }

    match agent {
        AgentEntry::Interactive(index) => app.interactive_agents[*index].is_waiting_for_input(),
        // Terminal sessions always have the cursor at the last row (shell prompt),
        // so is_waiting_for_input would generate constant false positives.
        AgentEntry::Terminal(_) => false,
        _ => false,
    }
}

pub(super) fn is_agent_in_group(name: &str, app: &App) -> bool {
    app.split_groups
        .iter()
        .any(|group| group.session_a == name || group.session_b == name)
}

pub(super) fn render_sidebar_card_line<'a>(
    frame: &mut Frame,
    area: Rect,
    line_offset: u16,
    bg: Color,
    status_color: Color,
    mut spans: Vec<Span<'a>>,
) {
    if area.height < line_offset + 1 {
        return;
    }

    spans.insert(0, Span::raw(" "));
    spans.insert(0, Span::styled("▌", Style::default().fg(status_color)));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
        Rect::new(area.x, area.y + line_offset, area.width, 1),
    );
}

pub(super) fn group_agent_indices(app: &App) -> Vec<usize> {
    app.agents
        .iter()
        .enumerate()
        .filter(|(_, agent)| matches!(agent, AgentEntry::Group(_)))
        .map(|(index, _)| index)
        .collect()
}

#[derive(Clone, Copy)]
pub(super) struct GroupRowStyle {
    pub(super) bg: Color,
    pub(super) fg: Color,
    pub(super) modifier: Modifier,
    pub(super) prefix_color: Color,
    pub(super) active_tag: &'static str,
}

pub(super) fn group_row_style(is_selected: bool, is_active: bool, theme: &Theme) -> GroupRowStyle {
    GroupRowStyle {
        bg: if is_selected {
            theme.selected_bg
        } else {
            Color::Reset
        },
        fg: if is_selected {
            theme.header_color
        } else if is_active {
            theme.success
        } else {
            theme.text_primary
        },
        modifier: if is_active || is_selected {
            Modifier::BOLD
        } else {
            Modifier::empty()
        },
        prefix_color: if is_active {
            theme.success
        } else {
            theme.dim_text
        },
        active_tag: if is_active { " ●" } else { "" },
    }
}

pub(super) fn draw_groups_list(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let group_agent_indices = group_agent_indices(app);
    let mut y = area.y;

    for (position, (&agent_idx, group)) in group_agent_indices
        .iter()
        .zip(app.split_groups.iter())
        .enumerate()
    {
        if y >= area.y + area.height {
            break;
        }

        let is_selected = agent_idx == app.selected && !app.agents_rag_focused;
        let is_active = app
            .active_split_id
            .as_deref()
            .is_some_and(|id| id == group.id);
        let style = group_row_style(is_selected, is_active, theme);
        let label = format!("{} · {}", group.session_a, group.session_b);
        let text = format!(
            "{}{}",
            truncate_str(&label, area.width.saturating_sub(6) as usize),
            style.active_tag
        );
        let line = Line::from(vec![
            Span::styled("▌ ", Style::default().fg(style.prefix_color).bg(style.bg)),
            Span::styled(
                text,
                Style::default()
                    .fg(style.fg)
                    .bg(style.bg)
                    .add_modifier(style.modifier),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        app.sidebar_click_map.push((agent_idx, y, y + 1));

        y += if position < group_agent_indices.len() - 1 {
            2
        } else {
            1
        };
    }
}
