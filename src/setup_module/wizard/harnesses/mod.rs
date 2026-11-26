//! Interactive harness picker: every registry harness on one screen with
//! its install status and dated free-tier fact. Installed harnesses start
//! pre-selected; `Space` toggles installed ones only, `f` sorts free tier
//! first, `/` filters, `Enter` confirms.
//!
//! Raw-mode crossterm loop in the same style as `dir_browser`, kept slim:
//! row text, sorting and filtering live in `harness_list`.

use crate::setup_module::harness_list::{
    check_responding, collect_harnesses, filter_harnesses, format_free_tier_label,
    resolve_harness_binary, sort_free_first, HarnessEntry, HarnessInstallStatus,
};
use crate::setup_module::install::{
    format_confirmation_lines, format_no_command_label, post_install_path_notice, prompt_yes_no,
    resolve_approved_platform, run_vendor_install, select_install_command, InstallAvail,
};
use crate::setup_module::login::{
    check_all_parallel, format_login_label, resolve_login_platform, run_check, run_login_pty,
    LoginRun, LoginStatus, LOGIN_INSTRUCTION,
};
use crate::setup_module::models::Platform;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

/// Rows shown at once; the frame always prints this many row lines so the
/// redraw cursor math stays fixed while filtering.
const VISIBLE_ROWS: usize = 12;
/// Header lines (title, help, filter) plus the footer line.
const CHROME_ROWS: usize = 4;

/// Picker state; `entries` is the current display order, `registry_names`
/// the registry order used to restore it and to map back to `platforms`.
struct PickerState {
    entries: Vec<HarnessEntry>,
    registry_names: Vec<String>,
    free_first: bool,
    cursor: usize,
    selected: HashSet<String>,
    filter: String,
    filtering: bool,
    scroll: usize,
    install_notes: HashMap<String, String>,
    last_failure_tail: Vec<String>,
    login_status: HashMap<String, LoginStatus>,
    login_check_rx: Option<std::sync::mpsc::Receiver<Vec<(String, LoginStatus)>>>,
}

/// Outcome of one key press.
enum KeyAction {
    Confirm,
    Cancel,
    Install,
    Login,
    Continue,
}

/// Show every registry harness and return the selected platforms in
/// registry order. Installed harnesses start pre-selected.
pub(crate) fn select_platforms_interactive(platforms: &[Platform]) -> Result<Vec<&Platform>> {
    if platforms.is_empty() {
        return Ok(vec![]);
    }
    let mut state = PickerState::new(platforms);
    run_picker_loop(&mut state)?;
    Ok(selected_platforms(platforms, &state))
}

/// Selected platforms in registry order.
fn selected_platforms<'a>(platforms: &'a [Platform], state: &PickerState) -> Vec<&'a Platform> {
    platforms
        .iter()
        .filter(|p| state.selected.contains(&p.name))
        .collect()
}

impl PickerState {
    /// Collect entries and pre-select every installed harness.
    fn new(platforms: &[Platform]) -> Self {
        let entries = collect_harnesses(platforms);
        let selected: HashSet<String> = entries
            .iter()
            .filter(|e| e.status != HarnessInstallStatus::NotInstalled)
            .map(|e| e.platform.name.clone())
            .collect();
        let registry_names: Vec<String> = entries.iter().map(|e| e.platform.name.clone()).collect();
        let login_check_rx = spawn_login_checks(&entries);
        Self {
            entries,
            registry_names,
            free_first: false,
            cursor: 0,
            selected,
            filter: String::new(),
            filtering: false,
            scroll: 0,
            install_notes: HashMap::new(),
            last_failure_tail: Vec::new(),
            login_status: HashMap::new(),
            login_check_rx: Some(login_check_rx),
        }
    }

    /// Display positions passing the current filter, via the shared helper.
    fn visible(&self) -> Vec<usize> {
        filter_harnesses(&self.entries, &self.filter)
            .into_iter()
            .map(|entry| self.position(entry))
            .collect()
    }

    /// Display position of one entry reference.
    fn position(&self, target: &HarnessEntry) -> usize {
        self.entries
            .iter()
            .position(|e| std::ptr::eq(e, target))
            .unwrap_or(0)
    }

    /// Rebuild display order after the free-first toggle: shared
    /// free-tier-first sort, or registry order when toggled off.
    fn apply_order(&mut self) {
        if self.free_first {
            sort_free_first(&mut self.entries);
        } else {
            let registry_names = &self.registry_names;
            self.entries.sort_by_key(|e| {
                registry_names
                    .iter()
                    .position(|name| *name == e.platform.name)
                    .unwrap_or(usize::MAX)
            });
        }
    }

    /// Clamp the cursor into the visible rows and keep it on screen.
    fn clamp_cursor(&mut self, rows: &[usize]) {
        if rows.is_empty() {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        self.cursor = self.cursor.min(rows.len() - 1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.cursor - VISIBLE_ROWS + 1;
        }
    }
}

/// Spawn background login checks for installed harnesses.
fn spawn_login_checks(
    entries: &[HarnessEntry],
) -> std::sync::mpsc::Receiver<Vec<(String, LoginStatus)>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let platforms: Vec<Platform> = entries
        .iter()
        .filter(|e| e.status != HarnessInstallStatus::NotInstalled)
        .map(|e| e.platform.clone())
        .collect();
    let canopy_dir = dirs::home_dir()
        .map(|h| h.join(".canopy"))
        .unwrap_or_default();
    std::thread::spawn(move || {
        let results = check_all_parallel(&platforms, &canopy_dir);
        let _ = tx.send(results);
    });
    rx
}

/// Apply any completed background login check results.
fn poll_login_checks(state: &mut PickerState) {
    let Some(rx) = state.login_check_rx.as_ref() else {
        return;
    };
    match rx.try_recv() {
        Ok(results) => {
            for (name, status) in results {
                state.login_status.insert(name, status);
            }
            state.login_check_rx = None;
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {}
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            state.login_check_rx = None;
        }
    }
}

/// Raw-mode event loop; leaves the terminal cooked on every exit.
fn run_picker_loop(state: &mut PickerState) -> Result<()> {
    use ratatui::crossterm::event::{read, Event, KeyEventKind};
    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    let total_rows = CHROME_ROWS + VISIBLE_ROWS;
    let _ = enable_raw_mode();
    for _ in 0..total_rows {
        print!("\r\n");
    }
    loop {
        poll_login_checks(state);
        let rows = state.visible();
        state.clamp_cursor(&rows);
        render_frame(state, &rows, total_rows);
        let _ = std::io::stdout().flush();
        let action = if ratatui::crossterm::event::poll(std::time::Duration::from_millis(200))
            .unwrap_or(false)
        {
            match read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    handle_key(state, &rows, key.code)
                }
                _ => KeyAction::Continue,
            }
        } else {
            KeyAction::Continue
        };
        match action {
            KeyAction::Confirm => {
                let _ = disable_raw_mode();
                print!("\r\n");
                return Ok(());
            }
            KeyAction::Cancel => {
                let _ = disable_raw_mode();
                print!("\r\n");
                return Err(anyhow::anyhow!("Selection cancelled"));
            }
            KeyAction::Install => {
                let _ = disable_raw_mode();
                print!("\r\n");
                handle_install_selected(state, &rows);
                let _ = enable_raw_mode();
                for _ in 0..total_rows {
                    print!("\r\n");
                }
            }
            KeyAction::Login => {
                let _ = disable_raw_mode();
                print!("\r\n");
                handle_login_selected(state, &rows);
                let _ = enable_raw_mode();
                for _ in 0..total_rows {
                    print!("\r\n");
                }
            }
            KeyAction::Continue => {}
        }
    }
}

/// One key press against the current rows.
fn handle_key(
    state: &mut PickerState,
    rows: &[usize],
    code: ratatui::crossterm::event::KeyCode,
) -> KeyAction {
    use ratatui::crossterm::event::KeyCode;
    if state.filtering {
        return handle_filter_key(state, code);
    }
    match code {
        KeyCode::Enter => KeyAction::Confirm,
        KeyCode::Esc => KeyAction::Cancel,
        KeyCode::Up => {
            state.cursor = state.cursor.saturating_sub(1);
            KeyAction::Continue
        }
        KeyCode::Down => {
            if !rows.is_empty() {
                state.cursor = (state.cursor + 1).min(rows.len() - 1);
            }
            KeyAction::Continue
        }
        KeyCode::Char(' ') => {
            toggle_installed(state, rows);
            KeyAction::Continue
        }
        KeyCode::Char('f') => {
            state.free_first = !state.free_first;
            state.apply_order();
            KeyAction::Continue
        }
        KeyCode::Char('i') => KeyAction::Install,
        KeyCode::Char('l') => {
            if can_login_selected(state, rows) {
                KeyAction::Login
            } else {
                KeyAction::Continue
            }
        }
        KeyCode::Char('r') => {
            if can_retry_selected(state, rows) {
                KeyAction::Login
            } else {
                KeyAction::Continue
            }
        }
        KeyCode::Char('/') => {
            state.filtering = true;
            KeyAction::Continue
        }
        _ => KeyAction::Continue,
    }
}

/// Keys while the `/` filter bar is open: type to narrow, `Enter`
/// to keep the filter and go back, `Esc` to clear it and go back.
fn handle_filter_key(
    state: &mut PickerState,
    code: ratatui::crossterm::event::KeyCode,
) -> KeyAction {
    use ratatui::crossterm::event::KeyCode;
    match code {
        KeyCode::Enter | KeyCode::Esc => {
            if code == KeyCode::Esc {
                state.filter.clear();
            }
            state.filtering = false;
            reset_view(state);
            KeyAction::Continue
        }
        KeyCode::Backspace => {
            state.filter.pop();
            reset_view(state);
            KeyAction::Continue
        }
        KeyCode::Char(c) if !c.is_control() && state.filter.len() < 50 => {
            state.filter.push(c);
            reset_view(state);
            KeyAction::Continue
        }
        _ => KeyAction::Continue,
    }
}

/// Back to the top of the list after the filter changed.
fn reset_view(state: &mut PickerState) {
    state.cursor = 0;
    state.scroll = 0;
}

/// Toggle selection, but only for installed harnesses: a missing binary
/// cannot be configured, so `Space` on it does nothing.
fn toggle_installed(state: &mut PickerState, rows: &[usize]) {
    if let Some(entry) = rows
        .get(state.cursor)
        .and_then(|pos| state.entries.get(*pos))
    {
        if entry.status != HarnessInstallStatus::NotInstalled {
            if state.selected.contains(&entry.platform.name) {
                state.selected.remove(&entry.platform.name);
            } else {
                state.selected.insert(entry.platform.name.clone());
            }
        }
    }
}

fn can_login_selected(state: &PickerState, rows: &[usize]) -> bool {
    rows.get(state.cursor)
        .and_then(|pos| state.entries.get(*pos))
        .is_some_and(|e| {
            e.status != HarnessInstallStatus::NotInstalled && e.platform.login_command.is_some()
        })
}

fn can_retry_selected(state: &PickerState, rows: &[usize]) -> bool {
    rows.get(state.cursor)
        .and_then(|pos| state.entries.get(*pos))
        .is_some_and(|e| {
            e.status != HarnessInstallStatus::NotInstalled
                && e.platform.login_command.is_some()
                && matches!(
                    state.login_status.get(&e.platform.name),
                    Some(LoginStatus::NotAuthenticated)
                )
        })
}

/// Prompt and run the vendor installer for the cursor row when eligible.
/// `--yes` never skips confirmation in the interactive harness list.
fn handle_install_selected(state: &mut PickerState, rows: &[usize]) {
    let home = dirs::home_dir();
    let path_env = std::env::var("PATH").unwrap_or_default();
    let Some(entry_pos) = rows.get(state.cursor).copied() else {
        return;
    };
    perform_row_install(
        state,
        entry_pos,
        home.as_deref(),
        &path_env,
        |lines| {
            for line in lines {
                println!("{line}");
            }
            prompt_yes_no()
        },
        run_vendor_install,
    );
}

/// Run the vendor login flow for the cursor row.
fn handle_login_selected(state: &mut PickerState, rows: &[usize]) {
    let home = dirs::home_dir();
    let Some(entry_pos) = rows.get(state.cursor).copied() else {
        return;
    };
    perform_row_login(state, entry_pos, home.as_deref(), run_login_pty, run_check);
}

/// Testable core of interactive row login.
fn perform_row_login(
    state: &mut PickerState,
    entry_pos: usize,
    home: Option<&Path>,
    runner: impl FnOnce(&str) -> LoginRun,
    checker: impl Fn(&str) -> bool,
) {
    let Some(entry) = state.entries.get(entry_pos) else {
        return;
    };
    if entry.status == HarnessInstallStatus::NotInstalled {
        return;
    }
    let Some(home) = home else {
        println!("Error: could not determine home directory");
        return;
    };
    let approved = match resolve_login_platform(&home.join(".canopy"), &entry.platform) {
        Ok(p) => p,
        Err(msg) => {
            println!("{msg}");
            state.install_notes.insert(entry.platform.name.clone(), msg);
            return;
        }
    };
    let Some(login_cmd) = approved.login_command.as_deref() else {
        return;
    };

    println!("{LOGIN_INSTRUCTION}");
    let _ = std::io::stdout().flush();
    let run = runner(login_cmd);
    if matches!(run, LoginRun::Cancelled) {
        return;
    }

    let status = match approved.login_check_command.as_deref() {
        Some(check) => {
            if checker(check) {
                LoginStatus::Authenticated
            } else {
                LoginStatus::NotAuthenticated
            }
        }
        None => LoginStatus::NotVerifiable,
    };
    state
        .login_status
        .insert(entry.platform.name.clone(), status);
}

/// Testable core of interactive row installation.
fn perform_row_install(
    state: &mut PickerState,
    entry_pos: usize,
    home: Option<&Path>,
    path_env: &str,
    confirm: impl FnOnce(&[String]) -> bool,
    runner: impl FnOnce(&str) -> anyhow::Result<(i32, Vec<String>)>,
) {
    let Some(entry) = state.entries.get(entry_pos) else {
        return;
    };
    if entry.status != HarnessInstallStatus::NotInstalled {
        return;
    }

    let approved = match home {
        Some(h) => match resolve_approved_platform(&h.join(".canopy"), &entry.platform) {
            Ok(p) => p,
            Err(msg) => {
                println!("{msg}");
                state.install_notes.insert(entry.platform.name.clone(), msg);
                return;
            }
        },
        None => entry.platform.clone(),
    };

    let InstallAvail::Ready(cmd) = select_install_command(&approved) else {
        return;
    };

    let lines = format_confirmation_lines(&approved, &cmd);
    if !confirm(&lines) {
        println!("Installation cancelled.");
        return;
    }

    state.last_failure_tail.clear();
    match runner(&cmd.command) {
        Ok((0, _)) => {
            let entry = &mut state.entries[entry_pos];
            let resolved = resolve_harness_binary(&approved, home, path_env);
            let new_status = match resolved {
                None => HarnessInstallStatus::NotInstalled,
                Some(ref bin) if check_responding(&approved, bin) => {
                    HarnessInstallStatus::Installed
                }
                Some(_) => HarnessInstallStatus::InstalledNotResponding,
            };
            entry.status = new_status;
            entry.binary_path = resolved;
            if new_status != HarnessInstallStatus::NotInstalled {
                state.selected.insert(entry.platform.name.clone());
            }
            if let Some(notice) = post_install_path_notice(&approved, home, path_env) {
                println!("{notice}");
                state
                    .install_notes
                    .insert(entry.platform.name.clone(), notice);
            } else {
                state.install_notes.remove(&entry.platform.name);
            }
        }
        Ok((code, tail)) => {
            println!("failed: {code}");
            for line in &tail {
                println!("{line}");
            }
            state.last_failure_tail = tail;
            state.install_notes.insert(
                state.entries[entry_pos].platform.name.clone(),
                format!("failed: {code}"),
            );
        }
        Err(err) => {
            println!("failed: {err}");
            state.install_notes.insert(
                state.entries[entry_pos].platform.name.clone(),
                format!("failed: {err}"),
            );
        }
    }
}

/// Redraw the whole frame in place.
fn render_frame(state: &PickerState, rows: &[usize], total_rows: usize) {
    print!("\x1b[{total_rows}A");
    print!("\r\x1b[2KSelect harnesses to configure (all registry harnesses listed)\r\n");
    print!(
        "\r\x1b[2K\x1b[90m↑↓ move  Space toggle  i install  l login  r retry  f free-tier first  / filter  Enter confirm  Esc cancel\x1b[0m\r\n"
    );
    render_filter_line(state);
    render_rows(state, rows);
    print!(
        "\r\x1b[2K\x1b[90m{}/{} shown, {} selected{}\x1b[0m\r\n",
        rows.len(),
        state.entries.len(),
        state.selected.len(),
        if state.free_first {
            " (free tier first)"
        } else {
            ""
        },
    );
}

/// The filter line, with a cursor hint while filtering.
fn render_filter_line(state: &PickerState) {
    if state.filtering {
        print!("\r\x1b[2K\x1b[33m/{}▌\x1b[0m\r\n", state.filter);
    } else if state.filter.is_empty() {
        print!("\r\x1b[2K\x1b[90m/\x1b[0m to filter\r\n");
    } else {
        print!(
            "\r\x1b[2K\x1b[33m/{} \x1b[90m(Esc clears)\x1b[0m\r\n",
            state.filter
        );
    }
}

/// The visible row window, padded with blank lines to a fixed height.
fn render_rows(state: &PickerState, rows: &[usize]) {
    for slot in 0..VISIBLE_ROWS {
        match rows.get(state.scroll + slot) {
            Some(entry_pos) => {
                let cursor_here = state.scroll + slot == state.cursor;
                render_row(state, *entry_pos, cursor_here);
            }
            None => print!("\r\x1b[2K\r\n"),
        }
    }
}

/// One harness row: checkbox, name, status, free-tier label, login state.
fn render_row(state: &PickerState, entry_pos: usize, cursor_here: bool) {
    let entry = &state.entries[entry_pos];
    let checkbox = if state.selected.contains(&entry.platform.name) {
        "[x]"
    } else {
        "[ ]"
    };
    let status = match entry.status {
        HarnessInstallStatus::Installed => "installed",
        HarnessInstallStatus::InstalledNotResponding => "installed (not responding)",
        HarnessInstallStatus::NotInstalled => "not installed",
    };
    let mut text = format!(
        "{checkbox} {} | {status} | {}",
        entry.platform.name,
        format_free_tier_label(&entry.platform)
    );
    if entry.status == HarnessInstallStatus::NotInstalled {
        if let Some(hint) = format_row_install_hint(entry, dirs::home_dir().as_deref()) {
            text.push_str(&format!(" | {hint}"));
        }
        text.push_str(&format!(" | {}", format_login_label(&entry.platform, None)));
        if let Some(url) = entry.platform.docs_url.as_deref() {
            text.push_str(&format!(" | docs: {url}"));
        }
    } else {
        let login_status = state.login_status.get(&entry.platform.name).copied();
        text.push_str(&format!(
            " | {}",
            format_login_label(&entry.platform, login_status)
        ));
        if entry.platform.login_command.is_some() {
            if matches!(login_status, Some(LoginStatus::NotAuthenticated)) {
                text.push_str(" | r retry");
            } else {
                text.push_str(" | l login");
            }
        }
    }
    if let Some(note) = state.install_notes.get(&entry.platform.name) {
        text.push_str(&format!(" | {note}"));
    }
    if cursor_here {
        print!("\r\x1b[2K\x1b[1m> {text}\x1b[0m\r\n");
    } else {
        print!("\r\x1b[2K  {text}\r\n");
    }
}

/// Status hint shown on a `NotInstalled` row for installation availability.
fn format_row_install_hint(entry: &HarnessEntry, home: Option<&Path>) -> Option<String> {
    let platform = match home {
        Some(h) => match resolve_approved_platform(&h.join(".canopy"), &entry.platform) {
            Ok(p) => p,
            Err(_) => return Some("install: pending approval".to_string()),
        },
        None => entry.platform.clone(),
    };
    Some(match select_install_command(&platform) {
        InstallAvail::Ready(_) => "i install".to_string(),
        InstallAvail::Needs { missing, .. } => format!("needs {missing} first"),
        InstallAvail::NoCommand { docs_url } => format_no_command_label(docs_url.as_deref()),
    })
}

#[cfg(test)]
mod tests;
