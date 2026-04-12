use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::perfetto::commands::{self, StartupCommand, COMMAND_CATALOG};
use crate::tui::chrome;
use crate::tui::text_input;
use crate::tui::theme;

enum Mode {
    /// Navigating the command list on the left.
    Browse,
    /// Picking a new command from the catalog.
    AddPicker(ListState),
    /// Editing an argument of the selected command.
    EditArg { cmd_idx: usize, arg_idx: usize, buffer: String },
}

pub struct CommandSetEditorScreen {
    name: String,
    commands: Vec<StartupCommand>,
    list_state: ListState,
    mode: Mode,
    error: Option<String>,
    status: theme::Status,
}

pub enum CmdEditorAction {
    None,
    Cancel,
    Save(Vec<StartupCommand>),
}

impl CommandSetEditorScreen {
    pub fn new(name: String, commands: Vec<StartupCommand>) -> Self {
        let mut list_state = ListState::default();
        if !commands.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            name,
            commands,
            list_state,
            mode: Mode::Browse,
            error: None,
            status: theme::Status::default(),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> CmdEditorAction {
        if key.kind != KeyEventKind::Press {
            return CmdEditorAction::None;
        }

        match &mut self.mode {
            Mode::EditArg { .. } => self.handle_edit_arg_key(key),
            Mode::AddPicker(_) => self.handle_picker_key(key),
            Mode::Browse => self.handle_browse_key(key),
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> CmdEditorAction {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => CmdEditorAction::Cancel,
            (KeyCode::Char('s'), m) if m.contains(KeyModifiers::CONTROL) => {
                CmdEditorAction::Save(self.commands.clone())
            }
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                self.move_cmd(1);
                CmdEditorAction::None
            }
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                self.move_cmd(-1);
                CmdEditorAction::None
            }
            (KeyCode::Char('a') | KeyCode::Char('n'), _) => {
                self.mode = Mode::AddPicker(ListState::default().with_selected(Some(0)));
                CmdEditorAction::None
            }
            (KeyCode::Char('x') | KeyCode::Delete, _) => {
                if let Some(i) = self.list_state.selected() {
                    if i < self.commands.len() {
                        self.commands.remove(i);
                        if self.commands.is_empty() {
                            self.list_state.select(None);
                        } else {
                            self.list_state.select(Some(i.min(self.commands.len() - 1)));
                        }
                    }
                }
                CmdEditorAction::None
            }
            // Move command up/down in the list
            (KeyCode::Char('K'), _) => {
                if let Some(i) = self.list_state.selected() {
                    if i > 0 {
                        self.commands.swap(i, i - 1);
                        self.list_state.select(Some(i - 1));
                    }
                }
                CmdEditorAction::None
            }
            (KeyCode::Char('J'), _) => {
                if let Some(i) = self.list_state.selected() {
                    if i + 1 < self.commands.len() {
                        self.commands.swap(i, i + 1);
                        self.list_state.select(Some(i + 1));
                    }
                }
                CmdEditorAction::None
            }
            (KeyCode::Enter, _) => {
                // Start editing the first arg of the selected command
                if let Some(i) = self.list_state.selected() {
                    if i < self.commands.len() {
                        let cmd = &self.commands[i];
                        let spec = commands::find_spec(&cmd.id);
                        let n_args = spec.map(|s| s.args.len()).unwrap_or(cmd.args.len());
                        if n_args > 0 {
                            let val = cmd.args.first().cloned().unwrap_or_default();
                            self.mode = Mode::EditArg {
                                cmd_idx: i,
                                arg_idx: 0,
                                buffer: val,
                            };
                        }
                    }
                }
                CmdEditorAction::None
            }
            _ => CmdEditorAction::None,
        }
    }

    fn handle_picker_key(&mut self, key: KeyEvent) -> CmdEditorAction {
        let Mode::AddPicker(ref mut state) = self.mode else {
            return CmdEditorAction::None;
        };
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let cur = state.selected().unwrap_or(0);
                state.select(Some((cur + 1) % COMMAND_CATALOG.len()));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let cur = state.selected().unwrap_or(0);
                state.select(Some(
                    (cur + COMMAND_CATALOG.len() - 1) % COMMAND_CATALOG.len(),
                ));
            }
            KeyCode::Enter => {
                if let Some(i) = state.selected() {
                    if let Some(spec) = COMMAND_CATALOG.get(i) {
                        let args = spec.args.iter().map(|_| String::new()).collect();
                        self.commands.push(StartupCommand {
                            id: spec.id.to_string(),
                            args,
                        });
                        let new_idx = self.commands.len() - 1;
                        self.list_state.select(Some(new_idx));
                        // Jump into editing the first arg of the new command
                        if !spec.args.is_empty() {
                            self.mode = Mode::EditArg {
                                cmd_idx: new_idx,
                                arg_idx: 0,
                                buffer: String::new(),
                            };
                        } else {
                            self.mode = Mode::Browse;
                        }
                    }
                }
            }
            _ => {}
        }
        CmdEditorAction::None
    }

    fn handle_edit_arg_key(&mut self, key: KeyEvent) -> CmdEditorAction {
        let Mode::EditArg {
            cmd_idx,
            arg_idx,
            ref mut buffer,
        } = self.mode
        else {
            return CmdEditorAction::None;
        };
        match text_input::apply(buffer, &key) {
            text_input::TextAction::Submit => {
                // Save the arg value
                if cmd_idx < self.commands.len() {
                    let cmd = &mut self.commands[cmd_idx];
                    while cmd.args.len() <= arg_idx {
                        cmd.args.push(String::new());
                    }
                    cmd.args[arg_idx] = buffer.clone();
                    // Advance to next arg if available
                    let spec = commands::find_spec(&cmd.id);
                    let n_args = spec.map(|s| s.args.len()).unwrap_or(cmd.args.len());
                    if arg_idx + 1 < n_args {
                        let next_val = cmd.args.get(arg_idx + 1).cloned().unwrap_or_default();
                        self.mode = Mode::EditArg {
                            cmd_idx,
                            arg_idx: arg_idx + 1,
                            buffer: next_val,
                        };
                    } else {
                        self.mode = Mode::Browse;
                    }
                } else {
                    self.mode = Mode::Browse;
                }
            }
            text_input::TextAction::Cancel => {
                self.mode = Mode::Browse;
            }
            _ => {}
        }
        CmdEditorAction::None
    }

    fn move_cmd(&mut self, delta: i32) {
        if self.commands.is_empty() {
            return;
        }
        let len = self.commands.len() as i32;
        let cur = self.list_state.selected().unwrap_or(0) as i32;
        self.list_state
            .select(Some((cur + delta).rem_euclid(len) as usize));
    }

    // -----------------------------------------------------------------------
    // Render
    // -----------------------------------------------------------------------

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Min(5),
                Constraint::Length(1),
            ])
            .split(area);

        let header = chrome::app_header(Line::from(vec![
            Span::styled("  🚀 Edit Commands ", theme::title()),
            Span::styled(format!("— {}", self.name), theme::hint()),
        ]));
        frame.render_widget(header, rows[0]);

        // Two panes: command list left, detail/picker right
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[1]);

        self.render_command_list(frame, cols[0]);

        match &self.mode {
            Mode::AddPicker(_) => self.render_picker(frame, cols[1]),
            _ => self.render_detail(frame, cols[1]),
        }

        let footer = if let Some(msg) = self.status.get() {
            Line::from(Span::styled(
                format!(" ✓ {msg}"),
                Style::default().fg(theme::ok()),
            ))
        } else if let Some(msg) = &self.error {
            Line::from(Span::styled(
                format!(" ✗ {msg}"),
                Style::default().fg(theme::err()),
            ))
        } else {
            match &self.mode {
                Mode::EditArg {
                    cmd_idx,
                    arg_idx,
                    buffer,
                } => {
                    let label = self
                        .commands
                        .get(*cmd_idx)
                        .and_then(|c| commands::find_spec(&c.id))
                        .and_then(|s| s.args.get(*arg_idx))
                        .map(|a| a.name)
                        .unwrap_or("arg");
                    Line::from(vec![
                        Span::styled(format!(" {label} › "), theme::title()),
                        Span::raw(buffer.clone()),
                        Span::styled("█", Style::default().fg(theme::accent())),
                        Span::styled(
                            "   [Enter] next/done  [Esc] cancel",
                            theme::hint(),
                        ),
                    ])
                }
                Mode::AddPicker(_) => Line::from(vec![
                    Span::styled(" [↑/↓]", theme::title()),
                    Span::raw(" navigate  "),
                    Span::styled("[Enter]", theme::title()),
                    Span::raw(" add  "),
                    Span::styled("[Esc]", theme::title()),
                    Span::raw(" cancel"),
                ]),
                Mode::Browse => Line::from(vec![
                    Span::styled(" [a]", theme::title()),
                    Span::raw(" add  "),
                    Span::styled("[Enter]", theme::title()),
                    Span::raw(" edit args  "),
                    Span::styled("[x]", theme::title()),
                    Span::raw(" remove  "),
                    Span::styled("[Shift-J/K]", theme::title()),
                    Span::raw(" reorder  "),
                    Span::styled("[Ctrl-S]", theme::title()),
                    Span::raw(" save  "),
                    Span::styled("[Esc]", theme::title()),
                    Span::raw(" cancel"),
                ]),
            }
        };
        frame.render_widget(Paragraph::new(footer), rows[2]);
    }

    fn render_command_list(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" Commands ({}) ", self.commands.len()));

        if self.commands.is_empty() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from("  No commands yet."),
                    Line::from(Span::styled(
                        "  Press [a] to add one.",
                        theme::hint(),
                    )),
                ])
                .block(block),
                area,
            );
            return;
        }

        let items: Vec<ListItem> = self
            .commands
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let short_id = cmd
                    .id
                    .strip_prefix("dev.perfetto.")
                    .unwrap_or(&cmd.id);
                let args_summary = if cmd.args.is_empty() || cmd.args.iter().all(|a| a.is_empty()) {
                    "(no args)".to_string()
                } else {
                    cmd.args
                        .iter()
                        .filter(|a| !a.is_empty())
                        .map(|a| {
                            if a.len() > 20 {
                                format!("{}…", &a[..20])
                            } else {
                                a.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let num = format!("{:>2}. ", i + 1);
                ListItem::new(Line::from(vec![
                    Span::styled(num, theme::hint()),
                    Span::styled(
                        short_id.to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(args_summary, theme::hint()),
                ]))
            })
            .collect();

        let is_browse = matches!(self.mode, Mode::Browse);
        let list = List::new(items)
            .block(block)
            .highlight_style(if is_browse {
                Style::default().bg(theme::accent()).fg(Color::Black)
            } else {
                Style::default().fg(theme::accent())
            })
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Command Detail ");

        let Some(idx) = self.list_state.selected() else {
            frame.render_widget(
                Paragraph::new(Span::styled("  Select a command", theme::hint())).block(block),
                area,
            );
            return;
        };
        let Some(cmd) = self.commands.get(idx) else {
            frame.render_widget(Paragraph::new("").block(block), area);
            return;
        };

        let spec = commands::find_spec(&cmd.id);
        let mut lines = vec![
            Line::from(vec![
                Span::styled("  ID: ", theme::hint()),
                Span::styled(
                    cmd.id.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
        ];

        if let Some(spec) = spec {
            lines.push(Line::from(Span::styled(
                format!("  {}", spec.description),
                theme::hint(),
            )));
            lines.push(Line::from(Span::styled(
                format!("  Category: {}", spec.category),
                theme::hint(),
            )));
            lines.push(Line::from(""));

            for (i, arg_spec) in spec.args.iter().enumerate() {
                let value = cmd.args.get(i).map(|s| s.as_str()).unwrap_or("");
                let req = if arg_spec.required { "*" } else { " " };
                let is_editing = matches!(
                    &self.mode,
                    Mode::EditArg { cmd_idx, arg_idx, .. }
                    if *cmd_idx == idx && *arg_idx == i
                );
                let display_val = if is_editing {
                    if let Mode::EditArg { buffer, .. } = &self.mode {
                        format!("{}█", buffer)
                    } else {
                        value.to_string()
                    }
                } else if value.is_empty() {
                    "(empty)".to_string()
                } else {
                    value.to_string()
                };
                let val_style = if is_editing {
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {req}{:<16}", arg_spec.name),
                        theme::hint(),
                    ),
                    Span::styled(display_val, val_style),
                ]));
                lines.push(Line::from(Span::styled(
                    format!("   {}", arg_spec.description),
                    Style::default().fg(theme::dim()),
                )));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "  Custom command (not in catalog)",
                theme::hint(),
            )));
            for (i, val) in cmd.args.iter().enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(format!("  arg[{i}]: "), theme::hint()),
                    Span::raw(val.clone()),
                ]));
            }
        }

        frame.render_widget(
            Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
            area,
        );
    }

    fn render_picker(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Pick a command ");

        let items: Vec<ListItem> = COMMAND_CATALOG
            .iter()
            .map(|spec| {
                let short = spec
                    .id
                    .strip_prefix("dev.perfetto.")
                    .unwrap_or(spec.id);
                ListItem::new(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("[{:<10}] ", spec.category),
                        theme::hint(),
                    ),
                    Span::styled(
                        short.to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]))
            })
            .collect();

        let Mode::AddPicker(ref state) = self.mode else {
            frame.render_widget(Paragraph::new("").block(block), area);
            return;
        };
        let mut picker_state = state.clone();
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().bg(theme::accent()).fg(Color::Black))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut picker_state);
    }
}
