use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::db::Database;
use crate::session::Session;
use crate::tui::chrome;
use crate::tui::theme;

pub struct SessionsListScreen {
    sessions: Vec<Session>,
    list_state: ListState,
    confirming_delete: bool,
    error: Option<String>,
}

pub enum SessionsAction {
    None,
    Quit,
    OpenConfigList,
    OpenCommandSets,
    OpenThemePicker,
    OpenCloudProviders,
    NewSession,
    OpenSession(i64),
}

impl SessionsListScreen {
    pub fn new(db: &Database) -> Self {
        let mut screen = Self {
            sessions: Vec::new(),
            list_state: ListState::default(),
            confirming_delete: false,
            error: None,
        };
        screen.reload(db);
        screen
    }

    pub fn reload(&mut self, db: &Database) {
        match db.list_sessions() {
            Ok(list) => {
                self.sessions = list;
                self.error = None;
                if self.sessions.is_empty() {
                    self.list_state.select(None);
                } else {
                    let current = self.list_state.selected().unwrap_or(0);
                    self.list_state
                        .select(Some(current.min(self.sessions.len() - 1)));
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn on_key(&mut self, db: &Database, key: KeyEvent) -> SessionsAction {
        if key.kind != KeyEventKind::Press {
            return SessionsAction::None;
        }

        if self.confirming_delete {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    if let Some(s) = self.selected_session().cloned() {
                        if let Err(e) = delete_session(db, &s) {
                            self.error = Some(format!("delete failed: {e}"));
                        }
                        self.reload(db);
                    }
                    self.confirming_delete = false;
                }
                _ => self.confirming_delete = false,
            }
            return SessionsAction::None;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => SessionsAction::Quit,
            KeyCode::Char('g') => SessionsAction::OpenConfigList,
            KeyCode::Char('s') => SessionsAction::OpenCommandSets,
            KeyCode::Char('t') => SessionsAction::OpenThemePicker,
            KeyCode::Char('p') => SessionsAction::OpenCloudProviders,
            KeyCode::Char('n') => SessionsAction::NewSession,
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                SessionsAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                SessionsAction::None
            }
            KeyCode::Enter => {
                if let Some(s) = self.selected_session() {
                    if let Some(id) = s.id {
                        return SessionsAction::OpenSession(id);
                    }
                }
                SessionsAction::None
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if self.selected_session().is_some() {
                    self.confirming_delete = true;
                }
                SessionsAction::None
            }
            _ => SessionsAction::None,
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.sessions.is_empty() {
            return;
        }
        let len = self.sessions.len() as i32;
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.list_state.select(Some(next as usize));
    }

    fn selected_session(&self) -> Option<&Session> {
        self.list_state.selected().and_then(|i| self.sessions.get(i))
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

        let header = chrome::app_header(Line::from(Span::styled(
            "  Android trace session manager",
            theme::hint(),
        )));
        frame.render_widget(header, chunks[0]);

        let body_block = Block::default().borders(Borders::ALL).title(" Sessions ");

        if let Some(err) = &self.error {
            let p = Paragraph::new(Line::from(Span::styled(
                format!("  ✗ {err}"),
                Style::default().fg(theme::err()),
            )))
            .block(body_block);
            frame.render_widget(p, chunks[1]);
        } else if self.sessions.is_empty() {
            // Vertically center the banner by prepending blank lines. ratatui
            // doesn't have a vertical-alignment knob on `Paragraph`, so we
            // compute how much top padding fits inside the bordered block
            // (area height minus the two border rows) and push the banner
            // content down by half the slack.
            let banner = chrome::home_banner();
            let inner_height = chunks[1].height.saturating_sub(2) as usize;
            let top_pad = inner_height.saturating_sub(banner.len()) / 2;
            let mut lines: Vec<Line<'static>> = Vec::with_capacity(top_pad + banner.len());
            for _ in 0..top_pad {
                lines.push(Line::from(""));
            }
            lines.extend(banner);

            let p = Paragraph::new(lines)
                .alignment(Alignment::Center)
                .block(body_block);
            frame.render_widget(p, chunks[1]);
        } else {
            let items: Vec<ListItem> = self
                .sessions
                .iter()
                .map(|s| {
                    let date = s.created_at.format("%Y-%m-%d %H:%M").to_string();
                    let device = s
                        .device_serial
                        .as_deref()
                        .unwrap_or("(no device)")
                        .to_string();
                    let mut spans = vec![
                        Span::raw("  "),
                        Span::styled(
                            s.name.clone(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                    ];
                    if s.is_imported {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            "[imported]",
                            Style::default()
                                .fg(theme::accent_secondary())
                                .add_modifier(Modifier::DIM),
                        ));
                    }
                    spans.extend([
                        Span::raw("  "),
                        Span::styled(format!("({})", s.package_name), theme::hint()),
                        Span::raw("  "),
                        Span::styled(date, theme::hint()),
                        Span::raw("  "),
                        Span::styled(format!("[{device}]"), theme::hint()),
                    ]);
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let list = List::new(items)
                .block(body_block)
                .highlight_style(Style::default().bg(theme::accent()).fg(Color::Black))
                .highlight_symbol("▶ ");
            frame.render_stateful_widget(list, chunks[1], &mut self.list_state);
        }

        let footer = if self.confirming_delete {
            let name = self
                .selected_session()
                .map(|s| s.name.as_str())
                .unwrap_or("?");
            Line::from(vec![
                Span::styled(
                    format!(" ⚠ delete \"{name}\" and its folder? "),
                    Style::default().fg(theme::warn()),
                ),
                Span::styled("[y]", theme::title()),
                Span::raw(" yes  "),
                Span::styled("[n]", theme::title()),
                Span::raw(" cancel"),
            ])
        } else {
            Line::from(vec![
                Span::styled(" [q]", theme::title()),
                Span::raw(" quit  "),
                Span::styled("[n]", theme::title()),
                Span::raw(" new  "),
                Span::styled("[Enter]", theme::title()),
                Span::raw(" open  "),
                Span::styled("[x]", theme::title()),
                Span::raw(" delete  "),
                Span::styled("[g]", theme::title()),
                Span::raw(" configs  "),
                Span::styled("[s]", theme::title()),
                Span::raw(" commands  "),
                Span::styled("[t]", theme::title()),
                Span::raw(" theme  "),
                Span::styled("[p]", theme::title()),
                Span::raw(" providers"),
            ])
        };
        frame.render_widget(Paragraph::new(footer), chunks[2]);
    }
}

fn delete_session(db: &Database, session: &Session) -> Result<()> {
    if let Some(id) = session.id {
        db.delete_session(id)?;
    }
    Session::remove_from_disk(&session.folder_path)?;
    Ok(())
}
