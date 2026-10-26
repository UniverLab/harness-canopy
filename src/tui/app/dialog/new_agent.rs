//! `NewAgentDialog` — state and logic for the "new agent" creation overlay.

use ratatui::style::Color;

use crate::domain::models::Cli;
use crate::domain::models_db::{self, ModelCatalog, ModelEntry};

use crate::tui::app::types::Focus;
use crate::tui::ui::theme::Theme;

pub(crate) mod dir_browser;
pub(crate) mod mode_step;

/// Seed identity option for the agent creation dialog.
#[derive(Clone, Debug)]
pub enum SeedOption {
    /// No seed identity — default behavior.
    None,
    /// Bind to an existing seed identity.
    Seed { id: String, name: String },
    /// Create a new seed via the Nursery graph.
    PlantNewSeed,
}

/// Type of background_agent to create.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NewTaskType {
    Interactive,
    Terminal,
    Background,
}

/// Trigger type for background agents.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BackgroundTrigger {
    Cron,
    Watch,
}

/// Launch mode for interactive agents.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NewTaskMode {
    /// Start a fresh interactive session.
    Interactive,
    /// Resume a previous session.
    Resume,
}

/// Which screen of the new-session dialog is on show (CT34). The Mode step
/// follows a confirmed Interactive create-mode form and asks New or Resume.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DialogStep {
    /// The form: type, harness, identity, directory and the rest.
    Form,
    /// The New session / Resume a session choice after the form.
    Mode,
}

/// State for the "new agent" dialog.
pub struct NewAgentDialog {
    /// When `Some(id)`, the dialog is in edit mode for an existing agent.
    pub edit_id: Option<String>,
    pub task_type: NewTaskType,
    pub task_mode: NewTaskMode,
    /// Which step of the dialog flow is showing.
    pub step: DialogStep,
    /// Selection on the Mode step: 0 = New session, 1 = Resume a session.
    pub mode_index: usize,
    /// Resumable sessions the Mode step's Resume row offers, filtered to the
    /// harness and directory chosen on the form (no dedupe — every matching
    /// session stays listable).
    pub mode_candidates: Vec<crate::db::session::InteractiveSession>,
    pub background_trigger: BackgroundTrigger,
    pub cli_index: usize,
    pub available_clis: Vec<Cli>,
    pub cli_configs: Vec<Option<crate::domain::cli_config::CliConfig>>,
    pub working_dir: String,
    pub model: String,
    pub prompt: String,
    /// Cursor position (char index) inside `prompt`.
    pub prompt_cursor: usize,
    /// First visible visual line of the multi-line prompt input.
    pub prompt_scroll: usize,
    pub cron_expr: String,
    pub watch_path: String,
    pub watch_events: Vec<String>,
    /// Detected shells available on the system.
    pub available_shells: Vec<String>,
    /// Index into `available_shells` for the selected shell.
    pub shell_index: usize,
    /// Which field is focused: depends on task_type
    pub field: usize,
    pub dir_entries: Vec<String>,
    pub dir_selected: usize,
    pub dir_scroll: usize,
    pub dir_filter: String,
    pub current_path: String,
    pub prev_focus: Option<Focus>,
    // ── CLI picker ──
    pub cli_picker_open: bool,
    pub cli_picker_idx: usize,
    pub cli_picker_filter: String,
    // ── Model suggestions ──
    pub model_catalog: Option<ModelCatalog>,
    pub model_suggestions: Vec<ModelEntry>,
    pub model_suggestion_idx: usize,
    pub model_picker_open: bool,
    // ── Session picker (canopy-side, for CLIs with session_list_cmd) ──
    pub session_picker_open: bool,
    /// Parsed sessions: (id, display_label)
    pub session_entries: Vec<(String, String)>,
    pub session_picker_idx: usize,
    /// The session the user confirmed, if any.
    pub selected_session: Option<(String, String)>,
    // ── Canopy-native session-resume picker (C12) ──
    // Distinct from the session picker above: that one lists a single
    // already-chosen CLI's own sessions via `session_list_cmd`. This one
    // lists canopy's own resumable sessions across harnesses, and the
    // harness follows from whichever one is picked.
    /// Open when `NewTaskMode::Resume` was chosen with more than one
    /// resumable session.
    pub session_resume_picker: Option<crate::tui::app::session_resume::SessionResumePicker>,
    /// The canopy session resolved via the picker, or set directly when
    /// exactly one resumable session existed. Its `cli` is the harness.
    pub selected_resume_session: Option<crate::tui::app::session_resume::ResumableSession>,
    /// Whether to launch the agent in yolo (autonomous) mode.
    pub yolo_mode: bool,
    /// Whether to launch in a sandbox (git worktree with instruction file).
    pub sandbox_mode: bool,
    /// Delivery action for a background agent (CM53): run a CLI headlessly, or
    /// deliver the prompt into a live interactive session.
    pub background_action: super::background_action::BackgroundAction,
    /// Index into `send_target_names` for the selected target session.
    pub send_target_idx: usize,
    /// Live session names offered as `send_to_session` targets, populated when
    /// the dialog opens from the app's interactive session list.
    pub send_target_names: Vec<String>,
    /// Index into `seed_options` for the selected seed identity.
    pub seed_index: usize,
    /// Available seed identity options.
    pub seed_options: Vec<SeedOption>,
}

impl NewAgentDialog {
    pub fn new(start_dir: Option<&str>) -> Self {
        let (available, configs) = Self::load_available_clis();
        let cwd = start_dir.map(|s| s.to_string()).unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        });
        let catalog_ttl = dirs::home_dir()
            .map(|home| {
                crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"))
                    .models
                    .catalog_ttl()
            })
            .unwrap_or(models_db::DEFAULT_CATALOG_TTL);
        let catalog = models_db::load_catalog_nonblocking(catalog_ttl);
        let seed_options = load_seed_options();
        let mut dialog = Self {
            edit_id: None,
            task_type: NewTaskType::Interactive,
            task_mode: NewTaskMode::Interactive,
            step: DialogStep::Form,
            mode_index: 0,
            mode_candidates: Vec::new(),
            background_trigger: BackgroundTrigger::Cron,
            cli_index: 0,
            available_clis: if available.is_empty() {
                vec![Cli::new("opencode"), Cli::new("kiro"), Cli::new("qwen")]
            } else {
                available
            },
            cli_configs: if configs.is_empty() {
                vec![None, None, None]
            } else {
                configs
            },
            working_dir: cwd.clone(),
            model: String::new(),
            prompt: String::new(),
            prompt_cursor: 0,
            prompt_scroll: 0,
            cron_expr: "0 9 * * *".to_string(),
            watch_path: cwd.clone(),
            watch_events: vec!["create".to_string(), "modify".to_string()],
            available_shells: detect_available_shells(),
            shell_index: 0,
            field: 1,
            dir_entries: Vec::new(),
            dir_selected: 0,
            dir_scroll: 0,
            dir_filter: String::new(),
            current_path: cwd,
            prev_focus: None,
            cli_picker_open: false,
            cli_picker_idx: 0,
            cli_picker_filter: String::new(),
            model_catalog: catalog,
            model_suggestions: Vec::new(),
            model_suggestion_idx: 0,
            model_picker_open: false,
            session_picker_open: false,
            session_entries: Vec::new(),
            session_picker_idx: 0,
            selected_session: None,
            session_resume_picker: None,
            selected_resume_session: None,
            yolo_mode: false,
            sandbox_mode: false,
            background_action: super::background_action::BackgroundAction::Headless,
            send_target_idx: 0,
            send_target_names: Vec::new(),
            seed_index: 0,
            seed_options,
        };
        dialog.refresh_dir_entries();
        dialog.refresh_model_suggestions();
        dialog
    }

    /// Get the selected shell path.
    pub fn selected_shell(&self) -> &str {
        self.available_shells
            .get(self.shell_index)
            .map(|s| s.as_str())
            .unwrap_or("bash")
    }

    pub fn selected_seed_id(&self) -> Option<&str> {
        match self.seed_options.get(self.seed_index) {
            Some(SeedOption::Seed { id, .. }) => Some(id),
            _ => None,
        }
    }

    /// Returns true if the user selected "Plant New Seed".
    pub fn is_planting_new_seed(&self) -> bool {
        matches!(
            self.seed_options.get(self.seed_index),
            Some(SeedOption::PlantNewSeed)
        )
    }

    fn load_available_clis() -> (Vec<Cli>, Vec<Option<crate::domain::cli_config::CliConfig>>) {
        let usage = dirs::home_dir()
            .map(|h| crate::domain::usage_stats::CliUsage::load(&h.join(".canopy")))
            .unwrap_or_default();

        // Try configured CLIs first; fall back to auto-detected ones.
        let pairs = Self::configured_cli_pairs().unwrap_or_else(|| {
            Cli::detect_available()
                .into_iter()
                .map(|cli| (cli, None))
                .collect()
        });
        Self::sort_clis_by_usage(pairs, &usage)
    }

    fn configured_cli_pairs() -> Option<Vec<(Cli, Option<crate::domain::cli_config::CliConfig>)>> {
        let home = dirs::home_dir()?;
        let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
        if config.clis.is_empty() {
            return None;
        }
        let pairs: Vec<_> = config
            .clis
            .iter()
            .filter_map(|c| {
                Cli::resolve(Some(&c.name))
                    .ok()
                    .map(|cli| (cli, Some(c.clone())))
            })
            .collect();
        if pairs.is_empty() {
            None
        } else {
            Some(pairs)
        }
    }

    /// Sort CLI-config pairs by usage count descending (most-used first).
    fn sort_clis_by_usage(
        mut pairs: Vec<(Cli, Option<crate::domain::cli_config::CliConfig>)>,
        usage: &crate::domain::usage_stats::CliUsage,
    ) -> (Vec<Cli>, Vec<Option<crate::domain::cli_config::CliConfig>>) {
        pairs.sort_by(|a, b| {
            let count_a = usage.get(a.0.as_str());
            let count_b = usage.get(b.0.as_str());
            count_b.cmp(&count_a)
        });
        pairs.into_iter().unzip()
    }

    pub fn selected_cli(&self) -> Cli {
        self.available_clis[self.cli_index].clone()
    }

    /// Human label for the harness at `idx`: the product display string
    /// with the slug in parentheses when it differs, so the user still
    /// knows what gets stored. Storage and matching always use the slug
    /// from [`selected_cli`](Self::selected_cli).
    pub fn cli_display_label(&self, idx: usize) -> String {
        let slug = self
            .available_clis
            .get(idx)
            .map(|c| c.as_str())
            .unwrap_or("");
        let config = self.cli_configs.get(idx).and_then(|c| c.as_ref());
        let display = config
            .map(|c| c.display_name())
            .unwrap_or_else(|| slug.to_string());
        let base = if display == slug {
            display
        } else {
            format!("{display} ({slug})")
        };
        // CM35: interactive-only platforms stay selectable for a live
        // session — never filtered out — but are marked so the user knows
        // what this harness can and cannot do.
        if config.is_some_and(|c| !c.headless) {
            format!("{base} (interactive only)")
        } else {
            base
        }
    }

    pub fn selected_args(&self) -> Option<String> {
        let config = self
            .cli_configs
            .get(self.cli_index)
            .and_then(|c| c.as_ref())?;

        let inter = config
            .interactive_args
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string());

        self.build_resume_args(config, inter)
    }

    fn build_resume_args(
        &self,
        config: &crate::domain::cli_config::CliConfig,
        inter: Option<String>,
    ) -> Option<String> {
        if !matches!(self.task_mode, NewTaskMode::Resume) {
            return inter;
        }

        // Session-specific resume: interactive_args + session_resume_cmd + id.
        // Takes precedence over the canopy-native picker below (decision 10):
        // it's a different, unrelated feature that keeps working as today.
        if let Some((ref id, _)) = self.selected_session {
            if let Some(ref cmd) = config.session_resume_cmd {
                return Some(match inter {
                    Some(ref i) => format!("{i} {cmd} {id}"),
                    None => format!("{cmd} {id}"),
                });
            }
        }

        // Canopy-native resume picker: rebuild the chosen session's original
        // command line the same way auto-resume does (decision 8), so its
        // original flags survive and resume args are never duplicated.
        if let Some(session) = self.selected_resume_session.as_ref() {
            return crate::tui::app::session_resume::build_resumed_session_args(
                session.args.as_deref(),
                inter.as_deref(),
                config.resume_args.as_deref(),
                config.session_resume_cmd.as_deref(),
                config.yolo_flag.as_deref(),
            );
        }

        // Generic resume: interactive_args + resume_args (each optional).
        match (inter, config.resume_args.clone()) {
            (Some(i), Some(r)) => Some(format!("{i} {r}")),
            (Some(i), None) => Some(i),
            (None, Some(r)) => Some(r),
            (None, None) => None,
        }
    }

    /// Returns true when the current CLI has no dedicated resume_args configured.
    pub fn is_edit_mode(&self) -> bool {
        self.edit_id.is_some()
    }

    /// Returns true when the current CLI has a dedicated session list command
    /// (independent of `task_mode`, so the Mode step can pick the list source
    /// before Resume is confirmed).
    pub fn has_cli_session_list(&self) -> bool {
        self.cli_configs
            .get(self.cli_index)
            .and_then(|c| c.as_ref())
            .map(|c| {
                c.session_list_cmd
                    .as_ref()
                    .map(|cmd| !cmd.trim().is_empty())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// Run the CLI's session_list_cmd, parse the output and populate session_entries.
    pub fn load_sessions(&mut self) {
        let Some(config) = self
            .cli_configs
            .get(self.cli_index)
            .and_then(|c| c.as_ref())
        else {
            return;
        };
        let Some(ref list_cmd) = config.session_list_cmd.clone() else {
            return;
        };
        // Defense in depth: an empty/whitespace command must never spawn the
        // CLI binary with zero args (it would block on a REPL prompt forever).
        if list_cmd.trim().is_empty() {
            return;
        }
        let binary = config.binary.clone();

        let args: Vec<&str> = list_cmd.split_whitespace().collect();
        let Ok(output) = std::process::Command::new(&binary)
            .args(&args)
            .current_dir(&self.working_dir)
            .output()
        else {
            return;
        };

        let text = String::from_utf8_lossy(&output.stdout);
        self.session_entries = parse_session_list(&text);
        self.session_picker_idx = 0;
    }

    /// Open the session picker: load sessions and set picker_open = true.
    pub fn open_session_picker(&mut self) {
        self.load_sessions();
        if !self.session_entries.is_empty() {
            self.session_picker_open = true;
        }
    }

    /// Confirm the currently highlighted session.
    pub fn confirm_session_pick(&mut self) {
        if let Some(entry) = self.session_entries.get(self.session_picker_idx) {
            self.selected_session = Some(entry.clone());
        }
        self.session_picker_open = false;
    }

    pub fn selected_fallback_args(&self) -> Option<String> {
        self.cli_configs
            .get(self.cli_index)
            .and_then(|c| c.as_ref())
            .and_then(|c| c.fallback_interactive_args.clone())
    }

    /// Returns the yolo flag for the currently selected CLI, if any.
    pub fn selected_yolo_flag(&self) -> Option<String> {
        self.cli_configs
            .get(self.cli_index)
            .and_then(|c| c.as_ref())
            .and_then(|c| c.yolo_flag.clone())
    }

    pub fn cli_accent_color(&self, idx: usize, theme: &Theme) -> Color {
        if self.task_type == NewTaskType::Terminal {
            return theme.header_color;
        }

        self.cli_configs
            .get(idx)
            .and_then(|c| c.as_ref())
            .and_then(|c| c.accent_color)
            .map(|[r, g, b]| Color::Rgb(r, g, b))
            .unwrap_or(crate::tui::ui::INTERACTIVE_COLOR)
    }

    pub fn selected_accent_color(&self, theme: &Theme) -> Color {
        self.cli_accent_color(self.cli_index, theme)
    }

    pub fn set_cli_index(&mut self, idx: usize) {
        if idx >= self.available_clis.len() {
            return;
        }
        self.cli_index = idx;
        self.refresh_model_suggestions();
        if self.selected_yolo_flag().is_none() {
            self.yolo_mode = false;
        }
    }

    /// Apply a session chosen via the canopy-native resume picker (or the
    /// sole candidate when there was only one): its harness becomes the
    /// dialog's CLI, without the user picking the CLI separately, and its
    /// working directory becomes the dialog's — a generic resume is
    /// `--continue`-shaped (resumes the most recent conversation of that CLI
    /// in that directory), so landing on the chosen conversation requires
    /// launching from the directory it was recorded in.
    pub fn apply_resume_choice(
        &mut self,
        session: crate::tui::app::session_resume::ResumableSession,
    ) {
        if let Some(idx) = self
            .available_clis
            .iter()
            .position(|cli| cli.as_str() == session.cli)
        {
            self.set_cli_index(idx);
        }
        self.working_dir = session.working_dir.clone();
        self.selected_resume_session = Some(session);
    }

    /// Clear all canopy-native resume-picker state (the Mode step's Resume
    /// branch re-opens, or the picker cancelled).
    pub fn reset_resume_choice(&mut self) {
        self.session_resume_picker = None;
        self.selected_resume_session = None;
    }

    pub fn open_cli_picker(&mut self) {
        self.cli_picker_open = true;
        self.cli_picker_filter.clear();
        self.sync_cli_picker_to_current();
    }

    pub fn close_cli_picker(&mut self) {
        self.cli_picker_open = false;
        self.cli_picker_filter.clear();
        self.sync_cli_picker_to_current();
    }

    pub fn filtered_cli_indices(&self) -> Vec<usize> {
        let query = self.cli_picker_filter.trim().to_lowercase();
        self.available_clis
            .iter()
            .enumerate()
            .filter(|(idx, cli)| {
                query.is_empty()
                    || cli.as_str().to_lowercase().contains(&query)
                    || self.cli_display_label(*idx).to_lowercase().contains(&query)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    pub fn sync_cli_picker_to_current(&mut self) {
        let filtered = self.filtered_cli_indices();
        self.cli_picker_idx = filtered
            .iter()
            .position(|&idx| idx == self.cli_index)
            .unwrap_or(0);
    }

    pub fn move_cli_picker_next(&mut self) {
        let filtered = self.filtered_cli_indices();
        if filtered.is_empty() {
            return;
        }
        self.cli_picker_idx =
            crate::tui::selection::move_index(self.cli_picker_idx, filtered.len(), true);
        self.set_cli_index(filtered[self.cli_picker_idx]);
    }

    pub fn move_cli_picker_prev(&mut self) {
        let filtered = self.filtered_cli_indices();
        if filtered.is_empty() {
            return;
        }
        self.cli_picker_idx =
            crate::tui::selection::move_index(self.cli_picker_idx, filtered.len(), false);
        self.set_cli_index(filtered[self.cli_picker_idx]);
    }

    pub fn push_cli_picker_filter(&mut self, c: char) {
        self.cli_picker_filter.push(c);
        self.apply_cli_picker_filter();
    }

    pub fn pop_cli_picker_filter(&mut self) {
        self.cli_picker_filter.pop();
        self.apply_cli_picker_filter();
    }

    pub fn apply_cli_picker_filter(&mut self) {
        let filtered = self.filtered_cli_indices();
        if filtered.is_empty() {
            self.cli_picker_idx = 0;
            return;
        }

        if let Some(pos) = filtered.iter().position(|&idx| idx == self.cli_index) {
            self.cli_picker_idx = pos;
        } else {
            self.cli_picker_idx = 0;
            self.set_cli_index(filtered[0]);
        }
    }

    /// Recompute the filtered model suggestions based on current CLI and query.
    pub fn refresh_model_suggestions(&mut self) {
        let Some(catalog) = &self.model_catalog else {
            self.model_suggestions.clear();
            return;
        };
        let binding = self.selected_cli();
        self.model_suggestions = models_db::suggestions_for(catalog, binding.as_str(), &self.model);
        // Clamp selection index
        if self.model_suggestion_idx >= self.model_suggestions.len() {
            self.model_suggestion_idx = 0;
        }
    }

    /// Accept the currently highlighted model suggestion.
    pub fn accept_model_suggestion(&mut self) {
        if let Some(entry) = self.model_suggestions.get(self.model_suggestion_idx) {
            self.model = entry.id.clone();
            self.model_picker_open = false;
        }
    }
}

/// Parse the output of a CLI session list command into (id, label) pairs.
/// Handles the opencode `session list` table format:
///   ses_<id>  Title...   Updated
/// Lines that are headers, separators, or do not start with an identifier are skipped.
pub fn parse_session_list(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('\u{2500}') // ─ separator
        })
        .filter_map(|line| {
            let mut parts = line.splitn(2, |c: char| c.is_whitespace());
            let id = parts.next()?.trim().to_string();
            // Skip header rows — real IDs contain letters+digits+mixed case
            if id == "Session" || id.len() < 8 {
                return None;
            }
            let label = parts.next().unwrap_or("").trim().to_string();
            Some((id, label))
        })
        .collect()
}

/// Detect installed shells on the system, ordered with the platform default first.
pub fn detect_available_shells() -> Vec<String> {
    let candidates = ["bash", "zsh", "fish", "sh"];

    let mut found: Vec<String> = candidates
        .iter()
        .filter(|name| {
            std::process::Command::new("which")
                .arg(name)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
        .map(|s| s.to_string())
        .collect();

    if found.is_empty() {
        found.push("bash".to_string());
    }

    // On macOS prefer zsh as default; on Linux prefer bash
    let preferred = if cfg!(target_os = "macos") {
        "zsh"
    } else {
        "bash"
    };

    if let Some(pos) = found.iter().position(|s| s == preferred) {
        found.swap(0, pos);
    }

    found
}

fn load_seed_options() -> Vec<SeedOption> {
    let mut options = vec![SeedOption::None];
    let seeds = crate::domain::seeds::list_seeds()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| {
            crate::domain::seeds::load_seed(&id)
                .ok()
                .map(|identity| SeedOption::Seed {
                    id,
                    name: identity.name,
                })
        });
    options.extend(seeds);
    options.push(SeedOption::PlantNewSeed);
    options
}

#[cfg(test)]
mod tests;
