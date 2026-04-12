use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::db::Database;
use crate::db::configs::SavedConfig;
use crate::perfetto::{TraceConfig, textproto};
use crate::tui::chrome;
use crate::tui::text_input::{self, TextAction};
use crate::tui::theme;

enum Mode {
    Browse,
    Naming { buffer: String },
    ConfirmDelete,
}

pub struct ConfigListScreen {
    configs: Vec<SavedConfig>,
    list_state: ListState,
    mode: Mode,
    error: Option<String>,
    status: theme::Status,
    db: Database,
}

pub enum ConfigListAction {
    None,
    Back,
    Edit(i64, String, TraceConfig),
    CreateNew(String),
    Import,
}

impl ConfigListScreen {
    pub fn new(db: Database) -> Self {
        let mut screen = Self {
            configs: Vec::new(),
            list_state: ListState::default(),
            mode: Mode::Browse,
            error: None,
            status: theme::Status::default(),
            db,
        };
        screen.reload();
        screen
    }

    pub fn reload(&mut self) {
        match self.db.list_configs() {
            Ok(list) => {
                self.configs = list;
                self.error = None;
                if self.configs.is_empty() {
                    self.list_state.select(None);
                } else {
                    let idx = self
                        .list_state
                        .selected()
                        .unwrap_or(0)
                        .min(self.configs.len() - 1);
                    self.list_state.select(Some(idx));
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> ConfigListAction {
        if key.kind != KeyEventKind::Press {
            return ConfigListAction::None;
        }

        match &mut self.mode {
            Mode::Naming { .. } => return self.handle_naming_key(key),
            Mode::ConfirmDelete => return self.handle_confirm_delete(key),
            Mode::Browse => {}
        }

        // Ctrl-I: import a textproto config
        if matches!(key.code, KeyCode::Char('i'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return ConfigListAction::Import;
        }

        // Ctrl-E: export selected config's textproto to clipboard
        if matches!(key.code, KeyCode::Char('e'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            if let Some(cfg) = self.selected_config() {
                let proto = textproto::build(&cfg.config);
                match cli_clipboard::set_contents(proto) {
                    Ok(_) => self.status.set("Textproto copied to clipboard".into()),
                    Err(e) => self.error = Some(format!("clipboard: {e}")),
                }
            }
            return ConfigListAction::None;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ConfigListAction::Back,
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                ConfigListAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                ConfigListAction::None
            }
            KeyCode::Char('n') => {
                self.mode = Mode::Naming {
                    buffer: String::new(),
                };
                ConfigListAction::None
            }
            KeyCode::Enter => {
                if let Some(cfg) = self.selected_config() {
                    ConfigListAction::Edit(cfg.id, cfg.name.clone(), cfg.config.clone())
                } else {
                    ConfigListAction::None
                }
            }
            KeyCode::Char('d') => {
                if let Some(cfg) = self.selected_config() {
                    let new_name = format!("{} (copy)", cfg.name);
                    let id = cfg.id;
                    match self.db.duplicate_config(id, &new_name) {
                        Ok(_) => self.reload(),
                        Err(e) => self.error = Some(format!("duplicate failed: {e}")),
                    }
                }
                ConfigListAction::None
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if self.selected_config().is_some() {
                    self.mode = Mode::ConfirmDelete;
                }
                ConfigListAction::None
            }
            _ => ConfigListAction::None,
        }
    }

    fn handle_naming_key(&mut self, key: KeyEvent) -> ConfigListAction {
        let Mode::Naming { mut buffer } = std::mem::replace(&mut self.mode, Mode::Browse) else {
            return ConfigListAction::None;
        };
        match text_input::apply(&mut buffer, &key) {
            TextAction::Cancel => ConfigListAction::None,
            TextAction::Submit => {
                let name = buffer.trim().to_string();
                if name.is_empty() {
                    self.error = Some("Name cannot be empty".into());
                    ConfigListAction::None
                } else {
                    ConfigListAction::CreateNew(name)
                }
            }
            TextAction::Edited | TextAction::Ignored => {
                self.mode = Mode::Naming { buffer };
                ConfigListAction::None
            }
        }
    }

    fn handle_confirm_delete(&mut self, key: KeyEvent) -> ConfigListAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(cfg) = self.selected_config().cloned() {
                    if let Err(e) = self.db.delete_config(cfg.id) {
                        self.error = Some(format!("delete failed: {e}"));
                    }
                    self.reload();
                }
                self.mode = Mode::Browse;
            }
            _ => self.mode = Mode::Browse,
        }
        ConfigListAction::None
    }

    fn move_selection(&mut self, delta: i32) {
        if self.configs.is_empty() {
            return;
        }
        let len = self.configs.len() as i32;
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.list_state.select(Some(next as usize));
    }

    fn selected_config(&self) -> Option<&SavedConfig> {
        self.list_state
            .selected()
            .and_then(|i| self.configs.get(i))
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        let header = chrome::app_header(Line::from(vec![
            Span::styled("  ⚙  Configurations ", theme::title()),
            Span::styled("— reusable trace configs for new sessions", theme::hint()),
        ]));
        frame.render_widget(header, chunks[0]);

        let body_block = Block::default()
            .borders(Borders::ALL)
            .title(" Saved Configs ");

        if let Some(err) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("  ✗ {err}"),
                    Style::default().fg(theme::ERR),
                )))
                .block(body_block),
                chunks[1],
            );
        } else if self.configs.is_empty() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from("  No saved configurations yet."),
                    Line::from(""),
                    Line::from(Span::styled(
                        "  Press [n] to create one.",
                        theme::hint(),
                    )),
                ])
                .block(body_block),
                chunks[1],
            );
        } else {
            let items: Vec<ListItem> = self
                .configs
                .iter()
                .map(|c| {
                    let cats = c.config.atrace_categories.len();
                    let summary = format!(
                        "{}ms  {}KB  {} categories",
                        c.config.duration_ms,
                        c.config.buffer_size_kb,
                        cats,
                    );
                    ListItem::new(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            c.name.clone(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                        Span::styled(summary, theme::hint()),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .block(body_block)
                .highlight_style(Style::default().bg(theme::ACCENT).fg(Color::Black))
                .highlight_symbol("▶ ");
            frame.render_stateful_widget(list, chunks[1], &mut self.list_state);
        }

        let footer = match &self.mode {
            Mode::Naming { buffer } => Line::from(vec![
                Span::styled(" config name › ", theme::title()),
                Span::raw(buffer.clone()),
                Span::styled("█", Style::default().fg(theme::ACCENT)),
                Span::styled("   [Enter] create  [Esc] cancel", theme::hint()),
            ]),
            Mode::ConfirmDelete => {
                let name = self
                    .selected_config()
                    .map(|c| c.name.as_str())
                    .unwrap_or("?");
                Line::from(vec![
                    Span::styled(
                        format!(" ⚠ delete \"{name}\"? "),
                        Style::default().fg(theme::WARN),
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
                        Style::default().fg(theme::OK),
                    ))
                } else {
                    Line::from(vec![
                        Span::styled(" [n]", theme::title()),
                        Span::raw(" new  "),
                        Span::styled("[Enter]", theme::title()),
                        Span::raw(" edit  "),
                        Span::styled("[d]", theme::title()),
                        Span::raw(" duplicate  "),
                        Span::styled("[Ctrl-I]", theme::title()),
                        Span::raw(" import  "),
                        Span::styled("[Ctrl-E]", theme::title()),
                        Span::raw(" export  "),
                        Span::styled("[x]", theme::title()),
                        Span::raw(" delete  "),
                        Span::styled("[Esc]", theme::title()),
                        Span::raw(" back"),
                    ])
                }
            }
        };
        frame.render_widget(Paragraph::new(footer), chunks[2]);
    }
}
