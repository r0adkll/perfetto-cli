use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use tokio::sync::mpsc::UnboundedSender;

use crate::adb;
use crate::adb::DeviceInfo;
use crate::config::Paths;
use crate::db::Database;
use crate::db::command_sets::SavedCommandSet;
use crate::db::configs::SavedConfig;
use crate::perfetto::TraceConfig;
use crate::session::Session;
use crate::tui::chrome;
use crate::tui::event::AppEvent;
use crate::tui::screens::device_picker::{self, DeviceEntry, EntryState};
use crate::tui::text_input::{self, TextAction};
use crate::tui::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Name,
    Package,
    Suggestions,
    Device,
    Config,
    Commands,
    Submit,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Name => Focus::Package,
            Focus::Package => Focus::Suggestions,
            Focus::Suggestions => Focus::Device,
            Focus::Device => Focus::Config,
            Focus::Config => Focus::Commands,
            Focus::Commands => Focus::Submit,
            Focus::Submit => Focus::Name,
        }
    }
    fn prev(self) -> Self {
        match self {
            Focus::Name => Focus::Submit,
            Focus::Package => Focus::Name,
            Focus::Suggestions => Focus::Package,
            Focus::Device => Focus::Suggestions,
            Focus::Config => Focus::Device,
            Focus::Commands => Focus::Config,
            Focus::Submit => Focus::Commands,
        }
    }
}

pub struct NewSessionScreen {
    name: String,
    package: String,
    focus: Focus,
    devices: Vec<DeviceEntry>,
    device_state: ListState,
    loading_devices: bool,
    /// Distinct package names pulled from the sessions DB — persists across
    /// runs so the user can pick a previously-used package without retyping.
    recent_packages: Vec<String>,
    /// Third-party packages queried live from the selected device via
    /// `pm list packages -3`. Merged with `recent_packages` for suggestions.
    device_packages: Vec<String>,
    /// Human-readable name of the device those packages came from — used in
    /// the suggestion tags so `[pixel-7]` shows up instead of `[device]`.
    device_packages_source: Option<String>,
    /// Serial of the device we last queried packages for, so we can detect
    /// when the selection changes and re-query.
    device_packages_serial: Option<String>,
    /// Filtered view over `recent_packages` + `device_packages`, rebuilt on
    /// every keystroke in the package field.
    suggestions: Vec<String>,
    suggestions_state: ListState,
    loading_packages: bool,
    /// Saved configs available for selection. Index 0 = "Default", rest from DB.
    saved_configs: Vec<SavedConfig>,
    config_state: ListState,
    /// Saved command sets. Index 0 = "None", rest from DB.
    saved_command_sets: Vec<SavedCommandSet>,
    command_set_state: ListState,
    /// Info about the currently-highlighted online device.
    device_info: Option<DeviceInfo>,
    device_info_serial: Option<String>,
    error: Option<String>,
    db: Database,
    paths: Paths,
    tx: UnboundedSender<AppEvent>,
}

pub enum WizardAction {
    None,
    Cancel,
    Created(i64),
}

impl NewSessionScreen {
    pub fn new(db: Database, paths: Paths, tx: UnboundedSender<AppEvent>) -> Self {
        let recent_packages = db.list_recent_packages().unwrap_or_default();
        let saved_configs = db.list_configs().unwrap_or_default();
        let mut config_state = ListState::default();
        config_state.select(Some(0));
        let saved_command_sets = db.list_command_sets().unwrap_or_default();
        let mut command_set_state = ListState::default();
        command_set_state.select(Some(0)); // "None" is pre-selected
        let mut screen = Self {
            name: String::new(),
            package: String::new(),
            focus: Focus::Name,
            devices: Vec::new(),
            device_state: ListState::default(),
            loading_devices: true,
            recent_packages,
            device_packages: Vec::new(),
            device_packages_source: None,
            device_packages_serial: None,
            suggestions: Vec::new(),
            suggestions_state: ListState::default(),
            loading_packages: false,
            saved_configs,
            config_state,
            saved_command_sets,
            command_set_state,
            device_info: None,
            device_info_serial: None,
            error: None,
            db,
            paths,
            tx,
        };
        screen.recompute_suggestions();
        screen.spawn_device_load();
        screen
    }

    fn recompute_suggestions(&mut self) {
        let query = self.package.trim().to_lowercase();
        // Preserve order: most-recent DB packages first, then device
        // packages. Dedup as we go so a package that's both recalled and
        // live only shows up once.
        let mut combined: Vec<String> = Vec::new();
        for p in self.recent_packages.iter().chain(self.device_packages.iter()) {
            if !combined.iter().any(|x| x == p) {
                combined.push(p.clone());
            }
        }
        self.suggestions = combined
            .into_iter()
            .filter(|p| query.is_empty() || p.to_lowercase().contains(&query))
            .collect();

        if self.suggestions.is_empty() {
            self.suggestions_state.select(None);
        } else {
            let idx = self
                .suggestions_state
                .selected()
                .unwrap_or(0)
                .min(self.suggestions.len() - 1);
            self.suggestions_state.select(Some(idx));
        }
    }

    fn spawn_packages_load(&mut self, serial: String) {
        self.loading_packages = true;
        self.device_packages_serial = Some(serial.clone());
        self.device_packages_source = self
            .devices
            .iter()
            .find(|d| d.serial == serial)
            .map(device_display_name);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = adb::list_installed_packages(&serial)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::PackagesLoaded(result));
        });
    }

    pub fn on_packages_loaded(&mut self, result: Result<Vec<String>, String>) {
        self.loading_packages = false;
        match result {
            Ok(pkgs) => {
                self.device_packages = pkgs;
                self.recompute_suggestions();
            }
            Err(e) => {
                // Soft fail — we still have recent packages from the DB.
                tracing::warn!(error = %e, "failed to list device packages");
            }
        }
    }

    fn spawn_device_load(&mut self) {
        self.loading_devices = true;
        let db = self.db.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = device_picker::load_entries(db)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::DevicesLoaded(result));
        });
    }

    pub fn on_devices_loaded(&mut self, result: Result<Vec<DeviceEntry>, String>) {
        self.loading_devices = false;
        match result {
            Ok(entries) => {
                self.devices = entries;
                if self.device_state.selected().is_none() && !self.devices.is_empty() {
                    // Prefer the first online device.
                    let idx = self
                        .devices
                        .iter()
                        .position(|e| matches!(e.state, EntryState::Online))
                        .unwrap_or(0);
                    self.device_state.select(Some(idx));
                }
                // Kick off the installed-packages query for whichever online
                // device is highlighted. If none is online we stick with the
                // DB-sourced recent packages.
                if let Some(serial) = self.online_device_serial() {
                    self.spawn_packages_load(serial);
                }
                self.maybe_fetch_device_info();
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Serial of the currently-highlighted device if it's online, else `None`.
    fn online_device_serial(&self) -> Option<String> {
        let idx = self.device_state.selected()?;
        let entry = self.devices.get(idx)?;
        if matches!(entry.state, EntryState::Online) {
            Some(entry.serial.clone())
        } else {
            None
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> WizardAction {
        if key.kind != KeyEventKind::Press {
            return WizardAction::None;
        }

        // Global keys.
        match key.code {
            KeyCode::Esc => return WizardAction::Cancel,
            KeyCode::Tab => {
                self.focus = self.focus.next();
                return WizardAction::None;
            }
            KeyCode::BackTab => {
                self.focus = self.focus.prev();
                return WizardAction::None;
            }
            _ => {}
        }

        match self.focus {
            Focus::Name => self.handle_text_key(key, TextField::Name),
            Focus::Package => self.handle_text_key(key, TextField::Package),
            Focus::Suggestions => self.handle_suggestions_key(key),
            Focus::Device => self.handle_device_key(key),
            Focus::Config => self.handle_config_key(key),
            Focus::Commands => self.handle_commands_key(key),
            Focus::Submit => match key.code {
                KeyCode::Enter => self.try_create(),
                _ => WizardAction::None,
            },
        }
    }

    fn handle_text_key(&mut self, key: KeyEvent, field: TextField) -> WizardAction {
        let buffer = match field {
            TextField::Name => &mut self.name,
            TextField::Package => &mut self.package,
        };
        let action = text_input::apply(buffer, &key);
        // Any edit to the package field re-filters the suggestions list so
        // the user sees matching packages shrink as they type.
        if matches!(field, TextField::Package) && matches!(action, TextAction::Edited) {
            self.recompute_suggestions();
        }
        match action {
            TextAction::Submit => {
                self.focus = self.focus.next();
            }
            // Cancel is pre-intercepted at the top of `on_key` (Esc → global
            // cancel), so reaching Cancel here would be a logic bug; ignore.
            _ => {}
        }
        WizardAction::None
    }

    fn handle_suggestions_key(&mut self, key: KeyEvent) -> WizardAction {
        if self.suggestions.is_empty() {
            // Nothing to pick — any key just falls through to the next field.
            if matches!(key.code, KeyCode::Enter) {
                self.focus = Focus::Device;
            }
            return WizardAction::None;
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_suggestion(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_suggestion(-1);
            }
            KeyCode::Enter => {
                if let Some(i) = self.suggestions_state.selected() {
                    if let Some(pkg) = self.suggestions.get(i).cloned() {
                        self.package = pkg;
                        self.recompute_suggestions();
                    }
                }
                self.focus = Focus::Device;
            }
            _ => {}
        }
        WizardAction::None
    }

    fn move_suggestion(&mut self, delta: i32) {
        if self.suggestions.is_empty() {
            return;
        }
        let len = self.suggestions.len() as i32;
        let current = self.suggestions_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.suggestions_state.select(Some(next as usize));
    }

    fn handle_device_key(&mut self, key: KeyEvent) -> WizardAction {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_device(1);
                self.maybe_fetch_device_info();
                self.maybe_refresh_packages();
                WizardAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_device(-1);
                self.maybe_fetch_device_info();
                self.maybe_refresh_packages();
                WizardAction::None
            }
            KeyCode::Char('r') => {
                self.spawn_device_load();
                WizardAction::None
            }
            KeyCode::Enter => {
                self.focus = Focus::Config;
                WizardAction::None
            }
            _ => WizardAction::None,
        }
    }

    pub fn on_device_info_loaded(&mut self, result: Result<DeviceInfo, String>) {
        if let Ok(info) = result {
            if self.device_info_serial.as_deref() == Some(&info.serial) {
                self.device_info = Some(info);
            }
        }
    }

    /// Re-query installed packages if the selected device changed.
    fn maybe_refresh_packages(&mut self) {
        let current_serial = self.selected_online_device().map(|d| d.serial.clone());
        if current_serial != self.device_packages_serial {
            match current_serial {
                Some(serial) => self.spawn_packages_load(serial),
                None => {
                    self.device_packages.clear();
                    self.device_packages_source = None;
                    self.device_packages_serial = None;
                    self.recompute_suggestions();
                }
            }
        }
    }

    fn maybe_fetch_device_info(&mut self) {
        let Some(entry) = self.selected_online_device() else {
            self.device_info = None;
            self.device_info_serial = None;
            return;
        };
        let serial = entry.serial.clone();
        if self.device_info_serial.as_deref() == Some(&serial) {
            return; // already loading or loaded
        }
        self.device_info = None;
        self.device_info_serial = Some(serial.clone());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = adb::query_device_info(&serial).await.map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::DeviceInfoLoaded(result));
        });
    }

    fn selected_online_device(&self) -> Option<&DeviceEntry> {
        let idx = self.device_state.selected()?;
        let entry = self.devices.get(idx)?;
        if matches!(entry.state, EntryState::Online) {
            Some(entry)
        } else {
            None
        }
    }

    fn handle_config_key(&mut self, key: KeyEvent) -> WizardAction {
        // Total options = 1 ("Default") + saved_configs.len()
        let total = 1 + self.saved_configs.len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                let cur = self.config_state.selected().unwrap_or(0);
                self.config_state.select(Some((cur + 1) % total));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let cur = self.config_state.selected().unwrap_or(0);
                self.config_state
                    .select(Some((cur + total - 1) % total));
            }
            KeyCode::Enter => {
                self.focus = Focus::Submit;
            }
            _ => {}
        }
        WizardAction::None
    }

    fn handle_commands_key(&mut self, key: KeyEvent) -> WizardAction {
        let total = 1 + self.saved_command_sets.len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                let cur = self.command_set_state.selected().unwrap_or(0);
                self.command_set_state.select(Some((cur + 1) % total));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let cur = self.command_set_state.selected().unwrap_or(0);
                self.command_set_state
                    .select(Some((cur + total - 1) % total));
            }
            KeyCode::Enter => {
                self.focus = Focus::Submit;
            }
            _ => {}
        }
        WizardAction::None
    }

    /// Returns the selected startup commands. Index 0 = None, 1+ = saved sets.
    fn selected_startup_commands(&self) -> Vec<crate::perfetto::commands::StartupCommand> {
        match self.command_set_state.selected().unwrap_or(0) {
            0 => Vec::new(),
            i => self
                .saved_command_sets
                .get(i - 1)
                .map(|s| s.commands.clone())
                .unwrap_or_default(),
        }
    }

    /// Returns the TraceConfig for the currently selected config option.
    /// Index 0 = Default, 1+ = saved configs from DB.
    fn selected_trace_config(&self) -> TraceConfig {
        match self.config_state.selected().unwrap_or(0) {
            0 => TraceConfig::default(),
            i => self
                .saved_configs
                .get(i - 1)
                .map(|c| c.config.clone())
                .unwrap_or_default(),
        }
    }

    fn move_device(&mut self, delta: i32) {
        if self.devices.is_empty() {
            return;
        }
        let len = self.devices.len() as i32;
        let current = self.device_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.device_state.select(Some(next as usize));
    }

    fn try_create(&mut self) -> WizardAction {
        let name = self.name.trim();
        let package = self.package.trim();
        if name.is_empty() {
            self.error = Some("Name is required".into());
            self.focus = Focus::Name;
            return WizardAction::None;
        }
        if package.is_empty() {
            self.error = Some("Package name is required".into());
            self.focus = Focus::Package;
            return WizardAction::None;
        }
        let serial = self
            .device_state
            .selected()
            .and_then(|i| self.devices.get(i))
            .filter(|e| matches!(e.state, EntryState::Online))
            .map(|e| e.serial.clone());
        if serial.is_none() {
            self.error = Some("Select an online device".into());
            self.focus = Focus::Device;
            return WizardAction::None;
        }

        let created_at = Utc::now();
        let folder = Session::unique_folder_path(&self.paths.sessions_dir(), name);
        let session = Session {
            id: None,
            name: name.to_string(),
            package_name: package.to_string(),
            device_serial: serial,
            config: {
                let mut cfg = self.selected_trace_config();
                cfg.startup_commands = self.selected_startup_commands();
                cfg
            },
            folder_path: folder,
            created_at,
            notes: None,
        };

        if let Err(e) = session.ensure_filesystem() {
            self.error = Some(format!("failed to create folder: {e}"));
            return WizardAction::None;
        }
        match self.db.create_session(&session) {
            Ok(id) => WizardAction::Created(id),
            Err(e) => {
                self.error = Some(format!("db insert failed: {e}"));
                WizardAction::None
            }
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        let header = chrome::app_header(Line::from(vec![
            Span::styled("  ✨ New session ", theme::title()),
            Span::styled("— Tab to move between fields, Esc to cancel", theme::hint()),
        ]));
        frame.render_widget(header, outer[0]);

        // Two-pane: left 2/3 = form fields, right 1/3 = device list + info
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(outer[1]);

        // --- Left pane: form fields ---
        let form = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(6),
                Constraint::Length(5),
                Constraint::Length(5),
                Constraint::Length(3),
                Constraint::Min(0),
            ])
            .split(panes[0]);

        self.render_text_field(
            frame,
            form[0],
            "Session name",
            &self.name,
            self.focus == Focus::Name,
        );
        self.render_text_field(
            frame,
            form[1],
            "Package name (e.g. com.example.myapp)",
            &self.package,
            self.focus == Focus::Package,
        );
        self.render_suggestions(frame, form[2]);
        self.render_config_picker(frame, form[3]);
        self.render_command_set_picker(frame, form[4]);

        let submit_text = if self.focus == Focus::Submit {
            " ▶ Create session (Enter) "
        } else {
            "   Create session "
        };
        let submit = Paragraph::new(Line::from(Span::styled(
            submit_text,
            if self.focus == Focus::Submit {
                Style::default()
                    .bg(theme::accent())
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(focus_style(self.focus == Focus::Submit)),
        );
        frame.render_widget(submit, form[5]);

        // --- Right pane: device list (top) + device info (bottom) ---
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(panes[1]);

        self.render_device_list(frame, right[0]);
        self.render_device_info(frame, right[1]);

        // --- Footer ---
        let footer = match &self.error {
            Some(msg) => Line::from(Span::styled(
                format!(" ✗ {msg}"),
                Style::default().fg(theme::err()),
            )),
            None => Line::from(Span::styled(
                " Tab/Shift+Tab to move focus  •  Enter advances  •  Esc cancels",
                theme::hint(),
            )),
        };
        frame.render_widget(Paragraph::new(footer), outer[2]);
    }

    fn render_device_list(&mut self, frame: &mut Frame, area: Rect) {
        let device_block = Block::default()
            .borders(Borders::ALL)
            .title(" Device ")
            .border_style(focus_style(self.focus == Focus::Device));

        if self.loading_devices && self.devices.is_empty() {
            frame.render_widget(
                Paragraph::new("  ⏳ listing devices…").block(device_block),
                area,
            );
        } else if self.devices.is_empty() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from("  No devices found."),
                    Line::from(Span::styled(
                        "  Press [r] to retry.",
                        theme::hint(),
                    )),
                ])
                .block(device_block),
                area,
            );
        } else {
            let items: Vec<ListItem> = self
                .devices
                .iter()
                .map(|e| {
                    let label = e.nickname.clone().unwrap_or_else(|| {
                        e.model.clone().unwrap_or_else(|| "Android device".into())
                    });
                    let state_str = match e.state {
                        EntryState::Online => "●",
                        EntryState::Offline => "○",
                        EntryState::Unauthorized => "⚠",
                        EntryState::Other(_) => "?",
                        EntryState::NotConnected => "·",
                    };
                    ListItem::new(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(state_str, theme::hint()),
                        Span::raw(" "),
                        Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .block(device_block)
                .highlight_style(Style::default().bg(theme::accent()).fg(Color::Black))
                .highlight_symbol("▶ ");
            frame.render_stateful_widget(list, area, &mut self.device_state);
        }
    }

    fn render_device_info(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Device Info ");

        match &self.device_info {
            Some(info) => {
                let mut lines = Vec::new();
                if let Some(name) = &info.device_name {
                    lines.push(Line::from(vec![
                        Span::styled("  Device:   ", theme::hint()),
                        Span::styled(
                            name.clone(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
                if let Some(mfr) = &info.manufacturer {
                    lines.push(Line::from(vec![
                        Span::styled("  Make:     ", theme::hint()),
                        Span::raw(mfr.clone()),
                    ]));
                }
                lines.push(Line::from(vec![
                    Span::styled("  Android:  ", theme::hint()),
                    Span::raw(info.android_display()),
                ]));
                if let Some(cpu) = &info.cpu_abi {
                    lines.push(Line::from(vec![
                        Span::styled("  CPU:      ", theme::hint()),
                        Span::raw(cpu.clone()),
                    ]));
                }
                if let Some(ram) = info.ram_bytes {
                    lines.push(Line::from(vec![
                        Span::styled("  RAM:      ", theme::hint()),
                        Span::raw(format!("{:.1} GB", ram as f64 / 1_073_741_824.0)),
                    ]));
                }
                if let Some(pv) = &info.perfetto_version {
                    lines.push(Line::from(vec![
                        Span::styled("  Perfetto: ", theme::hint()),
                        Span::raw(pv.clone()),
                    ]));
                }
                lines.push(Line::from(vec![
                    Span::styled("  Serial:   ", theme::hint()),
                    Span::styled(info.serial.clone(), theme::hint()),
                ]));
                frame.render_widget(Paragraph::new(lines).block(block), area);
            }
            None => {
                let msg = if self.device_info_serial.is_some() {
                    "  ⏳ loading…"
                } else {
                    "  Select an online device"
                };
                frame.render_widget(
                    Paragraph::new(Span::styled(msg, theme::hint())).block(block),
                    area,
                );
            }
        }
    }

    fn render_suggestions(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Suggestions;
        let device_label = self
            .device_packages_source
            .as_deref()
            .unwrap_or("device");
        let title = if self.loading_packages {
            format!(" Suggestions — loading packages from {device_label}… ")
        } else if self.device_packages.is_empty() && self.recent_packages.is_empty() {
            " Suggestions ".to_string()
        } else {
            format!(
                " Suggestions ({} recent, {} on {}) ",
                self.recent_packages.len(),
                self.device_packages.len(),
                device_label,
            )
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(focus_style(focused));

        if self.suggestions.is_empty() {
            let msg = if self.loading_packages {
                "  ⏳ querying installed packages…"
            } else if self.package.trim().is_empty() {
                "  No suggestions yet — type in the package field or pick a device."
            } else {
                "  No matches. Keep typing to set a new package name."
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(msg, theme::hint()))).block(block),
                area,
            );
            return;
        }

        let items: Vec<ListItem> = self
            .suggestions
            .iter()
            .map(|pkg| {
                let from_device = self.device_packages.iter().any(|p| p == pkg);
                let tag: String = if from_device {
                    device_label.to_string()
                } else {
                    "recent".to_string()
                };
                ListItem::new(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        pkg.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(format!("[{tag}]"), theme::hint()),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().bg(theme::accent()).fg(Color::Black))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.suggestions_state);
    }

    fn render_config_picker(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Config;
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Config ")
            .border_style(focus_style(focused));

        // Build items: "Default" first, then saved configs
        let mut items: Vec<ListItem> = vec![ListItem::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "Default",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("built-in defaults", theme::hint()),
        ]))];

        for c in &self.saved_configs {
            let cats = c.config.atrace_categories.len();
            items.push(ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    c.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{}ms  {}KB  {} cats", c.config.duration_ms, c.config.buffer_size_kb, cats),
                    theme::hint(),
                ),
            ])));
        }

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().bg(theme::accent()).fg(Color::Black))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.config_state);
    }

    fn render_command_set_picker(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Commands;
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Startup Commands ")
            .border_style(focus_style(focused));

        let mut items: Vec<ListItem> = vec![ListItem::new(Line::from(vec![
            Span::raw("  "),
            Span::styled("None", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("no startup commands", theme::hint()),
        ]))];

        for s in &self.saved_command_sets {
            let count = s.commands.len();
            items.push(ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    s.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(format!("{count} cmd(s)"), theme::hint()),
            ])));
        }

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().bg(theme::accent()).fg(Color::Black))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.command_set_state);
    }

    fn render_text_field(
        &self,
        frame: &mut Frame,
        area: Rect,
        title: &str,
        value: &str,
        focused: bool,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "))
            .border_style(focus_style(focused));
        let content = if focused {
            format!(" {value}█")
        } else if value.is_empty() {
            String::from(" ")
        } else {
            format!(" {value}")
        };
        frame.render_widget(Paragraph::new(content).block(block), area);
    }
}

enum TextField {
    Name,
    Package,
}

fn focus_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(theme::accent())
    } else {
        Style::default().fg(theme::dim())
    }
}

fn device_display_name(entry: &DeviceEntry) -> String {
    entry
        .nickname
        .clone()
        .or_else(|| entry.model.clone())
        .unwrap_or_else(|| entry.serial.clone())
}
