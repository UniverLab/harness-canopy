//! The hub's Remote tab — GitHub sign-in, the relay switch, relay status,
//! pairing and the device list — plus the pairing-code overlay.
//!
//! The four fixed rows render first, then a `Devices (n)` header and one row
//! per paired device; the selected row carries the `▸` marker on
//! `selected_bg` like the Shelf tab. The overlay shows the last fetched
//! code with its link, QR and countdown.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::app::hub_remote::{remote_rows, LoginFlow, RemoteRow};
use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

/// Fixed rows before the devices header.
const FIXED_ROWS: usize = 4;

pub(super) fn draw_remote_tab(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let remote = &app.hub.remote;
    let foot_rows =
        usize::from(remote.revoke_confirm.is_some()) + usize::from(remote.message.is_some());
    let [main_area, foot_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(foot_rows as u16)]).areas(area);

    let width = main_area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (index, row) in remote_rows(&remote.view)
        .iter()
        .enumerate()
        .take(FIXED_ROWS)
    {
        lines.push(remote_row_line(
            app,
            theme,
            *row,
            remote.row_idx == index,
            width,
        ));
    }
    lines.push(devices_header_line(app, theme, width));
    lines.extend(device_lines(app, theme, main_area, width));

    frame.render_widget(Paragraph::new(lines), main_area);

    let mut foot_y = foot_area.y;
    if let Some(device_id) = &remote.revoke_confirm {
        draw_revoke_confirm(
            frame,
            app,
            theme,
            Rect::new(foot_area.x, foot_y, foot_area.width, 1),
            device_id,
        );
        foot_y += 1;
    }
    if let Some(message) = &remote.message {
        let style = if message.is_error {
            Style::default().fg(theme.error)
        } else {
            Style::default().fg(theme.success)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {}", truncate(&message.text, width)),
                style,
            ))),
            Rect::new(foot_area.x, foot_y, foot_area.width, 1),
        );
    }
}

/// The visible device rows, scrolled so the selected device shows.
fn device_lines(app: &App, theme: &Theme, main_area: Rect, width: usize) -> Vec<Line<'static>> {
    let remote = &app.hub.remote;
    let count = remote.view.devices.len();
    if count == 0 {
        return Vec::new();
    }
    let visible = (main_area.height as usize).saturating_sub(FIXED_ROWS + 1);
    let selected = remote.row_idx.saturating_sub(FIXED_ROWS).min(count - 1);
    let start = crate::tui::selection::clamp_scroll(selected, 0, count, visible.max(1));
    let end = (start + visible.max(1)).min(count);
    remote.view.devices[start..end]
        .iter()
        .enumerate()
        .map(|(offset, device)| {
            device_line(
                device,
                theme,
                remote.row_idx == FIXED_ROWS + start + offset,
                width,
            )
        })
        .collect()
}

/// One fixed row: the `▸` marker, a padded label and its value.
fn remote_row_line(
    app: &App,
    theme: &Theme,
    row: RemoteRow,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let (label, value, dimmed) = remote_row_text(app, row);
    let mut spans = vec![Span::styled(
        if selected { "▸" } else { " " },
        marker_style(theme, selected),
    )];
    let label_style = if selected {
        Style::default()
            .fg(theme.accent_fg)
            .add_modifier(Modifier::BOLD)
    } else if dimmed {
        Style::default().fg(theme.dim_text)
    } else {
        Style::default().fg(theme.text_primary)
    };
    let value_style = if dimmed {
        Style::default().fg(theme.dim_text)
    } else {
        Style::default().fg(theme.text_primary)
    };
    let mut rest = width.saturating_sub(1);
    let head = truncate(&format!(" {label:<14} "), rest);
    rest -= head.chars().count();
    spans.push(Span::styled(head, label_style));
    spans.push(Span::styled(truncate(&value, rest), value_style));
    let line = Line::from(spans);
    if selected {
        line.style(Style::default().bg(theme.selected_bg))
    } else {
        line
    }
}

fn marker_style(theme: &Theme, selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(theme.header_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.dim_text)
    }
}

/// The label, value and dim flag of one fixed row.
fn remote_row_text(app: &App, row: RemoteRow) -> (&'static str, String, bool) {
    let remote = &app.hub.remote;
    let view = &remote.view;
    match row {
        RemoteRow::GitHub => ("GitHub", github_value(remote), false),
        RemoteRow::Control => {
            let value = if view.enabled { "on" } else { "off" }.to_string();
            ("Remote control", value, !view.signed_in)
        }
        RemoteRow::Relay => ("Relay", view.relay.clone(), false),
        RemoteRow::Pair => ("Pair a device", "Enter for a new code".to_string(), false),
        RemoteRow::Device(_) => ("", String::new(), false),
    }
}

/// The GitHub row's value: the login, the waiting code and URL, or the
/// signed-out prompt.
fn github_value(remote: &crate::tui::app::hub_remote::RemoteState) -> String {
    match &remote.login {
        LoginFlow::Waiting {
            code: Some(code), ..
        } => {
            // Compact: the code and the URL must both fit the 80-column hub.
            format!("{} @ https://github.com/login/device", code.user_code)
        }
        LoginFlow::Waiting { code: None, .. } => "waiting for GitHub…".to_string(),
        LoginFlow::Idle => match &remote.view.login {
            Some(login) => format!("signed in as {login}"),
            None => "not signed in — Enter to sign in".to_string(),
        },
    }
}

fn devices_header_line(app: &App, theme: &Theme, width: usize) -> Line<'static> {
    let count = app.hub.remote.view.devices.len();
    Line::from(Span::styled(
        truncate(&format!("  Devices ({count})"), width),
        Style::default()
            .fg(theme.header_color)
            .add_modifier(Modifier::BOLD),
    ))
}

/// One `label  id  last seen …  live` device row.
fn device_line(
    device: &crate::remote::panel::DeviceView,
    theme: &Theme,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        if selected { "▸" } else { " " },
        marker_style(theme, selected),
    )];
    let name_style = if selected {
        Style::default()
            .fg(theme.accent_fg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text_primary)
    };
    let dim = Style::default().fg(theme.dim_text);
    let mut rest = width.saturating_sub(1);
    let mut push = |text: String, style: Style| {
        if rest == 0 {
            return;
        }
        let text = truncate(&text, rest);
        rest -= text.chars().count();
        spans.push(Span::styled(text, style));
    };
    push(" ".to_string(), Style::default());
    push(device.label.clone(), name_style);
    push("  ".to_string(), Style::default());
    push(device.id.clone(), dim);
    push("  last seen ".to_string(), Style::default());
    push(
        device
            .last_seen
            .as_deref()
            .map(super::hub_shelf::shelf_time)
            .unwrap_or_else(|| "never".to_string()),
        dim,
    );
    if device.live {
        push("  ".to_string(), Style::default());
        push("live".to_string(), Style::default().fg(theme.success));
    }
    let line = Line::from(spans);
    if selected {
        line.style(Style::default().bg(theme.selected_bg))
    } else {
        line
    }
}

/// The revoke confirmation pinned above the message line.
fn draw_revoke_confirm(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, device_id: &str) {
    let label = app
        .hub
        .remote
        .view
        .devices
        .iter()
        .find(|device| device.id == device_id)
        .map(|device| device.label.as_str())
        .unwrap_or(device_id);
    let text = format!("Revoke {label}?  y = confirm  n = cancel");
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&text, area.width as usize),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ))),
        area,
    );
}

/// The pairing overlay: code, link, QR, countdown and the Esc hint,
/// centered on the frame over the hub.
pub(super) fn draw_pair_overlay(frame: &mut Frame, app: &App, theme: &Theme) {
    let remote = &app.hub.remote;
    let (Some(pair), true) = (remote.pair.as_ref(), remote.pair_open) else {
        return;
    };
    let qr_lines: Vec<&str> = pair.qr.lines().collect();
    let code_line = format!("Pairing code: {}", pair.code);
    let countdown = pair_countdown(pair.expires_at, chrono::Utc::now().timestamp());
    let content_width = code_line
        .chars()
        .count()
        .max(pair.link.chars().count())
        .max(qr_width(&qr_lines))
        .max(countdown.chars().count());
    let frame_area = frame.area();
    let width = ((content_width + 4) as u16).min(frame_area.width.saturating_sub(2));
    let height = (qr_lines.len() as u16 + 6).min(frame_area.height);
    if width < 10 || height < 8 {
        return;
    }
    let area = centered_box(width, height, frame_area);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Pair a device ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(qr_lines.len() as u16),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    let inner_width = inner.width as usize;
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&code_line, inner_width),
            Style::default()
                .fg(theme.accent_fg)
                .add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&pair.link, inner_width),
            Style::default().fg(theme.text_primary),
        ))),
        rows[1],
    );
    let qr: Vec<Line> = qr_lines
        .iter()
        .map(|line| {
            Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(theme.text_primary),
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(qr), rows[2]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate(&countdown, inner_width),
            Style::default().fg(theme.dim_text),
        ))),
        rows[3],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Esc hide — the code stays valid",
            Style::default().fg(theme.dim_text),
        ))),
        rows[4],
    );
}

fn qr_width(lines: &[&str]) -> usize {
    lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
}

/// A `width`×`height` box centered on `area` (absolute cells, unlike
/// `centered_rect`'s horizontal percentage).
fn centered_box(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

/// `Expires in M:SS`, or the expired hint.
fn pair_countdown(expires_at: i64, now: i64) -> String {
    let remaining = expires_at.saturating_sub(now);
    if remaining <= 0 {
        return "Expired — Enter on Pair a device for a new code".to_string();
    }
    format!("Expires in {}:{:02}", remaining / 60, remaining % 60)
}

fn truncate(text: &str, width: usize) -> String {
    crate::tui::ui::truncate_str(text, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::remote::auth::DeviceCodeInfo;
    use crate::remote::panel::{DeviceView, PairView, RemoteView};
    use crate::tui::app::hub::HubTab;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::mpsc;
    use std::sync::Arc;

    fn test_app() -> App {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap()
    }

    fn remote_app() -> App {
        let mut app = test_app();
        app.hub_open = true;
        app.hub.tab = HubTab::Remote;
        app
    }

    fn device(id: &str, label: &str, live: bool) -> DeviceView {
        DeviceView {
            id: id.to_string(),
            label: label.to_string(),
            last_seen: Some("2026-10-01T10:00:00Z".to_string()),
            live,
        }
    }

    fn unseen_device(id: &str, label: &str) -> DeviceView {
        DeviceView {
            id: id.to_string(),
            label: label.to_string(),
            last_seen: None,
            live: false,
        }
    }

    fn draw(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                super::super::hub::draw_hub(frame, app, &theme);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    /// Every row state renders at both sizes.
    #[test]
    fn remote_tab_renders_each_row_state_at_both_sizes() {
        for (width, height) in [(80u16, 24u16), (120u16, 40u16)] {
            // Signed out, off.
            let mut app = remote_app();
            let text = draw(&mut app, width, height);
            assert!(text.contains("Remote"), "tab bar: {text}");
            assert!(text.contains("not signed in"), "{width}x{height}: {text}");
            assert!(text.contains("Remote control"), "{width}x{height}: {text}");
            assert!(text.contains("Relay"), "{width}x{height}: {text}");
            assert!(text.contains("Pair a device"), "{width}x{height}: {text}");
            assert!(text.contains("Devices (0)"), "{width}x{height}: {text}");
            assert!(text.contains('▸'), "cursor marker: {text}");

            // Signed in, on, connected.
            let mut app = remote_app();
            app.hub.remote.view = RemoteView {
                signed_in: true,
                login: Some("octocat".to_string()),
                enabled: true,
                relay_url: "https://remote.univerlab.org".to_string(),
                relay: "connected".to_string(),
                live_devices: vec!["bfcba2b742a807bb".to_string()],
                devices: vec![
                    device("bfcba2b742a807bb", "laptop", true),
                    unseen_device("aabbccddeeff0011", "tablet"),
                ],
            };
            let text = draw(&mut app, width, height);
            assert!(
                text.contains("signed in as octocat"),
                "{width}x{height}: {text}"
            );
            assert!(text.contains("connected"), "{width}x{height}: {text}");
            assert!(text.contains("laptop"), "{width}x{height}: {text}");
            assert!(
                text.contains("bfcba2b742a807bb"),
                "{width}x{height}: {text}"
            );
            assert!(text.contains("live"), "{width}x{height}: {text}");
            assert!(text.contains("last seen 10:00"), "{width}x{height}: {text}");
            assert!(text.contains("tablet"), "{width}x{height}: {text}");
            assert!(text.contains("never"), "{width}x{height}: {text}");

            // Token rejected.
            let mut app = remote_app();
            app.hub.remote.view.signed_in = true;
            app.hub.remote.view.login = Some("octocat".to_string());
            app.hub.remote.view.relay = "token rejected: sign in again".to_string();
            let text = draw(&mut app, width, height);
            assert!(
                text.contains("token rejected: sign in again"),
                "{width}x{height}: {text}"
            );

            // Connecting.
            let mut app = remote_app();
            app.hub.remote.view.signed_in = true;
            app.hub.remote.view.enabled = true;
            app.hub.remote.view.relay = "connecting".to_string();
            let text = draw(&mut app, width, height);
            assert!(text.contains("connecting"), "{width}x{height}: {text}");
        }
    }

    #[test]
    fn github_row_shows_the_waiting_code_and_url() {
        let mut app = remote_app();
        let (_tx, rx) = mpsc::channel();
        app.hub.remote.login = LoginFlow::Waiting {
            code: Some(DeviceCodeInfo {
                user_code: "ABCD-1234".to_string(),
                verification_uri_complete: None,
            }),
            rx,
        };
        for (width, height) in [(80u16, 24u16), (120u16, 40u16)] {
            let text = draw(&mut app, width, height);
            assert!(text.contains("ABCD-1234"), "{width}x{height}: {text}");
            assert!(
                text.contains("https://github.com/login/device"),
                "{width}x{height}: {text}"
            );
        }
    }

    #[test]
    fn revoke_confirm_and_footer_hint_render() {
        let mut app = remote_app();
        app.hub.remote.view.devices = vec![device("bfcba2b742a807bb", "laptop", false)];
        app.hub.remote.revoke_confirm = Some("bfcba2b742a807bb".to_string());
        let text = draw(&mut app, 120, 40);
        assert!(
            text.contains("Revoke laptop?  y = confirm  n = cancel"),
            "{text}"
        );
        assert!(text.contains("d revoke"), "footer names the key: {text}");
    }

    #[test]
    fn pair_overlay_shows_code_link_qr_and_countdown_at_both_sizes() {
        let qr = crate::remote::qr::render_level(
            "https://remote.univerlab.org/pair#7K3QH-M9P2X",
            qrcode::EcLevel::L,
        )
        .expect("qr");
        let qr_line_count = qr.lines().count();
        assert_eq!(qr_line_count, 17);
        for (width, height) in [(80u16, 24u16), (120u16, 40u16)] {
            let mut app = remote_app();
            app.hub.remote.pair = Some(PairView {
                code: "7K3QH-M9P2X".to_string(),
                link: "https://remote.univerlab.org/pair#7K3QH-M9P2X".to_string(),
                expires_at: chrono::Utc::now().timestamp() + 570,
                qr: qr.clone(),
            });
            app.hub.remote.pair_open = true;
            let text = draw(&mut app, width, height);
            assert!(text.contains("7K3QH-M9P2X"), "{width}x{height}: {text}");
            assert!(
                text.contains("https://remote.univerlab.org/pair#7K3QH-M9P2X"),
                "{width}x{height}: {text}"
            );
            assert!(
                text.contains("Expires in 9:"),
                "{width}x{height}: countdown: {text}"
            );
            assert!(text.contains("Esc hide"), "{width}x{height}: {text}");
            // The whole QR fits: every symbol line renders unclipped.
            let qr_rendered = qr
                .lines()
                .filter(|line| !line.trim().is_empty())
                .filter(|line| text.contains(line.trim_end()))
                .count();
            let qr_symbols = qr.lines().filter(|line| !line.trim().is_empty()).count();
            assert_eq!(
                qr_rendered, qr_symbols,
                "{width}x{height}: the QR fits unclipped: {text}"
            );
        }
    }

    #[test]
    fn pair_countdown_spells_expiry() {
        assert_eq!(pair_countdown(1000, 430), "Expires in 9:30");
        assert_eq!(pair_countdown(1000, 999), "Expires in 0:01");
        assert_eq!(
            pair_countdown(1000, 1000),
            "Expired — Enter on Pair a device for a new code"
        );
        assert_eq!(
            pair_countdown(1000, 2000),
            "Expired — Enter on Pair a device for a new code"
        );
    }
}
