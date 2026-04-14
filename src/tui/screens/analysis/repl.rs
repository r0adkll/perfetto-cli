//! SQL tab: metric authoring surface.
//!
//! The REPL is a three-pane editor for creating and managing the per-app
//! saved metrics that populate the Summary tab's "Custom metrics" section.
//!
//! Layout (vertical):
//!   1. Saved metrics list — one row per persisted metric for this package
//!      with a compact latest-result summary. `Alt+Up`/`Alt+Down` cycle the
//!      highlight; `Alt+L` loads the highlighted metric into the editor.
//!   2. Result pane — renders the most recent run of the editor's SQL.
//!   3. Editor — multi-line `ratatui_textarea` textarea.
//!
//! Actions are invoked via `Alt+<chord>` so they don't collide with SQL
//! content typed into the editor. Plain Enter inserts a newline.
//!
//!   `Alt+Enter`  run the editor content
//!   `Alt+S`      save the editor content as a metric (prompts for a name
//!                when new; upserts in place when editing an existing one)
//!   `Alt+L`      load the highlighted metric into the editor
//!   `Alt+N`      clear the editor (start a new metric)
//!   `Alt+R`      rename the highlighted metric (inline prompt)
//!   `Alt+D`      delete the highlighted metric (requires confirm)
//!   `Alt+Up/Dn`  cycle the saved-metrics highlight
//!
//! Modal sub-states (`SaveAs`, `Rename`, `ConfirmDelete`) mirror the
//! `session_detail::Mode` pattern: while in one of these modes the editor
//! area is replaced by a prompt block.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, Paragraph, Row as TableRow, Table, Wrap,
};
use ratatui_textarea::{CursorMove, TextArea};

use crate::db::Database;
use crate::trace_processor::QueryResult;
use crate::tui::text_input::{self, TextAction};
use crate::tui::theme;

use super::library::{self, LIBRARY};
use super::summary::cell_display;

/// Max rows we render from a single result. Decoder still materialises all
/// rows — this is purely a display/scroll cap.
const RESULT_ROW_CAP: usize = 500;

/// Height of the multi-line SQL input area, in rows (including borders).
const INPUT_HEIGHT: u16 = 8;

/// Height of the saved-metrics pane. 6 visible rows + top/bottom borders
/// gives room for ~4 metrics at a glance without eating the result pane.
const SAVED_HEIGHT: u16 = 8;

/// Cap for how many saved metrics we render before truncating.
const SAVED_VISIBLE_CAP: usize = 6;

/// Max characters to render on a single saved-metric row before truncating.
const SAVED_LINE_MAX_CHARS: usize = 60;

/// Max length of a metric name the user can type. Matches the DB limit
/// applied by the DAO's `name` column (no enforced length there, but
/// this keeps the UI sane).
const MAX_NAME_CHARS: usize = 80;

// ── types ────────────────────────────────────────────────────────────────

/// One entry in the REPL's saved-metrics list. Holds the DB-backed state
/// plus a volatile "latest result summary" populated when a
/// `CustomResult` event for this name arrives.
#[derive(Debug, Clone)]
struct SavedMetricRow {
    name: String,
    sql: String,
    latest: Option<MetricRunSummary>,
}

#[derive(Debug, Clone)]
struct MetricRunSummary {
    /// Compact human-readable representation: `"73,031"` for 1×1,
    /// `"12 rows"` for multi-row, `"✗"` for errors, etc.
    text: String,
    /// Elapsed wall time reported by trace_processor, if any.
    elapsed_ms: Option<f64>,
}

/// Current (most-recent) query result displayed in the middle pane.
pub enum Current {
    Idle,
    Result {
        #[allow(dead_code)]
        sql: String,
        data: QueryResult,
    },
    Error {
        #[allow(dead_code)]
        sql: String,
        message: String,
    },
}

/// Modal sub-state of the REPL. `Editing` is the default and implies the
/// textarea takes focus; the other three replace the editor area with an
/// inline prompt.
enum Mode {
    Editing,
    SaveAs { buffer: String },
    Rename { original: String, buffer: String },
    ConfirmDelete { name: String },
    /// Library picker: browse out-of-box queries and load one into the
    /// editor. Highlight tracks the currently-selected `LIBRARY` entry.
    Library { highlight: usize },
}

pub struct ReplState {
    db: Database,
    package_name: String,

    editor: TextArea<'static>,
    /// DB-backed snapshot of this package's saved metrics, ordered by
    /// creation. Refreshed via `reload_saved()` after every mutation.
    saved: Vec<SavedMetricRow>,
    /// Index in `saved` the user is pointing at. `None` when empty.
    highlight: Option<usize>,
    /// Name of the saved metric whose SQL was last loaded into the editor.
    /// Compared byte-for-byte against `editor` content to derive the
    /// dirty marker shown in the editor title.
    editing: Option<String>,

    current: Current,
    scroll: u16,
    mode: Mode,
    /// Name pre-filled into the SaveAs prompt when the user runs
    /// `Alt+S` on a library-loaded query. Cleared on successful save
    /// or on `Alt+N` (new).
    suggested_save_name: Option<String>,
    /// Transient error from a modal prompt (empty name, rename collision).
    /// Parent polls via [`take_command_error`] and routes to its status
    /// bar.
    command_error: Option<String>,
}

/// Action emitted by `on_key` for the parent screen to act on.
pub enum KeyOutcome {
    None,
    /// Run this SQL via the worker.
    Submit(String),
    /// REPL touched the saved_queries table. The parent reloads the
    /// Summary tab's custom-metrics snapshot and re-dispatches
    /// `RunSummary` so the dashboard picks up the new/changed metric.
    SavedMetricsChanged,
}

impl ReplState {
    pub fn new(db: Database, package_name: String) -> Self {
        let mut this = Self {
            db,
            package_name,
            editor: fresh_editor(Mode::Editing, None, false),
            saved: Vec::new(),
            highlight: None,
            editing: None,
            current: Current::Idle,
            scroll: 0,
            mode: Mode::Editing,
            suggested_save_name: None,
            command_error: None,
        };
        this.reload_saved();
        this
    }

    pub fn take_command_error(&mut self) -> Option<String> {
        self.command_error.take()
    }

    /// Re-read the saved_queries table for this package. Preserves the
    /// highlight position when possible (keeps the same name selected);
    /// falls back to the first row or nothing otherwise.
    fn reload_saved(&mut self) {
        let prior_name = self.highlight.and_then(|i| self.saved.get(i)).map(|r| r.name.clone());
        let prior_latest: std::collections::HashMap<String, MetricRunSummary> = self
            .saved
            .drain(..)
            .filter_map(|r| r.latest.map(|l| (r.name, l)))
            .collect();

        self.saved = self
            .db
            .list_saved_queries(&self.package_name)
            .unwrap_or_default()
            .into_iter()
            .map(|sq| SavedMetricRow {
                latest: prior_latest.get(&sq.name).cloned(),
                name: sq.name,
                sql: sq.sql,
            })
            .collect();

        self.highlight = if self.saved.is_empty() {
            None
        } else if let Some(prior) = prior_name {
            self.saved
                .iter()
                .position(|r| r.name == prior)
                .or(Some(0))
        } else {
            Some(0)
        };
    }

    /// Update the saved-metric summary when a `CustomResult` event arrives.
    /// Called by the parent screen with the name and the raw query result.
    pub fn on_custom_result(
        &mut self,
        name: &str,
        result: &Result<QueryResult, String>,
    ) {
        let summary = match result {
            Ok(qr) => MetricRunSummary {
                text: summarise_result(qr),
                elapsed_ms: qr.elapsed_ms,
            },
            Err(_) => MetricRunSummary {
                text: "✗".into(),
                elapsed_ms: None,
            },
        };
        if let Some(row) = self.saved.iter_mut().find(|r| r.name == name) {
            row.latest = Some(summary);
        }
    }

    /// Record the result of a REPL-submitted query (not a saved-metric
    /// dispatch — that flows through `on_custom_result`).
    pub fn on_result(
        &mut self,
        _id: u64,
        sql: String,
        result: Result<QueryResult, String>,
    ) {
        self.current = match result {
            Ok(data) => Current::Result { sql, data },
            Err(message) => Current::Error { sql, message },
        };
    }

    /// Detect whether the editor content diverges from the currently
    /// "editing" saved metric's SQL. `true` only when we have an
    /// `editing = Some(name)` and the editor buffer doesn't match.
    fn is_dirty(&self) -> bool {
        let Some(name) = &self.editing else {
            return false;
        };
        let Some(row) = self.saved.iter().find(|r| &r.name == name) else {
            return false;
        };
        self.editor_text() != row.sql
    }

    fn editor_text(&self) -> String {
        self.editor.lines().join("\n")
    }

    // ── key handling ─────────────────────────────────────────────────────

    pub fn on_key(&mut self, key: KeyEvent) -> KeyOutcome {
        if key.kind != KeyEventKind::Press {
            return KeyOutcome::None;
        }

        // Route to the active modal before anything else — modal keys
        // should not leak back to the editor.
        match &self.mode {
            Mode::SaveAs { .. } => return self.on_key_save_as(key),
            Mode::Rename { .. } => return self.on_key_rename(key),
            Mode::ConfirmDelete { .. } => return self.on_key_confirm_delete(key),
            Mode::Library { .. } => return self.on_key_library(key),
            Mode::Editing => {}
        }

        self.on_key_editing(key)
    }

    fn on_key_editing(&mut self, key: KeyEvent) -> KeyOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // Alt-chords: action keys. Order matters only for readability —
        // none overlap.
        if alt {
            match key.code {
                KeyCode::Enter => return self.submit_current_editor(),
                KeyCode::Char('s') | KeyCode::Char('S') => return self.start_save(),
                KeyCode::Char('l') | KeyCode::Char('L') => return self.load_highlighted(),
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.clear_editor_new();
                    return KeyOutcome::None;
                }
                KeyCode::Char('d') | KeyCode::Char('D') => return self.start_delete(),
                KeyCode::Char('r') | KeyCode::Char('R') => return self.start_rename(),
                KeyCode::Char('i') | KeyCode::Char('I') => {
                    self.mode = Mode::Library { highlight: 0 };
                    return KeyOutcome::None;
                }
                KeyCode::Up => {
                    self.cycle_highlight(-1);
                    return KeyOutcome::None;
                }
                KeyCode::Down => {
                    self.cycle_highlight(1);
                    return KeyOutcome::None;
                }
                _ => {}
            }
        }

        // Ctrl+U (clear editor, shell convention).
        if ctrl && matches!(key.code, KeyCode::Char('u')) {
            self.clear_editor_new();
            return KeyOutcome::None;
        }

        // Esc clears the editor too (unchanged from prior REPL).
        if matches!(key.code, KeyCode::Esc) {
            self.clear_editor_new();
            return KeyOutcome::None;
        }

        // Result-pane scroll: Shift+↑/↓ for line, PageUp/PageDown for 10.
        match (key.code, shift) {
            (KeyCode::PageDown, _) => {
                self.scroll = self.scroll.saturating_add(10);
                return KeyOutcome::None;
            }
            (KeyCode::PageUp, _) => {
                self.scroll = self.scroll.saturating_sub(10);
                return KeyOutcome::None;
            }
            (KeyCode::Up, true) => {
                self.scroll = self.scroll.saturating_sub(1);
                return KeyOutcome::None;
            }
            (KeyCode::Down, true) => {
                self.scroll = self.scroll.saturating_add(1);
                return KeyOutcome::None;
            }
            _ => {}
        }

        // Everything else feeds the textarea.
        self.editor.input(key);
        KeyOutcome::None
    }

    fn on_key_save_as(&mut self, key: KeyEvent) -> KeyOutcome {
        let Mode::SaveAs { mut buffer } = std::mem::replace(&mut self.mode, Mode::Editing) else {
            return KeyOutcome::None;
        };
        match text_input::apply(&mut buffer, &key) {
            TextAction::Cancel => {
                // Back to editor, editor content untouched.
                KeyOutcome::None
            }
            TextAction::Submit => {
                let name = buffer.trim().to_string();
                if let Some(err) = validate_name(&name) {
                    self.command_error = Some(err);
                    self.mode = Mode::SaveAs { buffer };
                    return KeyOutcome::None;
                }
                let sql = self.editor_text();
                match self.db.upsert_saved_query(&self.package_name, &name, &sql) {
                    Ok(_) => {
                        self.reload_saved();
                        self.highlight = self.saved.iter().position(|r| r.name == name);
                        self.editing = Some(name);
                        self.suggested_save_name = None;
                        KeyOutcome::SavedMetricsChanged
                    }
                    Err(e) => {
                        self.command_error = Some(format!("save failed: {e:#}"));
                        self.mode = Mode::SaveAs { buffer };
                        KeyOutcome::None
                    }
                }
            }
            TextAction::Edited | TextAction::Ignored => {
                self.mode = Mode::SaveAs { buffer };
                KeyOutcome::None
            }
        }
    }

    fn on_key_rename(&mut self, key: KeyEvent) -> KeyOutcome {
        let Mode::Rename { original, mut buffer } = std::mem::replace(&mut self.mode, Mode::Editing)
        else {
            return KeyOutcome::None;
        };
        match text_input::apply(&mut buffer, &key) {
            TextAction::Cancel => KeyOutcome::None,
            TextAction::Submit => {
                let new_name = buffer.trim().to_string();
                if let Some(err) = validate_name(&new_name) {
                    self.command_error = Some(err);
                    self.mode = Mode::Rename { original, buffer };
                    return KeyOutcome::None;
                }
                if new_name == original {
                    // No-op rename.
                    return KeyOutcome::None;
                }
                match self
                    .db
                    .rename_saved_query(&self.package_name, &original, &new_name)
                {
                    Ok(_) => {
                        // Keep the "editing" pointer consistent if the user
                        // was editing this metric.
                        if self.editing.as_deref() == Some(original.as_str()) {
                            self.editing = Some(new_name.clone());
                        }
                        self.reload_saved();
                        self.highlight =
                            self.saved.iter().position(|r| r.name == new_name);
                        KeyOutcome::SavedMetricsChanged
                    }
                    Err(e) => {
                        self.command_error = Some(format!("rename failed: {e:#}"));
                        self.mode = Mode::Rename { original, buffer };
                        KeyOutcome::None
                    }
                }
            }
            TextAction::Edited | TextAction::Ignored => {
                self.mode = Mode::Rename { original, buffer };
                KeyOutcome::None
            }
        }
    }

    fn on_key_library(&mut self, key: KeyEvent) -> KeyOutcome {
        let current = match &self.mode {
            Mode::Library { highlight } => *highlight,
            _ => return KeyOutcome::None,
        };
        if LIBRARY.is_empty() {
            // Degenerate; shouldn't happen since LIBRARY is a const.
            self.mode = Mode::Editing;
            return KeyOutcome::None;
        }
        let len = LIBRARY.len();
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Editing;
                KeyOutcome::None
            }
            KeyCode::Enter => {
                let entry = &LIBRARY[current];
                let sql = library::render_sql(entry, &self.package_name);
                self.editor = editor_with(&sql);
                self.editing = None;
                self.suggested_save_name = Some(entry.name.to_string());
                self.mode = Mode::Editing;
                KeyOutcome::None
            }
            KeyCode::Up => {
                let next = if current == 0 { len - 1 } else { current - 1 };
                self.mode = Mode::Library { highlight: next };
                KeyOutcome::None
            }
            KeyCode::Down => {
                let next = (current + 1) % len;
                self.mode = Mode::Library { highlight: next };
                KeyOutcome::None
            }
            KeyCode::PageUp | KeyCode::Home => {
                self.mode = Mode::Library { highlight: 0 };
                KeyOutcome::None
            }
            KeyCode::PageDown | KeyCode::End => {
                self.mode = Mode::Library { highlight: len - 1 };
                KeyOutcome::None
            }
            _ => KeyOutcome::None,
        }
    }

    fn on_key_confirm_delete(&mut self, key: KeyEvent) -> KeyOutcome {
        let name = match &self.mode {
            Mode::ConfirmDelete { name } => name.clone(),
            _ => return KeyOutcome::None,
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                match self.db.delete_saved_query(&self.package_name, &name) {
                    Ok(_) => {
                        // If the user was editing the now-deleted metric,
                        // detach (editor content stays in place as an
                        // unsaved buffer).
                        if self.editing.as_deref() == Some(name.as_str()) {
                            self.editing = None;
                        }
                        self.reload_saved();
                        self.mode = Mode::Editing;
                        KeyOutcome::SavedMetricsChanged
                    }
                    Err(e) => {
                        self.command_error = Some(format!("delete failed: {e:#}"));
                        self.mode = Mode::Editing;
                        KeyOutcome::None
                    }
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = Mode::Editing;
                KeyOutcome::None
            }
            _ => KeyOutcome::None,
        }
    }

    // ── action implementations ──────────────────────────────────────────

    fn submit_current_editor(&mut self) -> KeyOutcome {
        let sql = self.editor_text().trim().to_string();
        if sql.is_empty() {
            return KeyOutcome::None;
        }
        self.scroll = 0;
        KeyOutcome::Submit(sql)
    }

    fn start_save(&mut self) -> KeyOutcome {
        let sql = self.editor_text();
        if sql.trim().is_empty() {
            self.command_error = Some("editor is empty — nothing to save".into());
            return KeyOutcome::None;
        }
        // If we're editing an existing metric, update in place.
        if let Some(name) = self.editing.clone() {
            match self.db.upsert_saved_query(&self.package_name, &name, &sql) {
                Ok(_) => {
                    self.reload_saved();
                    self.highlight = self.saved.iter().position(|r| r.name == name);
                    self.suggested_save_name = None;
                    return KeyOutcome::SavedMetricsChanged;
                }
                Err(e) => {
                    self.command_error = Some(format!("save failed: {e:#}"));
                    return KeyOutcome::None;
                }
            }
        }
        // Otherwise prompt for a name, seeding the buffer with any
        // library-derived suggestion (keeps the "load from library →
        // save" flow to a single keystroke on Enter).
        let buffer = self.suggested_save_name.clone().unwrap_or_default();
        self.mode = Mode::SaveAs { buffer };
        KeyOutcome::None
    }

    fn load_highlighted(&mut self) -> KeyOutcome {
        let Some(idx) = self.highlight else {
            self.command_error = Some("no metric highlighted".into());
            return KeyOutcome::None;
        };
        let Some(row) = self.saved.get(idx).cloned() else {
            return KeyOutcome::None;
        };
        self.editor = editor_with(&row.sql);
        self.editing = Some(row.name.clone());
        KeyOutcome::None
    }

    fn clear_editor_new(&mut self) {
        self.editor = fresh_editor(Mode::Editing, None, false);
        self.editing = None;
        self.suggested_save_name = None;
    }

    fn start_delete(&mut self) -> KeyOutcome {
        let Some(idx) = self.highlight else {
            self.command_error = Some("no metric highlighted".into());
            return KeyOutcome::None;
        };
        let Some(row) = self.saved.get(idx) else {
            return KeyOutcome::None;
        };
        self.mode = Mode::ConfirmDelete {
            name: row.name.clone(),
        };
        KeyOutcome::None
    }

    fn start_rename(&mut self) -> KeyOutcome {
        let Some(idx) = self.highlight else {
            self.command_error = Some("no metric highlighted".into());
            return KeyOutcome::None;
        };
        let Some(row) = self.saved.get(idx) else {
            return KeyOutcome::None;
        };
        self.mode = Mode::Rename {
            original: row.name.clone(),
            buffer: row.name.clone(),
        };
        KeyOutcome::None
    }

    fn cycle_highlight(&mut self, delta: i32) {
        if self.saved.is_empty() {
            self.highlight = None;
            return;
        }
        let len = self.saved.len() as i32;
        let cur = self.highlight.unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(len);
        self.highlight = Some(next as usize);
    }

    // ── rendering ────────────────────────────────────────────────────────

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(SAVED_HEIGHT),
                Constraint::Min(5),
                Constraint::Length(INPUT_HEIGHT),
            ])
            .split(area);

        self.render_saved(frame, chunks[0]);
        self.render_result(frame, chunks[1]);
        self.render_editor_or_modal(frame, chunks[2]);
    }

    fn render_saved(&self, frame: &mut Frame, area: Rect) {
        let dim = Style::default().fg(theme::dim());
        let title_text = format!(
            " Saved metrics · {} · {} ",
            self.saved.len(),
            self.package_name
        );
        let hints = "  [Alt+Up/Dn] cycle · [Alt+L] load · [Alt+R] rename · [Alt+D] delete ";
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dim)
            .title(Span::styled(title_text, dim))
            .title_bottom(Line::from(Span::styled(hints, dim)));

        if self.saved.is_empty() {
            let p = Paragraph::new(Line::from(Span::styled(
                "  no saved metrics — run a query and press Alt+S to save",
                dim,
            )))
            .block(block);
            frame.render_widget(p, area);
            return;
        }

        let items: Vec<ListItem> = self
            .saved
            .iter()
            .take(SAVED_VISIBLE_CAP)
            .enumerate()
            .map(|(i, row)| {
                let selected = self.highlight == Some(i);
                let marker = if selected { "▶ " } else { "  " };
                let marker_style = if selected {
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    dim
                };
                let name_style = if selected {
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let summary_text = match &row.latest {
                    Some(s) => {
                        let elapsed = s
                            .elapsed_ms
                            .map(|ms| format!(" · {ms:.1} ms"))
                            .unwrap_or_default();
                        format!("{}{elapsed}", s.text)
                    }
                    None => "…".into(),
                };
                let name_col = truncate(&row.name, 30);
                let rest = truncate(
                    &format!("{:<30} {}", name_col, summary_text),
                    SAVED_LINE_MAX_CHARS.saturating_sub(marker.len()),
                );
                ListItem::new(Line::from(vec![
                    Span::styled(marker.to_string(), marker_style),
                    Span::styled(rest, name_style),
                ]))
            })
            .collect();
        let list = List::new(items).block(block);
        frame.render_widget(list, area);
    }

    fn render_result(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Result ", Style::default().fg(theme::dim())));
        match &self.current {
            Current::Idle => {
                let p = Paragraph::new("type PerfettoSQL and press Alt+Enter to run")
                    .block(block)
                    .style(Style::default().fg(theme::dim()));
                frame.render_widget(p, area);
            }
            Current::Error { message, .. } => {
                let p = Paragraph::new(message.clone())
                    .block(block.title(Span::styled(
                        " Error ",
                        Style::default().fg(theme::err()),
                    )))
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(theme::err()));
                frame.render_widget(p, area);
            }
            Current::Result { data, .. } => {
                if data.columns.is_empty() {
                    let p = Paragraph::new("(no columns)")
                        .block(block)
                        .style(Style::default().fg(theme::dim()));
                    frame.render_widget(p, area);
                    return;
                }
                self.render_result_table(frame, area, data, block);
            }
        }
    }

    fn render_result_table(
        &self,
        frame: &mut Frame,
        area: Rect,
        data: &QueryResult,
        block: Block<'_>,
    ) {
        let ncols = data.columns.len();
        let widths: Vec<Constraint> = (0..ncols)
            .map(|_| Constraint::Percentage((100 / ncols as u16).max(1)))
            .collect();

        let header = TableRow::new(data.columns.iter().cloned().collect::<Vec<_>>()).style(
            Style::default()
                .fg(theme::dim())
                .add_modifier(Modifier::BOLD),
        );

        let display_rows = data.rows.len().min(RESULT_ROW_CAP);
        let skip = (self.scroll as usize).min(display_rows.saturating_sub(1));
        let body: Vec<TableRow> = data
            .rows
            .iter()
            .take(display_rows)
            .skip(skip)
            .map(|r| {
                TableRow::new(
                    r.cells()
                        .iter()
                        .map(|c| truncate(&cell_display(c), 40).to_string())
                        .collect::<Vec<_>>(),
                )
            })
            .collect();

        let total = data.rows.len();
        let title = if total > display_rows {
            format!(" Result · showing {display_rows} of {total} ")
        } else {
            format!(" Result · {total} rows ")
        };
        let elapsed = data
            .elapsed_ms
            .map(|ms| format!(" · {ms:.1} ms"))
            .unwrap_or_default();

        let table = Table::new(body, widths).header(header).block(block.title(
            Span::styled(
                format!("{title}{elapsed}"),
                Style::default().fg(theme::dim()),
            ),
        ));
        frame.render_widget(table, area);
    }

    fn render_editor_or_modal(&self, frame: &mut Frame, area: Rect) {
        match &self.mode {
            Mode::Editing => self.render_editor(frame, area),
            Mode::SaveAs { buffer } => {
                render_name_prompt(frame, area, "Save as…", "metric name", buffer)
            }
            Mode::Rename { original, buffer } => render_name_prompt(
                frame,
                area,
                &format!("Rename {original}"),
                "new name",
                buffer,
            ),
            Mode::ConfirmDelete { name } => render_confirm_delete(frame, area, name),
            Mode::Library { highlight } => render_library_picker(frame, area, *highlight),
        }
    }

    fn render_editor(&self, frame: &mut Frame, area: Rect) {
        let dim = Style::default().fg(theme::dim());
        let dirty = self.is_dirty();
        let title_text = match (&self.editing, dirty) {
            (None, _) => {
                " SQL · new metric · [Alt+Enter] run · [Alt+S] save · [Alt+I] library ".to_string()
            }
            (Some(name), false) => format!(
                " SQL · editing {name} · [Alt+Enter] run · [Alt+S] update · [Alt+I] library "
            ),
            (Some(name), true) => format!(
                " SQL · editing {name} * · [Alt+Enter] run · [Alt+S] update · [Alt+I] library "
            ),
        };
        // Re-style each render — the inner block title lives on TextArea
        // config, so we configure a fresh borrowed version each frame.
        // ratatui_textarea's `set_block` replaces the entire block.
        let mut editor_view = self.editor.clone();
        editor_view.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(title_text, dim)),
        );
        editor_view.set_cursor_line_style(Style::default());
        editor_view.set_line_number_style(Style::default().fg(theme::dim()));
        frame.render_widget(&editor_view, area);
    }
}

fn render_name_prompt(frame: &mut Frame, area: Rect, title: &str, prompt: &str, buffer: &str) {
    let dim = Style::default().fg(theme::dim());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {title} · Enter to save · Esc to cancel "),
            dim,
        ));
    let line = Line::from(vec![
        Span::styled(format!("  {prompt} › "), dim),
        Span::styled(
            buffer,
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("█", Style::default().fg(theme::accent())),
    ]);
    let p = Paragraph::new(line).block(block);
    frame.render_widget(p, area);
}

fn render_library_picker(frame: &mut Frame, area: Rect, highlight: usize) {
    let dim = Style::default().fg(theme::dim());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dim)
        .title(Span::styled(
            format!(
                " Library · {} queries · Enter to load · Esc to cancel ",
                LIBRARY.len()
            ),
            dim,
        ));

    // The picker sits in the editor slot (INPUT_HEIGHT rows). Show a
    // window of entries around the highlight so the highlight is always
    // visible even when scrolled.
    let inner_rows = area.height.saturating_sub(2) as usize;
    let total = LIBRARY.len();
    // Centre the window on the highlight where possible; clamp at
    // both ends.
    let start = highlight.saturating_sub(inner_rows / 2);
    let start = start.min(total.saturating_sub(inner_rows).max(0));

    let items: Vec<ListItem> = LIBRARY
        .iter()
        .enumerate()
        .skip(start)
        .take(inner_rows)
        .map(|(i, entry)| {
            let selected = i == highlight;
            let marker = if selected { "▶ " } else { "  " };
            let marker_style = if selected {
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                dim
            };
            let name_style = if selected {
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            // Line shape: "▶ name   — short description"
            let name = truncate(entry.name, 32);
            let rest = format!(
                " {:<32}  — {}",
                name,
                truncate(entry.description, 80),
            );
            ListItem::new(Line::from(vec![
                Span::styled(marker.to_string(), marker_style),
                Span::styled(rest, name_style),
            ]))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn render_confirm_delete(frame: &mut Frame, area: Rect, name: &str) {
    let dim = Style::default().fg(theme::dim());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::err()))
        .title(Span::styled(
            format!(" Delete \"{name}\"? "),
            Style::default()
                .fg(theme::err())
                .add_modifier(Modifier::BOLD),
        ));
    let line = Line::from(vec![
        Span::styled("  [y]", theme::title()),
        Span::raw(" confirm    "),
        Span::styled("[n]", theme::title()),
        Span::raw(" cancel"),
        Span::styled("      (Enter = y, Esc = n)", dim),
    ]);
    let p = Paragraph::new(line).block(block).alignment(Alignment::Left);
    frame.render_widget(p, area);
}

fn fresh_editor(_mode: Mode, _editing: Option<&str>, _dirty: bool) -> TextArea<'static> {
    let mut ta = TextArea::default();
    // Block is configured per-render in `render_editor`; this base config
    // just sets cursor / line-number styles so an unrendered / fallback
    // path still looks right.
    ta.set_cursor_line_style(Style::default());
    ta.set_line_number_style(Style::default().fg(theme::dim()));
    ta
}

fn editor_with(text: &str) -> TextArea<'static> {
    let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let mut ta = TextArea::new(lines);
    // Park the cursor at the end so "Alt+L" users can immediately append
    // / modify the tail without a manual jump-to-bottom.
    ta.move_cursor(CursorMove::Bottom);
    ta.move_cursor(CursorMove::End);
    ta.set_cursor_line_style(Style::default());
    ta.set_line_number_style(Style::default().fg(theme::dim()));
    ta
}

impl ReplState {
    /// Insert bracketed-paste content directly into the editor.
    /// `TextArea::insert_str` handles multi-line atomically.
    pub fn on_paste(&mut self, text: &str) {
        if !matches!(self.mode, Mode::Editing) {
            // Modal prompts collapse newlines via `text_input::apply_paste`
            // at the screen level; the REPL itself doesn't paste into
            // modal buffers because those are single-line by design.
            return;
        }
        self.editor.insert_str(text);
    }
}

/// Compact summary text for a successful `CustomResult`:
///   1 row × 1 col   → the cell's value
///   1 row × N cols  → "N cols"
///   M rows × …      → "M rows"
///   empty           → "—"
fn summarise_result(qr: &QueryResult) -> String {
    if qr.rows.is_empty() {
        return "—".into();
    }
    if qr.rows.len() == 1 && qr.columns.len() == 1 {
        if let Some(cell) = qr.rows[0].cells().first() {
            return truncate(&cell_display(cell), 30);
        }
    }
    if qr.rows.len() == 1 {
        return format!("{} cols", qr.columns.len());
    }
    format!("{} rows", qr.rows.len())
}

/// Validate a user-entered metric name. Returns Some(err) on invalid
/// input, None when the name is usable.
fn validate_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("name must not be empty".into());
    }
    if name.len() > MAX_NAME_CHARS {
        return Some(format!("name must be ≤ {MAX_NAME_CHARS} chars"));
    }
    if name.chars().any(|c| c == '\n' || c == '\r') {
        return Some("name must be a single line".into());
    }
    None
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(max_chars + 1);
    let mut count = 0usize;
    for ch in s.chars() {
        if count + 1 > max_chars {
            out.push('…');
            return out;
        }
        out.push(ch);
        count += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_processor::{Cell, Row};
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn test_db() -> Database {
        let conn = Connection::open_in_memory().expect("open memory db");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(include_str!("../../../db/schema.sql"))
            .expect("apply schema");
        Database::from_connection(Arc::new(Mutex::new(conn)))
    }

    fn repl_with(saved: &[(&str, &str)]) -> ReplState {
        let db = test_db();
        for (name, sql) in saved {
            db.upsert_saved_query("com.app", name, sql).unwrap();
        }
        ReplState::new(db, "com.app".into())
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn type_str(r: &mut ReplState, s: &str) {
        for ch in s.chars() {
            r.on_key(press(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn new_with_empty_db_highlights_nothing() {
        let r = repl_with(&[]);
        assert!(r.saved.is_empty());
        assert!(r.highlight.is_none());
        assert!(r.editing.is_none());
    }

    #[test]
    fn new_with_existing_metrics_loads_and_highlights_first() {
        let r = repl_with(&[("a", "SELECT 1"), ("b", "SELECT 2")]);
        assert_eq!(r.saved.len(), 2);
        assert_eq!(r.highlight, Some(0));
    }

    #[test]
    fn alt_up_down_cycles_highlight_with_wrap() {
        let mut r = repl_with(&[("a", "1"), ("b", "2"), ("c", "3")]);
        let _ = r.on_key(alt(KeyCode::Down));
        assert_eq!(r.highlight, Some(1));
        let _ = r.on_key(alt(KeyCode::Down));
        assert_eq!(r.highlight, Some(2));
        let _ = r.on_key(alt(KeyCode::Down));
        assert_eq!(r.highlight, Some(0));
        let _ = r.on_key(alt(KeyCode::Up));
        assert_eq!(r.highlight, Some(2));
    }

    #[test]
    fn alt_up_down_is_noop_on_empty_saved() {
        let mut r = repl_with(&[]);
        let _ = r.on_key(alt(KeyCode::Up));
        assert!(r.highlight.is_none());
    }

    #[test]
    fn alt_l_loads_highlighted_into_editor() {
        let mut r = repl_with(&[("a", "SELECT alpha")]);
        let _ = r.on_key(alt(KeyCode::Char('l')));
        assert_eq!(r.editor_text(), "SELECT alpha");
        assert_eq!(r.editing.as_deref(), Some("a"));
    }

    #[test]
    fn alt_s_on_empty_editor_surfaces_error() {
        let mut r = repl_with(&[]);
        let _ = r.on_key(alt(KeyCode::Char('s')));
        assert!(r.take_command_error().is_some());
        assert!(matches!(r.mode, Mode::Editing));
    }

    #[test]
    fn alt_s_new_metric_enters_save_as_mode() {
        let mut r = repl_with(&[]);
        type_str(&mut r, "SELECT 1");
        let _ = r.on_key(alt(KeyCode::Char('s')));
        assert!(matches!(r.mode, Mode::SaveAs { .. }));
    }

    #[test]
    fn save_as_submit_persists_and_emits_saved_metrics_changed() {
        let mut r = repl_with(&[]);
        type_str(&mut r, "SELECT 1");
        let _ = r.on_key(alt(KeyCode::Char('s'))); // enter SaveAs
        type_str(&mut r, "m1");
        let out = r.on_key(press(KeyCode::Enter));
        assert!(matches!(out, KeyOutcome::SavedMetricsChanged));
        assert!(matches!(r.mode, Mode::Editing));
        assert_eq!(r.saved.len(), 1);
        assert_eq!(r.saved[0].name, "m1");
        assert_eq!(r.editing.as_deref(), Some("m1"));
    }

    #[test]
    fn save_as_empty_name_rejects() {
        let mut r = repl_with(&[]);
        type_str(&mut r, "SELECT 1");
        let _ = r.on_key(alt(KeyCode::Char('s'))); // enter SaveAs
        let _ = r.on_key(press(KeyCode::Enter)); // submit empty
        assert!(r.take_command_error().is_some());
        assert!(matches!(r.mode, Mode::SaveAs { .. }));
    }

    #[test]
    fn save_as_cancel_keeps_editor_content() {
        let mut r = repl_with(&[]);
        type_str(&mut r, "SELECT 42");
        let _ = r.on_key(alt(KeyCode::Char('s')));
        let _ = r.on_key(press(KeyCode::Esc));
        assert!(matches!(r.mode, Mode::Editing));
        assert_eq!(r.editor_text(), "SELECT 42");
    }

    #[test]
    fn alt_s_editing_existing_updates_in_place() {
        let mut r = repl_with(&[("m", "SELECT 1")]);
        let _ = r.on_key(alt(KeyCode::Char('l'))); // load
        // Modify the editor.
        type_str(&mut r, " AS v");
        let out = r.on_key(alt(KeyCode::Char('s')));
        assert!(matches!(out, KeyOutcome::SavedMetricsChanged));
        assert!(matches!(r.mode, Mode::Editing));
        assert_eq!(r.saved[0].sql, "SELECT 1 AS v");
    }

    #[test]
    fn dirty_flag_detects_divergence() {
        let mut r = repl_with(&[("m", "SELECT 1")]);
        let _ = r.on_key(alt(KeyCode::Char('l')));
        assert!(!r.is_dirty());
        type_str(&mut r, " AS v");
        assert!(r.is_dirty());
    }

    #[test]
    fn alt_n_clears_editor_and_detaches() {
        let mut r = repl_with(&[("m", "SELECT 1")]);
        let _ = r.on_key(alt(KeyCode::Char('l')));
        let _ = r.on_key(alt(KeyCode::Char('n')));
        assert!(r.editor_text().is_empty());
        assert!(r.editing.is_none());
    }

    #[test]
    fn alt_d_highlights_and_confirm_deletes() {
        let mut r = repl_with(&[("a", "1"), ("b", "2")]);
        let _ = r.on_key(alt(KeyCode::Char('d'))); // enter confirm
        assert!(matches!(r.mode, Mode::ConfirmDelete { .. }));
        let out = r.on_key(press(KeyCode::Char('y')));
        assert!(matches!(out, KeyOutcome::SavedMetricsChanged));
        assert_eq!(r.saved.len(), 1);
        assert_eq!(r.saved[0].name, "b");
        assert!(matches!(r.mode, Mode::Editing));
    }

    #[test]
    fn alt_d_cancel_leaves_metric() {
        let mut r = repl_with(&[("a", "1")]);
        let _ = r.on_key(alt(KeyCode::Char('d')));
        let _ = r.on_key(press(KeyCode::Char('n')));
        assert_eq!(r.saved.len(), 1);
        assert!(matches!(r.mode, Mode::Editing));
    }

    #[test]
    fn alt_d_cancel_via_esc_also_works() {
        let mut r = repl_with(&[("a", "1")]);
        let _ = r.on_key(alt(KeyCode::Char('d')));
        let _ = r.on_key(press(KeyCode::Esc));
        assert_eq!(r.saved.len(), 1);
    }

    #[test]
    fn alt_r_renames_in_place() {
        let mut r = repl_with(&[("old", "SELECT 1")]);
        let _ = r.on_key(alt(KeyCode::Char('r'))); // enter Rename mode
        // Buffer pre-filled with the old name; apply Ctrl+U style clear
        // then type a new name. For simplicity, use text_input::apply
        // path via plain keys — need to clear the buffer first.
        // The rename buffer starts with "old" — simulate a fresh input.
        let _ = r.on_key(KeyEvent {
            code: KeyCode::Char('u'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        });
        type_str(&mut r, "new");
        let out = r.on_key(press(KeyCode::Enter));
        assert!(matches!(out, KeyOutcome::SavedMetricsChanged));
        assert!(matches!(r.mode, Mode::Editing));
        assert_eq!(r.saved.len(), 1);
        assert_eq!(r.saved[0].name, "new");
        assert_eq!(r.saved[0].sql, "SELECT 1");
    }

    #[test]
    fn alt_r_collision_surfaces_error() {
        let mut r = repl_with(&[("a", "1"), ("b", "2")]);
        // Highlight "a", rename to "b" — UNIQUE constraint should surface.
        let _ = r.on_key(alt(KeyCode::Char('r')));
        let _ = r.on_key(KeyEvent {
            code: KeyCode::Char('u'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        });
        type_str(&mut r, "b");
        let out = r.on_key(press(KeyCode::Enter));
        assert!(matches!(out, KeyOutcome::None));
        assert!(r.take_command_error().is_some());
        assert!(matches!(r.mode, Mode::Rename { .. }));
    }

    #[test]
    fn on_custom_result_records_1x1_summary() {
        let mut r = repl_with(&[("m", "SELECT 1")]);
        let qr = QueryResult {
            columns: vec!["n".into()],
            rows: vec![Row::new_for_test(vec![Cell::Int(73_031)])],
            elapsed_ms: Some(1.2),
        };
        r.on_custom_result("m", &Ok(qr));
        let row = r.saved.iter().find(|r| r.name == "m").unwrap();
        let latest = row.latest.as_ref().unwrap();
        assert_eq!(latest.text, "73031");
        assert_eq!(latest.elapsed_ms, Some(1.2));
    }

    #[test]
    fn on_custom_result_records_multi_row_summary() {
        let mut r = repl_with(&[("m", "SELECT 1")]);
        let qr = QueryResult {
            columns: vec!["a".into()],
            rows: vec![
                Row::new_for_test(vec![Cell::Int(1)]),
                Row::new_for_test(vec![Cell::Int(2)]),
                Row::new_for_test(vec![Cell::Int(3)]),
            ],
            elapsed_ms: None,
        };
        r.on_custom_result("m", &Ok(qr));
        let row = r.saved.iter().find(|r| r.name == "m").unwrap();
        assert_eq!(row.latest.as_ref().unwrap().text, "3 rows");
    }

    #[test]
    fn on_custom_result_records_error_as_cross() {
        let mut r = repl_with(&[("m", "SELECT 1")]);
        r.on_custom_result("m", &Err("no such table".into()));
        let row = r.saved.iter().find(|r| r.name == "m").unwrap();
        assert_eq!(row.latest.as_ref().unwrap().text, "✗");
    }

    #[test]
    fn alt_enter_submits_current_editor() {
        let mut r = repl_with(&[]);
        type_str(&mut r, "SELECT 1");
        match r.on_key(alt(KeyCode::Enter)) {
            KeyOutcome::Submit(sql) => assert_eq!(sql, "SELECT 1"),
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn alt_enter_with_empty_editor_is_noop() {
        let mut r = repl_with(&[]);
        let out = r.on_key(alt(KeyCode::Enter));
        assert!(matches!(out, KeyOutcome::None));
    }

    #[test]
    fn plain_enter_inserts_newline_not_submit() {
        let mut r = repl_with(&[]);
        type_str(&mut r, "a");
        let out = r.on_key(press(KeyCode::Enter));
        assert!(matches!(out, KeyOutcome::None));
        assert_eq!(r.editor.lines(), vec!["a", ""]);
    }

    #[test]
    fn paste_inserts_multiline_atomically() {
        let mut r = repl_with(&[]);
        r.on_paste("SELECT ts\nFROM slice\nLIMIT 10");
        assert_eq!(r.editor.lines(), vec!["SELECT ts", "FROM slice", "LIMIT 10"]);
    }

    #[test]
    fn delete_detaches_editing_pointer() {
        let mut r = repl_with(&[("m", "SELECT 1")]);
        let _ = r.on_key(alt(KeyCode::Char('l'))); // load m
        assert_eq!(r.editing.as_deref(), Some("m"));
        let _ = r.on_key(alt(KeyCode::Char('d')));
        let _ = r.on_key(press(KeyCode::Char('y')));
        assert!(r.editing.is_none(), "editing should detach after delete");
    }

    // ── library-mode tests ───────────────────────────────────────────────

    #[test]
    fn alt_i_enters_library_mode() {
        let mut r = repl_with(&[]);
        let _ = r.on_key(alt(KeyCode::Char('i')));
        assert!(matches!(r.mode, Mode::Library { highlight: 0 }));
    }

    #[test]
    fn library_up_down_cycles_highlight_with_wrap() {
        let mut r = repl_with(&[]);
        let _ = r.on_key(alt(KeyCode::Char('i')));
        let _ = r.on_key(press(KeyCode::Down));
        assert!(matches!(r.mode, Mode::Library { highlight: 1 }));
        // Jump to end via PageDown / End.
        let _ = r.on_key(press(KeyCode::End));
        let last = LIBRARY.len() - 1;
        assert!(matches!(r.mode, Mode::Library { highlight } if highlight == last));
        // Wrap forward.
        let _ = r.on_key(press(KeyCode::Down));
        assert!(matches!(r.mode, Mode::Library { highlight: 0 }));
        // Wrap backward.
        let _ = r.on_key(press(KeyCode::Up));
        assert!(matches!(r.mode, Mode::Library { highlight } if highlight == last));
    }

    #[test]
    fn library_enter_loads_into_editor_with_suggested_name() {
        let mut r = repl_with(&[]);
        let _ = r.on_key(alt(KeyCode::Char('i'))); // enter library
        // Highlight the second entry so the test isn't sensitive to
        // whatever LIBRARY[0] happens to be.
        let _ = r.on_key(press(KeyCode::Down));
        let expected_name = LIBRARY[1].name;
        let _ = r.on_key(press(KeyCode::Enter));

        assert!(matches!(r.mode, Mode::Editing));
        assert!(r.editing.is_none(), "library load must NOT mark as editing");
        assert_eq!(r.suggested_save_name.as_deref(), Some(expected_name));
        // Editor now contains the entry's SQL with {{package}} substituted.
        let body = r.editor_text();
        assert!(!body.is_empty());
        assert!(
            !body.contains("{{package}}"),
            "placeholder must be substituted"
        );
    }

    #[test]
    fn library_esc_cancels_without_touching_editor() {
        let mut r = repl_with(&[]);
        type_str(&mut r, "SELECT 42");
        let _ = r.on_key(alt(KeyCode::Char('i')));
        let _ = r.on_key(press(KeyCode::Esc));
        assert!(matches!(r.mode, Mode::Editing));
        assert_eq!(r.editor_text(), "SELECT 42");
        assert!(r.suggested_save_name.is_none());
    }

    #[test]
    fn library_load_then_save_prefills_save_as_buffer() {
        let mut r = repl_with(&[]);
        let _ = r.on_key(alt(KeyCode::Char('i'))); // enter library
        let _ = r.on_key(press(KeyCode::Enter)); // load LIBRARY[0]
        let expected_name = LIBRARY[0].name;
        let _ = r.on_key(alt(KeyCode::Char('s'))); // start save
        match &r.mode {
            Mode::SaveAs { buffer } => assert_eq!(buffer, expected_name),
            other => panic!("expected SaveAs mode, got {:?}", other as *const _),
        }
    }

    #[test]
    fn alt_n_after_library_load_clears_suggested_name() {
        let mut r = repl_with(&[]);
        let _ = r.on_key(alt(KeyCode::Char('i')));
        let _ = r.on_key(press(KeyCode::Enter));
        assert!(r.suggested_save_name.is_some());
        let _ = r.on_key(alt(KeyCode::Char('n')));
        assert!(r.suggested_save_name.is_none());
        assert!(r.editor_text().is_empty());
    }

    #[test]
    fn successful_save_clears_suggested_name() {
        let mut r = repl_with(&[]);
        let _ = r.on_key(alt(KeyCode::Char('i'))); // library
        let _ = r.on_key(press(KeyCode::Enter)); // load
        assert!(r.suggested_save_name.is_some());
        let _ = r.on_key(alt(KeyCode::Char('s'))); // SaveAs with pre-filled name
        let out = r.on_key(press(KeyCode::Enter)); // accept pre-filled
        assert!(matches!(out, KeyOutcome::SavedMetricsChanged));
        assert!(r.suggested_save_name.is_none());
    }
}
