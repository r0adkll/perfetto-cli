use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use tokio::sync::mpsc::UnboundedSender;

use crate::adb;
use crate::config::Paths;
use crate::db::Database;
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
    Submit,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Name => Focus::Package,
            Focus::Package => Focus::Suggestions,
            Focus::Suggestions => Focus::Device,
            Focus::Device => Focus::Submit,
            Focus::Submit => Focus::Name,
        }
    }
    fn prev(self) -> Self {
        match self {
            Focus::Name => Focus::Submit,
            Focus::Package => Focus::Name,
            Focus::Suggestions => Focus::Package,
            Focus::Device => Focus::Suggestions,
            Focus::Submit => Focus::Device,
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
    /// Filtered view over `recent_packages` + `device_packages`, rebuilt on
    /// every keystroke in the package field.
    suggestions: Vec<String>,
    suggestions_state: ListState,
    loading_packages: bool,
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
            suggestions: Vec::new(),
            suggestions_state: ListState::default(),
            loading_packages: false,
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
        // Remember the display name of the device we're about to query so
        // suggestion tags (and the panel title) can show something like
        // `[pixel-7]` instead of a generic `[device]` label.
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
                WizardAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_device(-1);
                WizardAction::None
            }
            KeyCode::Char('r') => {
                self.spawn_device_load();
                WizardAction::None
            }
            KeyCode::Enter => {
                self.focus = Focus::Submit;
                WizardAction::None
            }
            _ => WizardAction::None,
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
            config: TraceConfig::default(),
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
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(6),
                Constraint::Min(5),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);

        let header = chrome::app_header(Line::from(vec![
            Span::styled("  ✨ New session ", theme::title()),
            Span::styled("— Tab to move between fields, Esc to cancel", theme::hint()),
        ]));
        frame.render_widget(header, chunks[0]);

        self.render_text_field(
            frame,
            chunks[1],
            "Session name",
            &self.name,
            self.focus == Focus::Name,
        );
        self.render_text_field(
            frame,
            chunks[2],
            "Package name (e.g. com.example.myapp)",
            &self.package,
            self.focus == Focus::Package,
        );

        self.render_suggestions(frame, chunks[3]);

        let device_block = Block::default()
            .borders(Borders::ALL)
            .title(" Device ")
            .border_style(focus_style(self.focus == Focus::Device));

        if self.loading_devices && self.devices.is_empty() {
            frame.render_widget(
                Paragraph::new("  ⏳ listing devices…").block(device_block),
                chunks[4],
            );
        } else if self.devices.is_empty() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from("  No devices found."),
                    Line::from(Span::styled(
                        "  Press [r] to retry or Esc to cancel.",
                        theme::hint(),
                    )),
                ])
                .block(device_block),
                chunks[4],
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
                        EntryState::Online => "● online",
                        EntryState::Offline => "○ offline",
                        EntryState::Unauthorized => "⚠ unauthorized",
                        EntryState::Other(_) => "? other",
                        EntryState::NotConnected => "· remembered",
                    };
                    ListItem::new(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("{state_str:<14}"), theme::hint()),
                        Span::raw("  "),
                        Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
                        Span::raw("  "),
                        Span::styled(format!("({})", e.serial), theme::hint()),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .block(device_block)
                .highlight_style(Style::default().bg(theme::ACCENT).fg(Color::Black))
                .highlight_symbol("▶ ");
            frame.render_stateful_widget(list, chunks[4], &mut self.device_state);
        }

        let submit_text = if self.focus == Focus::Submit {
            " ▶ Create session (Enter) "
        } else {
            "   Create session "
        };
        let submit = Paragraph::new(Line::from(Span::styled(
            submit_text,
            if self.focus == Focus::Submit {
                Style::default()
                    .bg(theme::ACCENT)
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
        frame.render_widget(submit, chunks[5]);

        let footer = match &self.error {
            Some(msg) => Line::from(Span::styled(
                format!(" ✗ {msg}"),
                Style::default().fg(theme::ERR),
            )),
            None => Line::from(Span::styled(
                " Tab/Shift+Tab to move focus  •  Enter advances  •  Esc cancels",
                theme::hint(),
            )),
        };
        frame.render_widget(Paragraph::new(footer), chunks[6]);
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
            .highlight_style(Style::default().bg(theme::ACCENT).fg(Color::Black))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.suggestions_state);
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
        Style::default().fg(theme::ACCENT)
    } else {
        Style::default().fg(theme::DIM)
    }
}

fn device_display_name(entry: &DeviceEntry) -> String {
    entry
        .nickname
        .clone()
        .or_else(|| entry.model.clone())
        .unwrap_or_else(|| entry.serial.clone())
}
