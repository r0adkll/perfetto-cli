use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use tokio::sync::mpsc::UnboundedSender;

use crate::cloud::{self, CloudProvider};
use crate::db::Database;
use crate::tui::chrome;
use crate::tui::event::AppEvent;
use crate::tui::text_input::{self, TextAction};
use crate::tui::theme;

pub enum ProviderAction {
    None,
    Back,
}

enum Mode {
    Browse,
    EditFolder { buffer: String },
    ConfirmLogout,
}

struct ProviderEntry {
    provider: Arc<dyn CloudProvider>,
    /// `None` while the async auth check is in flight.
    authenticated: Option<bool>,
    is_default: bool,
    folder: String,
}

pub struct CloudProvidersScreen {
    providers: Vec<ProviderEntry>,
    list_state: ListState,
    mode: Mode,
    error: Option<String>,
    status: theme::Status,
    db: Database,
    tx: UnboundedSender<AppEvent>,
}

impl CloudProvidersScreen {
    pub fn new(db: Database, tx: UnboundedSender<AppEvent>) -> Self {
        let default_id = cloud::default_provider_id(&db);
        let all = cloud::all_providers();
        let providers: Vec<ProviderEntry> = all
            .into_iter()
            .map(|p| {
                let is_default = p.id() == default_id;
                let folder = p.upload_folder(&db);
                ProviderEntry {
                    provider: p,
                    authenticated: None,
                    is_default,
                    folder,
                }
            })
            .collect();

        let mut list_state = ListState::default();
        if !providers.is_empty() {
            list_state.select(Some(0));
        }

        let screen = Self {
            providers,
            list_state,
            mode: Mode::Browse,
            error: None,
            status: theme::Status::default(),
            db,
            tx,
        };
        screen.spawn_auth_checks();
        screen
    }

    /// Spawn async tasks to check auth status for each provider.
    fn spawn_auth_checks(&self) {
        for entry in &self.providers {
            let provider = entry.provider.clone();
            let db = self.db.clone();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let authed = provider.is_authenticated(&db).await;
                let _ = tx.send(AppEvent::CloudProviderStatus {
                    provider_id: provider.id().to_string(),
                    authenticated: authed,
                });
            });
        }
    }

    /// Handle the async auth status result.
    pub fn on_provider_status(&mut self, provider_id: &str, authenticated: bool) {
        if let Some(entry) = self.providers.iter_mut().find(|e| e.provider.id() == provider_id) {
            entry.authenticated = Some(authenticated);
        }
    }

    /// Handle auth result (login success/failure).
    pub fn on_auth_result(&mut self, result: Result<String, String>) {
        match result {
            Ok(provider_id) => {
                if let Some(entry) = self.providers.iter_mut().find(|e| e.provider.id() == provider_id) {
                    entry.authenticated = Some(true);
                }
                self.status.set("logged in".into());
                self.error = None;
            }
            Err(msg) => {
                self.error = Some(format!("login failed: {msg}"));
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> ProviderAction {
        if key.kind != KeyEventKind::Press {
            return ProviderAction::None;
        }
        match &mut self.mode {
            Mode::EditFolder { .. } => return self.handle_edit_folder(key),
            Mode::ConfirmLogout => return self.handle_confirm_logout(key),
            Mode::Browse => {}
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ProviderAction::Back,
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                ProviderAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                ProviderAction::None
            }
            KeyCode::Char('l') => {
                self.login_selected();
                ProviderAction::None
            }
            KeyCode::Char('o') => {
                if self.selected_entry().is_some_and(|e| e.authenticated == Some(true)) {
                    self.mode = Mode::ConfirmLogout;
                }
                ProviderAction::None
            }
            KeyCode::Char('d') => {
                self.set_default();
                ProviderAction::None
            }
            KeyCode::Char('f') => {
                if let Some(entry) = self.selected_entry() {
                    let buffer = entry.folder.clone();
                    self.mode = Mode::EditFolder { buffer };
                }
                ProviderAction::None
            }
            _ => ProviderAction::None,
        }
    }

    fn handle_edit_folder(&mut self, key: KeyEvent) -> ProviderAction {
        let Mode::EditFolder { mut buffer } = std::mem::replace(&mut self.mode, Mode::Browse) else {
            return ProviderAction::None;
        };
        match text_input::apply(&mut buffer, &key) {
            TextAction::Cancel => {}
            TextAction::Submit => {
                let trimmed = buffer.trim().to_string();
                if let Some(idx) = self.list_state.selected() {
                    let settings_key = self.providers[idx].provider.folder_settings_key();
                    if trimmed.is_empty() {
                        let _ = self.db.delete_setting(&settings_key);
                        let new_folder = self.providers[idx].provider.upload_folder(&self.db);
                        self.providers[idx].folder = new_folder;
                    } else {
                        let _ = self.db.set_setting(&settings_key, &trimmed);
                        self.providers[idx].folder = trimmed;
                    }
                    self.status.set("folder updated".into());
                    self.error = None;
                }
            }
            TextAction::Edited | TextAction::Ignored => {
                self.mode = Mode::EditFolder { buffer };
            }
        }
        ProviderAction::None
    }

    fn handle_confirm_logout(&mut self, key: KeyEvent) -> ProviderAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(entry) = self.selected_entry() {
                    let provider = entry.provider.clone();
                    let db = self.db.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        match provider.logout(&db).await {
                            Ok(()) => {
                                let _ = tx.send(AppEvent::CloudProviderStatus {
                                    provider_id: provider.id().to_string(),
                                    authenticated: false,
                                });
                            }
                            Err(e) => {
                                tracing::error!(?e, "logout failed");
                            }
                        }
                    });
                    self.status.set("logged out".into());
                    self.error = None;
                }
                self.mode = Mode::Browse;
            }
            _ => self.mode = Mode::Browse,
        }
        ProviderAction::None
    }

    fn login_selected(&mut self) {
        let Some(entry) = self.selected_entry() else { return };
        if entry.authenticated == Some(true) {
            // Already logged in.
            return;
        }
        let provider = entry.provider.clone();
        let db = self.db.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = provider.authenticate(&db).await;
            let _ = tx.send(AppEvent::CloudAuthResult(
                result.map(|()| provider.id().to_string()).map_err(|e| format!("{e:#}")),
            ));
        });
    }

    fn set_default(&mut self) {
        let Some(idx) = self.list_state.selected() else { return };
        let id = self.providers[idx].provider.id().to_string();
        let _ = self.db.set_setting("cloud.default_provider", &id);
        for (i, entry) in self.providers.iter_mut().enumerate() {
            entry.is_default = i == idx;
        }
        self.status.set("default provider updated".into());
        self.error = None;
    }

    fn move_selection(&mut self, delta: i32) {
        if self.providers.is_empty() {
            return;
        }
        let len = self.providers.len() as i32;
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.list_state.select(Some(next as usize));
    }

    fn selected_entry(&self) -> Option<&ProviderEntry> {
        self.list_state.selected().and_then(|i| self.providers.get(i))
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
            Span::styled("  Cloud Providers", theme::title()),
        ]));
        frame.render_widget(header, outer[0]);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Providers ");

        if self.providers.is_empty() {
            frame.render_widget(
                Paragraph::new("  No providers registered.")
                    .block(block),
                outer[1],
            );
        } else {
            let items: Vec<ListItem> = self
                .providers
                .iter()
                .map(|entry| {
                    let name = entry.provider.name();
                    let auth_indicator = match entry.authenticated {
                        Some(true) => Span::styled("● ", Style::default().fg(theme::ok())),
                        Some(false) => Span::styled("○ ", theme::hint()),
                        None => Span::styled("… ", theme::hint()),
                    };
                    let auth_label = match entry.authenticated {
                        Some(true) => Span::styled("logged in", Style::default().fg(theme::ok())),
                        Some(false) => Span::styled("not connected", theme::hint()),
                        None => Span::styled("checking…", theme::hint()),
                    };
                    let mut spans = vec![
                        Span::raw("  "),
                        Span::styled(
                            name.to_string(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("     "),
                        auth_indicator,
                        auth_label,
                    ];
                    if entry.is_default {
                        spans.push(Span::styled(
                            "   ★ default",
                            Style::default().fg(theme::accent()),
                        ));
                    }
                    spans.push(Span::raw("   "));
                    spans.push(Span::styled(
                        format!("folder: {}", entry.folder),
                        theme::hint(),
                    ));
                    ListItem::new(Line::from(spans))
                })
                .collect();

            let list = List::new(items)
                .block(block)
                .highlight_style(Style::default().bg(theme::accent()).fg(Color::Black))
                .highlight_symbol("▶ ");
            frame.render_stateful_widget(list, outer[1], &mut self.list_state);
        }

        let footer = match &self.mode {
            Mode::EditFolder { buffer } => Line::from(vec![
                Span::styled(" folder › ", theme::title()),
                Span::raw(buffer.clone()),
                Span::styled("█", Style::default().fg(theme::accent())),
                Span::styled(
                    "   [Enter] save  [Esc] cancel  [Alt-⌫] word  [Ctrl-U] clear",
                    theme::hint(),
                ),
            ]),
            Mode::ConfirmLogout => {
                let name = self
                    .selected_entry()
                    .map(|e| e.provider.name())
                    .unwrap_or("provider");
                Line::from(vec![
                    Span::styled(
                        format!(" Log out of {name}? "),
                        Style::default().fg(theme::warn()),
                    ),
                    Span::styled("[y]", theme::title()),
                    Span::raw(" yes  "),
                    Span::styled("[n]", theme::title()),
                    Span::raw(" cancel"),
                ])
            }
            Mode::Browse => {
                if let Some(msg) = self.status.get() {
                    Line::from(Span::styled(
                        format!(" ✓ {msg}"),
                        Style::default().fg(theme::ok()),
                    ))
                } else if let Some(err) = &self.error {
                    Line::from(Span::styled(
                        format!(" ✗ {err}"),
                        Style::default().fg(theme::err()),
                    ))
                } else {
                    let is_authed = self
                        .selected_entry()
                        .is_some_and(|e| e.authenticated == Some(true));
                    let mut spans = vec![
                        Span::styled(" [l]", theme::title()),
                        Span::raw(" login  "),
                    ];
                    if is_authed {
                        spans.push(Span::styled("[o]", theme::title()));
                        spans.push(Span::raw(" logout  "));
                    }
                    spans.extend([
                        Span::styled("[d]", theme::title()),
                        Span::raw(" default  "),
                        Span::styled("[f]", theme::title()),
                        Span::raw(" folder  "),
                        Span::styled("[Esc]", theme::title()),
                        Span::raw(" back"),
                    ]);
                    Line::from(spans)
                }
            }
        };
        frame.render_widget(Paragraph::new(footer), outer[2]);
    }
}
