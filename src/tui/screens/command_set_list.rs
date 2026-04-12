use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::db::Database;
use crate::db::command_sets::SavedCommandSet;
use crate::perfetto::commands::StartupCommand;
use crate::tui::chrome;
use crate::tui::text_input::{self, TextAction};
use crate::tui::theme;

enum Mode {
    Browse,
    Naming { buffer: String },
    ConfirmDelete,
}

pub struct CommandSetListScreen {
    sets: Vec<SavedCommandSet>,
    list_state: ListState,
    mode: Mode,
    error: Option<String>,
    db: Database,
}

pub enum CommandSetListAction {
    None,
    Back,
    Edit(i64, String, Vec<StartupCommand>),
    CreateNew(String),
}

impl CommandSetListScreen {
    pub fn new(db: Database) -> Self {
        let mut screen = Self {
            sets: Vec::new(),
            list_state: ListState::default(),
            mode: Mode::Browse,
            error: None,
            db,
        };
        screen.reload();
        screen
    }

    pub fn reload(&mut self) {
        match self.db.list_command_sets() {
            Ok(list) => {
                self.sets = list;
                self.error = None;
                if self.sets.is_empty() {
                    self.list_state.select(None);
                } else {
                    let idx = self
                        .list_state
                        .selected()
                        .unwrap_or(0)
                        .min(self.sets.len() - 1);
                    self.list_state.select(Some(idx));
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> CommandSetListAction {
        if key.kind != KeyEventKind::Press {
            return CommandSetListAction::None;
        }

        match &mut self.mode {
            Mode::Naming { .. } => return self.handle_naming_key(key),
            Mode::ConfirmDelete => return self.handle_confirm_delete(key),
            Mode::Browse => {}
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => CommandSetListAction::Back,
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                CommandSetListAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                CommandSetListAction::None
            }
            KeyCode::Char('n') => {
                self.mode = Mode::Naming {
                    buffer: String::new(),
                };
                CommandSetListAction::None
            }
            KeyCode::Enter => {
                if let Some(set) = self.selected().cloned() {
                    CommandSetListAction::Edit(set.id, set.name, set.commands)
                } else {
                    CommandSetListAction::None
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if self.selected().is_some() {
                    self.mode = Mode::ConfirmDelete;
                }
                CommandSetListAction::None
            }
            _ => CommandSetListAction::None,
        }
    }

    fn handle_naming_key(&mut self, key: KeyEvent) -> CommandSetListAction {
        let Mode::Naming { mut buffer } = std::mem::replace(&mut self.mode, Mode::Browse) else {
            return CommandSetListAction::None;
        };
        match text_input::apply(&mut buffer, &key) {
            TextAction::Cancel => CommandSetListAction::None,
            TextAction::Submit => {
                let name = buffer.trim().to_string();
                if name.is_empty() {
                    self.error = Some("Name cannot be empty".into());
                    CommandSetListAction::None
                } else {
                    CommandSetListAction::CreateNew(name)
                }
            }
            TextAction::Edited | TextAction::Ignored => {
                self.mode = Mode::Naming { buffer };
                CommandSetListAction::None
            }
        }
    }

    fn handle_confirm_delete(&mut self, key: KeyEvent) -> CommandSetListAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(set) = self.selected().cloned() {
                    if let Err(e) = self.db.delete_command_set(set.id) {
                        self.error = Some(format!("delete failed: {e}"));
                    }
                    self.reload();
                }
                self.mode = Mode::Browse;
            }
            _ => self.mode = Mode::Browse,
        }
        CommandSetListAction::None
    }

    fn move_selection(&mut self, delta: i32) {
        if self.sets.is_empty() {
            return;
        }
        let len = self.sets.len() as i32;
        let cur = self.list_state.selected().unwrap_or(0) as i32;
        self.list_state
            .select(Some((cur + delta).rem_euclid(len) as usize));
    }

    fn selected(&self) -> Option<&SavedCommandSet> {
        self.list_state.selected().and_then(|i| self.sets.get(i))
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
            Span::styled("  🚀 Startup Commands ", theme::title()),
            Span::styled("— reusable command sets for ui.perfetto.dev", theme::hint()),
        ]));
        frame.render_widget(header, chunks[0]);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Command Sets ");

        if let Some(err) = &self.error {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("  ✗ {err}"),
                    Style::default().fg(theme::ERR),
                ))
                .block(block),
                chunks[1],
            );
        } else if self.sets.is_empty() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from("  No command sets yet."),
                    Line::from(""),
                    Line::from(Span::styled(
                        "  Press [n] to create one.",
                        theme::hint(),
                    )),
                ])
                .block(block),
                chunks[1],
            );
        } else {
            let items: Vec<ListItem> = self
                .sets
                .iter()
                .map(|s| {
                    let count = s.commands.len();
                    let summary = s
                        .commands
                        .iter()
                        .take(3)
                        .map(|c| {
                            c.id.strip_prefix("dev.perfetto.")
                                .unwrap_or(&c.id)
                                .to_string()
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let ellipsis = if count > 3 { ", …" } else { "" };
                    ListItem::new(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            s.name.clone(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                        Span::styled(
                            format!("{count} cmd(s): {summary}{ellipsis}"),
                            theme::hint(),
                        ),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .block(block)
                .highlight_style(Style::default().bg(theme::ACCENT).fg(Color::Black))
                .highlight_symbol("▶ ");
            frame.render_stateful_widget(list, chunks[1], &mut self.list_state);
        }

        let footer = match &self.mode {
            Mode::Naming { buffer } => Line::from(vec![
                Span::styled(" name › ", theme::title()),
                Span::raw(buffer.clone()),
                Span::styled("█", Style::default().fg(theme::ACCENT)),
                Span::styled("   [Enter] create  [Esc] cancel", theme::hint()),
            ]),
            Mode::ConfirmDelete => {
                let name = self.selected().map(|s| s.name.as_str()).unwrap_or("?");
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
            Mode::Browse => Line::from(vec![
                Span::styled(" [n]", theme::title()),
                Span::raw(" new  "),
                Span::styled("[Enter]", theme::title()),
                Span::raw(" edit  "),
                Span::styled("[x]", theme::title()),
                Span::raw(" delete  "),
                Span::styled("[Esc]", theme::title()),
                Span::raw(" back"),
            ]),
        };
        frame.render_widget(Paragraph::new(footer), chunks[2]);
    }
}
