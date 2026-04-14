use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui_textarea::TextArea;

use crate::tui::chrome;
use crate::tui::text_input;
use crate::tui::theme;

enum Mode {
    Editing,
    Naming { buffer: String },
}

pub struct ConfigImportScreen<'a> {
    textarea: TextArea<'a>,
    mode: Mode,
    error: Option<String>,
}

pub enum ImportAction {
    None,
    Cancel,
    /// Save with the given name and raw textproto content.
    Save { name: String, textproto: String },
}

impl<'a> ConfigImportScreen<'a> {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Paste or type textproto config "),
        );
        textarea.set_cursor_line_style(Style::default());
        textarea.set_line_number_style(Style::default().fg(theme::dim()));
        Self {
            textarea,
            mode: Mode::Editing,
            error: None,
        }
    }

    #[allow(dead_code)]
    pub fn from_text(text: &str) -> Self {
        let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        let mut textarea = TextArea::new(lines);
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Edit imported textproto config "),
        );
        textarea.set_cursor_line_style(Style::default());
        textarea.set_line_number_style(Style::default().fg(theme::dim()));
        Self {
            textarea,
            mode: Mode::Editing,
            error: None,
        }
    }

    /// Handle a bracketed-paste payload. In Editing mode we drop it into the
    /// textarea verbatim; in Naming mode the paste becomes a single-line
    /// contribution with newlines collapsed to spaces so a multi-line paste
    /// doesn't break the single-line name field.
    pub fn on_paste(&mut self, text: &str) {
        match &mut self.mode {
            Mode::Editing => {
                self.textarea.insert_str(text);
            }
            Mode::Naming { buffer } => {
                for ch in text.chars() {
                    if ch == '\n' || ch == '\r' {
                        buffer.push(' ');
                    } else {
                        buffer.push(ch);
                    }
                }
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> ImportAction {
        if key.kind != KeyEventKind::Press {
            return ImportAction::None;
        }

        match &mut self.mode {
            Mode::Naming { .. } => self.handle_naming_key(key),
            Mode::Editing => self.handle_editing_key(key),
        }
    }

    fn handle_editing_key(&mut self, key: KeyEvent) -> ImportAction {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => ImportAction::Cancel,
            (KeyCode::Char('s'), m) if m.contains(KeyModifiers::CONTROL) => {
                let content = self.textarea.lines().join("\n");
                if content.trim().is_empty() {
                    self.error = Some("Cannot save empty config".into());
                    ImportAction::None
                } else {
                    self.mode = Mode::Naming {
                        buffer: String::new(),
                    };
                    ImportAction::None
                }
            }
            _ => {
                // Forward all other keys to the textarea for editing
                self.textarea.input(key);
                ImportAction::None
            }
        }
    }

    fn handle_naming_key(&mut self, key: KeyEvent) -> ImportAction {
        let Mode::Naming { mut buffer } = std::mem::replace(&mut self.mode, Mode::Editing) else {
            return ImportAction::None;
        };
        match text_input::apply(&mut buffer, &key) {
            text_input::TextAction::Cancel => ImportAction::None,
            text_input::TextAction::Submit => {
                let name = buffer.trim().to_string();
                if name.is_empty() {
                    self.error = Some("Name cannot be empty".into());
                    ImportAction::None
                } else {
                    let textproto = self.textarea.lines().join("\n");
                    ImportAction::Save { name, textproto }
                }
            }
            text_input::TextAction::Edited | text_input::TextAction::Ignored => {
                self.mode = Mode::Naming { buffer };
                ImportAction::None
            }
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Min(5),
                Constraint::Length(1),
            ])
            .split(area);

        let header = chrome::app_header(Line::from(vec![
            Span::styled("  📋 Import Config ", theme::title()),
            Span::styled(
                "— paste a textproto, then Ctrl-S to save",
                theme::hint(),
            ),
        ]));
        frame.render_widget(header, chunks[0]);

        frame.render_widget(&self.textarea, chunks[1]);

        let footer = if let Some(msg) = &self.error {
            Line::from(Span::styled(
                format!(" ✗ {msg}"),
                Style::default().fg(theme::err()),
            ))
        } else {
            match &self.mode {
                Mode::Naming { buffer } => Line::from(vec![
                    Span::styled(" config name › ", theme::title()),
                    Span::raw(buffer.clone()),
                    Span::styled("█", Style::default().fg(theme::accent())),
                    Span::styled("   [Enter] save  [Esc] cancel", theme::hint()),
                ]),
                Mode::Editing => Line::from(vec![
                    Span::styled(" [Ctrl-S]", theme::title()),
                    Span::raw(" save  "),
                    Span::styled("[Ctrl-V]", theme::title()),
                    Span::raw(" paste  "),
                    Span::styled("[Esc]", theme::title()),
                    Span::raw(" cancel"),
                ]),
            }
        };
        frame.render_widget(Paragraph::new(footer), chunks[2]);
    }
}
