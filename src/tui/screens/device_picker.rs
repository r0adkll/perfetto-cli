use std::collections::HashMap;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use tokio::sync::mpsc::UnboundedSender;

use crate::adb;
use crate::adb::DeviceState;
use crate::db::Database;
use crate::tui::chrome;
use crate::tui::event::AppEvent;
use crate::tui::text_input::{self, TextAction};
use crate::tui::theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    Online,
    Offline,
    Unauthorized,
    Other(String),
    NotConnected,
}

impl From<DeviceState> for EntryState {
    fn from(s: DeviceState) -> Self {
        match s {
            DeviceState::Online => EntryState::Online,
            DeviceState::Offline => EntryState::Offline,
            DeviceState::Unauthorized => EntryState::Unauthorized,
            DeviceState::Other(x) => EntryState::Other(x),
        }
    }
}

impl EntryState {
    fn badge(&self) -> (&'static str, Style) {
        match self {
            EntryState::Online => ("● online      ", Style::default().fg(theme::OK)),
            EntryState::Offline => ("○ offline     ", Style::default().fg(theme::DIM)),
            EntryState::Unauthorized => ("⚠ unauthorized", Style::default().fg(theme::WARN)),
            EntryState::Other(_) => ("? other       ", Style::default().fg(theme::WARN)),
            EntryState::NotConnected => ("· remembered  ", Style::default().fg(theme::DIM)),
        }
    }

    fn selectable(&self) -> bool {
        matches!(self, EntryState::Online)
    }
}

#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub serial: String,
    pub nickname: Option<String>,
    pub model: Option<String>,
    pub state: EntryState,
}

enum LoadState {
    Loading,
    Loaded,
    Error(String),
}

enum Mode {
    Browse,
    EditNickname { buffer: String },
}

pub struct DevicePickerScreen {
    entries: Vec<DeviceEntry>,
    list_state: ListState,
    load: LoadState,
    mode: Mode,
    db: Database,
    tx: UnboundedSender<AppEvent>,
}

impl DevicePickerScreen {
    pub fn new(db: Database, tx: UnboundedSender<AppEvent>) -> Self {
        let mut screen = Self {
            entries: Vec::new(),
            list_state: ListState::default(),
            load: LoadState::Loading,
            mode: Mode::Browse,
            db,
            tx,
        };
        screen.spawn_refresh();
        screen
    }

    fn spawn_refresh(&mut self) {
        self.load = LoadState::Loading;
        let db = self.db.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = load_entries(db).await.map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::DevicesLoaded(result));
        });
    }

    pub fn on_devices_loaded(&mut self, result: Result<Vec<DeviceEntry>, String>) {
        match result {
            Ok(entries) => {
                self.entries = entries;
                self.load = LoadState::Loaded;
                if self.list_state.selected().is_none() && !self.entries.is_empty() {
                    self.list_state.select(Some(0));
                }
                if let Some(i) = self.list_state.selected() {
                    if i >= self.entries.len() && !self.entries.is_empty() {
                        self.list_state.select(Some(self.entries.len() - 1));
                    }
                }
            }
            Err(e) => self.load = LoadState::Error(e),
        }
    }

    /// Returns `Some(serial)` if the user pressed Enter on a selectable device.
    pub fn on_key(&mut self, key: KeyEvent) -> PickerAction {
        if key.kind != KeyEventKind::Press {
            return PickerAction::None;
        }

        if matches!(self.mode, Mode::EditNickname { .. }) {
            let Mode::EditNickname { mut buffer } =
                std::mem::replace(&mut self.mode, Mode::Browse)
            else {
                unreachable!()
            };
            match text_input::apply(&mut buffer, &key) {
                TextAction::Cancel => {}
                TextAction::Submit => {
                    if let Some(entry) = self.selected_entry().cloned() {
                        let nickname = if buffer.trim().is_empty() {
                            None
                        } else {
                            Some(buffer.trim().to_string())
                        };
                        if let Err(e) = self
                            .db
                            .set_device_nickname(&entry.serial, nickname.as_deref())
                        {
                            self.load = LoadState::Error(format!("save nickname: {e}"));
                        }
                    }
                    self.spawn_refresh();
                }
                TextAction::Edited | TextAction::Ignored => {
                    self.mode = Mode::EditNickname { buffer };
                }
            }
            return PickerAction::None;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => PickerAction::Back,
            KeyCode::Char('r') => {
                self.spawn_refresh();
                PickerAction::None
            }
            KeyCode::Char('n') => {
                if let Some(entry) = self.selected_entry() {
                    self.mode = Mode::EditNickname {
                        buffer: entry.nickname.clone().unwrap_or_default(),
                    };
                }
                PickerAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                PickerAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                PickerAction::None
            }
            KeyCode::Enter => {
                if let Some(entry) = self.selected_entry() {
                    if entry.state.selectable() {
                        return PickerAction::Selected(entry.serial.clone());
                    }
                }
                PickerAction::None
            }
            _ => PickerAction::None,
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len() as i32;
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.list_state.select(Some(next as usize));
    }

    fn selected_entry(&self) -> Option<&DeviceEntry> {
        self.list_state.selected().and_then(|i| self.entries.get(i))
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
            Span::styled("  📱 Devices ", theme::title()),
            Span::styled("— pick the device to trace against", theme::hint()),
        ]));
        frame.render_widget(header, chunks[0]);

        let body_block = Block::default().borders(Borders::ALL).title(" adb devices ");

        match &self.load {
            LoadState::Loading => {
                let p = Paragraph::new("  ⏳ running `adb devices -l`…").block(body_block);
                frame.render_widget(p, chunks[1]);
            }
            LoadState::Error(msg) => {
                let p = Paragraph::new(vec![
                    Line::from(Span::styled(
                        format!("  ✗ {msg}"),
                        Style::default().fg(theme::ERR),
                    )),
                    Line::from(""),
                    Line::from(Span::styled("  Press [r] to retry", theme::hint())),
                ])
                .block(body_block);
                frame.render_widget(p, chunks[1]);
            }
            LoadState::Loaded if self.entries.is_empty() => {
                let p = Paragraph::new(vec![
                    Line::from("  No devices found."),
                    Line::from(""),
                    Line::from(Span::styled(
                        "  Connect a device, authorize adb, then press [r].",
                        theme::hint(),
                    )),
                ])
                .block(body_block);
                frame.render_widget(p, chunks[1]);
            }
            LoadState::Loaded => {
                let items: Vec<ListItem> = self
                    .entries
                    .iter()
                    .map(|e| {
                        let (badge, badge_style) = e.state.badge();
                        let label = e.nickname.clone().unwrap_or_else(|| {
                            e.model.clone().unwrap_or_else(|| "Android device".into())
                        });
                        ListItem::new(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(badge, badge_style),
                            Span::raw("  "),
                            Span::styled(
                                label,
                                Style::default().add_modifier(Modifier::BOLD),
                            ),
                            Span::raw("  "),
                            Span::styled(format!("({})", e.serial), theme::hint()),
                        ]))
                    })
                    .collect();
                let list = List::new(items)
                    .block(body_block)
                    .highlight_style(Style::default().bg(theme::ACCENT).fg(ratatui::style::Color::Black))
                    .highlight_symbol("▶ ");
                frame.render_stateful_widget(list, chunks[1], &mut self.list_state);
            }
        }

        let footer_line = match &self.mode {
            Mode::Browse => Line::from(vec![
                Span::styled(" [↑/↓]", theme::title()),
                Span::raw(" move  "),
                Span::styled("[Enter]", theme::title()),
                Span::raw(" select  "),
                Span::styled("[n]", theme::title()),
                Span::raw(" nickname  "),
                Span::styled("[r]", theme::title()),
                Span::raw(" refresh  "),
                Span::styled("[Esc]", theme::title()),
                Span::raw(" back"),
            ]),
            Mode::EditNickname { buffer } => Line::from(vec![
                Span::styled(" nickname › ", theme::title()),
                Span::raw(buffer.clone()),
                Span::styled("_", theme::hint()),
                Span::styled("   [Enter] save  [Esc] cancel", theme::hint()),
            ]),
        };
        frame.render_widget(Paragraph::new(footer_line), chunks[2]);
    }
}

pub enum PickerAction {
    None,
    Back,
    Selected(String),
}

pub(crate) async fn load_entries(db: Database) -> Result<Vec<DeviceEntry>> {
    let live = adb::list_live_devices().await?;
    for d in &live {
        db.upsert_device_seen(&d.serial, d.model.as_deref())?;
    }
    let known = db.list_known_devices()?;

    let mut live_map: HashMap<String, adb::Device> = HashMap::new();
    for d in live {
        live_map.insert(d.serial.clone(), d);
    }

    let mut entries: Vec<DeviceEntry> = known
        .into_iter()
        .map(|rec| {
            let live = live_map.remove(&rec.serial);
            let state = live
                .as_ref()
                .map(|d| d.state.clone().into())
                .unwrap_or(EntryState::NotConnected);
            let model = live
                .as_ref()
                .and_then(|d| d.model.clone())
                .or(rec.model);
            DeviceEntry {
                serial: rec.serial,
                nickname: rec.nickname,
                model,
                state,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        let a_conn = !matches!(a.state, EntryState::NotConnected);
        let b_conn = !matches!(b.state, EntryState::NotConnected);
        b_conn.cmp(&a_conn).then(a.serial.cmp(&b.serial))
    });

    Ok(entries)
}
