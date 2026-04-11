use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::tui::text_input;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::perfetto::{FillPolicy, Preset, TraceConfig, textproto};
use crate::tui::chrome;
use crate::tui::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Preset,
    Duration,
    Buffer,
    FillPolicy,
    ColdStart,
    AutoOpen,
    ComposeTracing,
    LaunchActivity,
    Categories,
    Ftrace,
    AtraceApps,
}

impl Field {
    const ORDER: [Field; 11] = [
        Field::Preset,
        Field::Duration,
        Field::Buffer,
        Field::FillPolicy,
        Field::ColdStart,
        Field::AutoOpen,
        Field::ComposeTracing,
        Field::LaunchActivity,
        Field::Categories,
        Field::Ftrace,
        Field::AtraceApps,
    ];

    fn next(self) -> Self {
        let idx = Self::ORDER.iter().position(|f| *f == self).unwrap();
        Self::ORDER[(idx + 1) % Self::ORDER.len()]
    }
    fn prev(self) -> Self {
        let idx = Self::ORDER.iter().position(|f| *f == self).unwrap();
        Self::ORDER[(idx + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }
    fn label(self) -> &'static str {
        match self {
            Field::Preset => "Preset",
            Field::Duration => "Duration (ms)",
            Field::Buffer => "Buffer (KB)",
            Field::FillPolicy => "Fill policy",
            Field::ColdStart => "Cold start",
            Field::AutoOpen => "Auto-open",
            Field::ComposeTracing => "Compose tracing",
            Field::LaunchActivity => "Launch activity",
            Field::Categories => "Categories",
            Field::Ftrace => "Ftrace events",
            Field::AtraceApps => "Atrace apps",
        }
    }
}

struct Draft {
    duration_ms: String,
    buffer_size_kb: String,
    fill_policy: FillPolicy,
    cold_start: bool,
    auto_open: bool,
    compose_tracing: bool,
    launch_activity: String,
    categories: String,
    ftrace_events: String,
    atrace_apps: String,
}

impl Draft {
    fn from_config(cfg: &TraceConfig) -> Self {
        Self {
            duration_ms: cfg.duration_ms.to_string(),
            buffer_size_kb: cfg.buffer_size_kb.to_string(),
            fill_policy: cfg.fill_policy,
            cold_start: cfg.cold_start,
            auto_open: cfg.auto_open,
            compose_tracing: cfg.compose_tracing,
            launch_activity: cfg.launch_activity.clone().unwrap_or_default(),
            categories: cfg.categories.join(", "),
            ftrace_events: cfg.ftrace_events.join(", "),
            atrace_apps: cfg.atrace_apps.join(", "),
        }
    }

    fn to_config(&self) -> Result<TraceConfig, String> {
        let duration_ms = self
            .duration_ms
            .trim()
            .parse::<u32>()
            .map_err(|_| "Duration must be a positive integer".to_string())?;
        let buffer_size_kb = self
            .buffer_size_kb
            .trim()
            .parse::<u32>()
            .map_err(|_| "Buffer size must be a positive integer".to_string())?;
        let split = |s: &str| -> Vec<String> {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        };
        let launch = self.launch_activity.trim();
        Ok(TraceConfig {
            duration_ms,
            buffer_size_kb,
            fill_policy: self.fill_policy,
            categories: split(&self.categories),
            ftrace_events: split(&self.ftrace_events),
            atrace_apps: split(&self.atrace_apps),
            cold_start: self.cold_start,
            auto_open: self.auto_open,
            compose_tracing: self.compose_tracing,
            launch_activity: if launch.is_empty() {
                None
            } else {
                Some(launch.to_string())
            },
        })
    }
}

pub struct ConfigEditorScreen {
    session_id: Option<i64>,
    session_name: String,
    draft: Draft,
    preset: Preset,
    field: Field,
    error: Option<String>,
    preview_scroll: u16,
}

pub enum EditorAction {
    None,
    Cancel,
    Save(TraceConfig),
}

impl ConfigEditorScreen {
    pub fn new(session_id: Option<i64>, session_name: String, config: &TraceConfig) -> Self {
        Self {
            session_id,
            session_name,
            draft: Draft::from_config(config),
            preset: Preset::Default,
            field: Field::Preset,
            error: None,
            preview_scroll: 0,
        }
    }

    pub fn session_id(&self) -> Option<i64> {
        self.session_id
    }

    pub fn on_key(&mut self, key: KeyEvent) -> EditorAction {
        if key.kind != KeyEventKind::Press {
            return EditorAction::None;
        }

        // Global first.
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => return EditorAction::Cancel,
            (KeyCode::Char('s'), m) if m.contains(KeyModifiers::CONTROL) => {
                return self.try_save();
            }
            (KeyCode::Char('['), _) => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
                return EditorAction::None;
            }
            (KeyCode::Char(']'), _) => {
                self.preview_scroll = self.preview_scroll.saturating_add(1);
                return EditorAction::None;
            }
            (KeyCode::Tab, _) | (KeyCode::Down, _) => {
                self.field = self.field.next();
                return EditorAction::None;
            }
            (KeyCode::BackTab, _) | (KeyCode::Up, _) => {
                self.field = self.field.prev();
                return EditorAction::None;
            }
            _ => {}
        }

        match self.field {
            Field::Preset => self.handle_cycle_key(key, CycleTarget::Preset),
            Field::FillPolicy => self.handle_cycle_key(key, CycleTarget::FillPolicy),
            Field::ColdStart => self.handle_toggle_key(key, ToggleTarget::ColdStart),
            Field::AutoOpen => self.handle_toggle_key(key, ToggleTarget::AutoOpen),
            Field::ComposeTracing => self.handle_toggle_key(key, ToggleTarget::ComposeTracing),
            Field::Duration => {
                self.handle_text_key(key, TextTarget::Duration, TextMode::Numeric)
            }
            Field::Buffer => self.handle_text_key(key, TextTarget::Buffer, TextMode::Numeric),
            Field::LaunchActivity => {
                self.handle_text_key(key, TextTarget::LaunchActivity, TextMode::Any)
            }
            Field::Categories => {
                self.handle_text_key(key, TextTarget::Categories, TextMode::Any)
            }
            Field::Ftrace => self.handle_text_key(key, TextTarget::Ftrace, TextMode::Any),
            Field::AtraceApps => {
                self.handle_text_key(key, TextTarget::AtraceApps, TextMode::Any)
            }
        }
        EditorAction::None
    }

    fn handle_cycle_key(&mut self, key: KeyEvent, target: CycleTarget) {
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => match target {
                CycleTarget::Preset => self.apply_preset(self.preset.cycle_back()),
                CycleTarget::FillPolicy => self.draft.fill_policy = self.draft.fill_policy.cycle(),
            },
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => match target {
                CycleTarget::Preset => self.apply_preset(self.preset.cycle_forward()),
                CycleTarget::FillPolicy => self.draft.fill_policy = self.draft.fill_policy.cycle(),
            },
            _ => {}
        }
    }

    fn handle_toggle_key(&mut self, key: KeyEvent, target: ToggleTarget) {
        if matches!(
            key.code,
            KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Left | KeyCode::Right
        ) {
            let slot = match target {
                ToggleTarget::ColdStart => &mut self.draft.cold_start,
                ToggleTarget::AutoOpen => &mut self.draft.auto_open,
                ToggleTarget::ComposeTracing => &mut self.draft.compose_tracing,
            };
            *slot = !*slot;
        }
    }

    fn handle_text_key(&mut self, key: KeyEvent, target: TextTarget, mode: TextMode) {
        let buffer: &mut String = match target {
            TextTarget::Duration => &mut self.draft.duration_ms,
            TextTarget::Buffer => &mut self.draft.buffer_size_kb,
            TextTarget::LaunchActivity => &mut self.draft.launch_activity,
            TextTarget::Categories => &mut self.draft.categories,
            TextTarget::Ftrace => &mut self.draft.ftrace_events,
            TextTarget::AtraceApps => &mut self.draft.atrace_apps,
        };
        let allow: fn(char) -> bool = match mode {
            TextMode::Any => |_| true,
            TextMode::Numeric => |c| c.is_ascii_digit(),
        };
        // Submit/Cancel from the helper are pre-intercepted at the top of
        // `on_key` (Enter toggles/cycles, Esc cancels globally), so anything
        // that bubbles up here is just an edit.
        let _ = text_input::apply_filtered(buffer, &key, allow);
    }

    fn apply_preset(&mut self, preset: Preset) {
        self.preset = preset;
        self.draft = Draft::from_config(&preset.config());
    }

    fn try_save(&mut self) -> EditorAction {
        match self.draft.to_config() {
            Ok(cfg) => EditorAction::Save(cfg),
            Err(e) => {
                self.error = Some(e);
                EditorAction::None
            }
        }
    }

    fn preview_config(&self) -> TraceConfig {
        self.draft.to_config().unwrap_or_else(|_| TraceConfig {
            duration_ms: 0,
            buffer_size_kb: 0,
            fill_policy: self.draft.fill_policy,
            categories: Vec::new(),
            ftrace_events: Vec::new(),
            atrace_apps: Vec::new(),
            cold_start: self.draft.cold_start,
            auto_open: self.draft.auto_open,
            compose_tracing: self.draft.compose_tracing,
            launch_activity: Some(self.draft.launch_activity.clone())
                .filter(|s| !s.trim().is_empty()),
        })
    }

    pub fn render(&self, frame: &mut Frame) {
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
            Span::styled("  ⚙  Config ", theme::title()),
            Span::styled(format!("— {}", self.session_name), theme::hint()),
        ]));
        frame.render_widget(header, rows[0]);

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[1]);

        let form = Paragraph::new(self.form_lines())
            .block(Block::default().borders(Borders::ALL).title(" Fields "))
            .wrap(Wrap { trim: false });
        frame.render_widget(form, cols[0]);

        let preview_text = textproto::build(&self.preview_config());
        let preview = Paragraph::new(preview_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Textproto preview "),
            )
            .scroll((self.preview_scroll, 0));
        frame.render_widget(preview, cols[1]);

        let footer = match &self.error {
            Some(msg) => Line::from(Span::styled(
                format!(" ✗ {msg}"),
                Style::default().fg(theme::ERR),
            )),
            None => Line::from(vec![
                Span::styled(" [Tab]", theme::title()),
                Span::raw(" move  "),
                Span::styled("[←/→]", theme::title()),
                Span::raw(" cycle  "),
                Span::styled("[Space]", theme::title()),
                Span::raw(" toggle  "),
                Span::styled("[ [ / ] ]", theme::title()),
                Span::raw(" scroll preview  "),
                Span::styled("[Ctrl-S]", theme::title()),
                Span::raw(" save  "),
                Span::styled("[Esc]", theme::title()),
                Span::raw(" cancel"),
            ]),
        };
        frame.render_widget(Paragraph::new(footer), rows[2]);
    }

    fn form_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for field in Field::ORDER {
            let focused = field == self.field;
            let arrow = if focused { "▶ " } else { "  " };
            let label = format!("{:<16}", field.label());
            let value = self.field_value(field);
            let value_style = if focused {
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let mut spans = vec![
                Span::styled(arrow, Style::default().fg(theme::ACCENT)),
                Span::styled(label, theme::hint()),
                Span::raw("  "),
                Span::styled(value, value_style),
            ];
            if focused {
                spans.push(Span::styled("█", Style::default().fg(theme::ACCENT)));
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {}", self.preset.description()),
            theme::hint(),
        )));
        lines
    }

    fn field_value(&self, field: Field) -> String {
        match field {
            Field::Preset => format!("‹ {} ›", self.preset.label()),
            Field::Duration => self.draft.duration_ms.clone(),
            Field::Buffer => self.draft.buffer_size_kb.clone(),
            Field::FillPolicy => format!("‹ {} ›", self.draft.fill_policy.label()),
            Field::ColdStart => {
                if self.draft.cold_start {
                    "yes".into()
                } else {
                    "no".into()
                }
            }
            Field::AutoOpen => {
                if self.draft.auto_open {
                    "yes".into()
                } else {
                    "no".into()
                }
            }
            Field::ComposeTracing => {
                if self.draft.compose_tracing {
                    "yes".into()
                } else {
                    "no".into()
                }
            }
            Field::LaunchActivity => {
                if self.draft.launch_activity.trim().is_empty() {
                    "(auto-resolve)".into()
                } else {
                    self.draft.launch_activity.clone()
                }
            }
            Field::Categories => {
                if self.draft.categories.is_empty() {
                    "(none)".into()
                } else {
                    self.draft.categories.clone()
                }
            }
            Field::Ftrace => {
                if self.draft.ftrace_events.is_empty() {
                    "(none)".into()
                } else {
                    self.draft.ftrace_events.clone()
                }
            }
            Field::AtraceApps => {
                if self.draft.atrace_apps.is_empty() {
                    "(none)".into()
                } else {
                    self.draft.atrace_apps.clone()
                }
            }
        }
    }
}

enum CycleTarget {
    Preset,
    FillPolicy,
}

enum ToggleTarget {
    ColdStart,
    AutoOpen,
    ComposeTracing,
}

enum TextTarget {
    Duration,
    Buffer,
    LaunchActivity,
    Categories,
    Ftrace,
    AtraceApps,
}

enum TextMode {
    Any,
    Numeric,
}
