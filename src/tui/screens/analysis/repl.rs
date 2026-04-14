//! REPL tab: raw PerfettoSQL input with a scrollable result table and
//! in-memory history.
//!
//! Input is a multi-line [`ratatui_textarea::TextArea`] — queries are often
//! multi-line (`INCLUDE PERFETTO MODULE`, CTEs, formatted SELECTs) and the
//! single-line buffer we used to ship forced people to cram everything onto
//! one line. **Submit is `Ctrl+Enter` or `Alt+Enter`** (both wired to the
//! same path because terminal key-modifier reporting varies); plain Enter
//! inserts a newline.
//!
//! Layout (vertical): history summary (top, 6 rows), result table (middle,
//! fills remaining height), input textarea (bottom, 8 rows).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, Paragraph, Row as TableRow, Table, Wrap,
};
use ratatui_textarea::TextArea;

use crate::trace_processor::QueryResult;
use crate::tui::theme;

use super::summary::cell_display;

/// Max rows we render from a single result. Decoder still materialises all
/// rows — this is purely a display/scroll cap.
const RESULT_ROW_CAP: usize = 500;

/// Height of the multi-line SQL input area, in rows (including borders).
const INPUT_HEIGHT: u16 = 16;

/// One entry in the REPL history list.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub sql: String,
    pub elapsed_ms: Option<f64>,
    pub row_count: Option<usize>,
    pub error: Option<String>,
}

/// Current (most-recent) result the REPL is displaying.
pub enum Current {
    Idle,
    Running {
        id: u64,
        sql: String,
    },
    Result {
        #[allow(dead_code)] // retained for future "re-run" / copy-to-input affordances
        sql: String,
        data: QueryResult,
    },
    Error {
        #[allow(dead_code)]
        sql: String,
        message: String,
    },
}

pub struct ReplState {
    editor: TextArea<'static>,
    /// History in chronological order; oldest first, newest last.
    history: Vec<HistoryEntry>,
    /// Index into `history` the user is currently browsing (None = editing
    /// new query).
    recall_idx: Option<usize>,
    current: Current,
    scroll: u16,
}

impl Default for ReplState {
    fn default() -> Self {
        Self::new()
    }
}

pub enum KeyOutcome {
    None,
    Submit(String),
}

impl ReplState {
    pub fn new() -> Self {
        Self {
            editor: fresh_editor(),
            history: Vec::new(),
            recall_idx: None,
            current: Current::Idle,
            scroll: 0,
        }
    }

    /// Handle a key event while the REPL tab has focus. The parent screen
    /// has already intercepted global keys (q, Tab, 1/2, `o`). Inside the
    /// REPL we route:
    ///
    /// - Submit chords (`Ctrl+Enter`, `Alt+Enter`) → run the query.
    /// - `Ctrl+U` → clear the input.
    /// - Shift+↑/↓, PageUp/PageDown → scroll the result table.
    /// - Plain ↑/↓ when the input is empty → recall history.
    /// - Esc → clear the input.
    /// - Everything else → delegate to the `TextArea` (typing, newlines,
    ///   cursor movement, word delete, etc).
    pub fn on_key(&mut self, key: KeyEvent) -> KeyOutcome {
        if key.kind != KeyEventKind::Press {
            return KeyOutcome::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // Submit: Ctrl+Enter or Alt+Enter. Both map to the same action so
        // users on any terminal (including those that can't distinguish
        // Ctrl+Enter from plain Enter) have a working submit chord.
        if matches!(key.code, KeyCode::Enter) && (ctrl || alt) {
            let sql = self.current_sql().trim().to_string();
            if sql.is_empty() {
                return KeyOutcome::None;
            }
            self.editor = fresh_editor();
            self.recall_idx = None;
            self.scroll = 0;
            return KeyOutcome::Submit(sql);
        }

        // Clear input on Ctrl+U (muscle memory from text_input::apply).
        if matches!(key.code, KeyCode::Char('u')) && ctrl {
            self.editor = fresh_editor();
            self.recall_idx = None;
            return KeyOutcome::None;
        }

        // Esc clears the input. (Outer screen's own Esc-to-back was already
        // consumed above it in the key-routing chain.)
        if matches!(key.code, KeyCode::Esc) {
            self.editor = fresh_editor();
            self.recall_idx = None;
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

        // History recall: active when the input is empty OR when the user is
        // already cycling (`recall_idx.is_some()`). The moment they edit the
        // recalled text, we clear `recall_idx` — after that Up/Down feed the
        // textarea for cursor movement instead. This matches shell history
        // behaviour: Up-Up-Up cycles back, but once you start typing, arrow
        // keys navigate the buffer.
        if matches!(key.code, KeyCode::Up | KeyCode::Down)
            && !ctrl
            && !alt
            && !shift
            && (self.is_editor_empty() || self.recall_idx.is_some())
        {
            self.navigate_history(key.code);
            return KeyOutcome::None;
        }

        // Everything else feeds the textarea. Any keystroke other than the
        // recall arrows (handled above) or result-scroll keys clears the
        // recall marker — we're composing now.
        self.recall_idx = None;
        self.editor.input(key);
        KeyOutcome::None
    }

    fn current_sql(&self) -> String {
        self.editor.lines().join("\n")
    }

    /// Insert bracketed-paste content directly into the textarea. `insert_str`
    /// applies the whole payload in one step (including newlines), which is
    /// why enabling bracketed paste matters — without it the terminal
    /// streams one synthetic keystroke per character and large pastes feel
    /// broken.
    pub fn on_paste(&mut self, text: &str) {
        self.recall_idx = None;
        self.editor.insert_str(text);
    }

    fn is_editor_empty(&self) -> bool {
        let lines = self.editor.lines();
        lines.is_empty() || (lines.len() == 1 && lines[0].is_empty())
    }

    /// Record that a query has been sent; the UI shows "running…" until the
    /// matching `on_result` arrives.
    pub fn on_submit(&mut self, id: u64, sql: String) {
        self.current = Current::Running {
            id,
            sql: sql.clone(),
        };
        self.history.push(HistoryEntry {
            sql,
            elapsed_ms: None,
            row_count: None,
            error: None,
        });
    }

    pub fn on_result(&mut self, id: u64, sql: String, result: Result<QueryResult, String>) {
        // Stale result (shouldn't happen in v1 since we only allow one query
        // in flight, but defend anyway).
        if let Current::Running { id: current_id, .. } = &self.current {
            if *current_id != id {
                return;
            }
        }

        // Fill in the row count / elapsed on the most recent history entry
        // (the one on_submit just appended).
        match &result {
            Ok(data) => {
                if let Some(last) = self.history.last_mut() {
                    last.elapsed_ms = data.elapsed_ms;
                    last.row_count = Some(data.rows.len());
                    last.error = None;
                }
                self.current = Current::Result {
                    sql,
                    data: result.unwrap(),
                };
            }
            Err(e) => {
                let msg = e.clone();
                if let Some(last) = self.history.last_mut() {
                    last.error = Some(msg.clone());
                    last.row_count = None;
                }
                self.current = Current::Error { sql, message: msg };
            }
        }
    }

    fn navigate_history(&mut self, code: KeyCode) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.recall_idx, code) {
            (None, KeyCode::Up) => Some(self.history.len() - 1),
            (Some(0), KeyCode::Up) => Some(0),
            (Some(idx), KeyCode::Up) => Some(idx - 1),
            (None, KeyCode::Down) => None,
            (Some(idx), KeyCode::Down) => {
                if idx + 1 >= self.history.len() {
                    None
                } else {
                    Some(idx + 1)
                }
            }
            _ => return,
        };
        self.recall_idx = next;
        self.editor = match next {
            Some(idx) => editor_with(self.history[idx].sql.as_str()),
            None => fresh_editor(),
        };
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6),                  // history
                Constraint::Min(3),                     // result
                Constraint::Length(INPUT_HEIGHT),       // textarea (content + borders)
            ])
            .split(area);

        self.render_history(frame, chunks[0]);
        self.render_result(frame, chunks[1]);
        self.render_input(frame, chunks[2]);
    }

    fn render_history(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" History ", Style::default().fg(theme::dim())));
        let items: Vec<ListItem> = self
            .history
            .iter()
            .rev()
            .take(10)
            .enumerate()
            .map(|(i, h)| {
                let idx_from_end = self.history.len() - 1 - i;
                let marker = if self.recall_idx == Some(idx_from_end) {
                    "▶ "
                } else {
                    "  "
                };
                let meta = match (&h.error, h.row_count, h.elapsed_ms) {
                    (Some(_), _, _) => "ERR".to_string(),
                    (None, Some(n), Some(ms)) => format!("{n} rows · {ms:.1} ms"),
                    (None, Some(n), None) => format!("{n} rows"),
                    (None, None, _) => "…".into(),
                };
                let meta_width = 20;
                let sql_line = truncate(h.sql.replace('\n', " ").as_str(), 200);
                ListItem::new(Line::from(vec![
                    Span::styled(marker, Style::default().fg(theme::accent())),
                    Span::styled(
                        format!("{:<meta_width$} ", meta, meta_width = meta_width),
                        Style::default().fg(theme::dim()),
                    ),
                    Span::raw(sql_line),
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
                let p = Paragraph::new("type PerfettoSQL and press Ctrl+Enter to run")
                    .block(block)
                    .style(Style::default().fg(theme::dim()));
                frame.render_widget(p, area);
            }
            Current::Running { sql, .. } => {
                let p = Paragraph::new(format!("running: {}", truncate(sql, 200)))
                    .block(block)
                    .wrap(Wrap { trim: true })
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

        let header = TableRow::new(
            data.columns
                .iter()
                .map(|c| c.clone())
                .collect::<Vec<_>>(),
        )
        .style(
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

        let table = Table::new(body, widths).header(header).block(
            block.title(Span::styled(
                format!("{title}{elapsed}"),
                Style::default().fg(theme::dim()),
            )),
        );
        frame.render_widget(table, area);
    }

    fn render_input(&self, frame: &mut Frame, area: Rect) {
        // Render the textarea. Title carries the submit hint because the
        // global footer is already dense.
        frame.render_widget(&self.editor, area);
    }
}

fn fresh_editor() -> TextArea<'static> {
    let mut ta = TextArea::default();
    configure_editor(&mut ta);
    ta
}

fn editor_with(text: &str) -> TextArea<'static> {
    let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let mut ta = TextArea::new(lines);
    configure_editor(&mut ta);
    ta
}

fn configure_editor(ta: &mut TextArea<'static>) {
    ta.set_block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                " SQL · Ctrl+Enter to run ",
                Style::default().fg(theme::dim()),
            )),
    );
    ta.set_cursor_line_style(Style::default());
    ta.set_line_number_style(Style::default().fg(theme::dim()));
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

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn with_mods(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        with_mods(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn plain_enter_inserts_newline_not_submit() {
        let mut r = ReplState::new();
        r.on_key(press(KeyCode::Char('a')));
        let out = r.on_key(press(KeyCode::Enter));
        assert!(matches!(out, KeyOutcome::None));
        // After Enter the buffer should span two lines.
        assert_eq!(r.editor.lines(), vec!["a", ""]);
    }

    #[test]
    fn ctrl_enter_submits_multiline() {
        let mut r = ReplState::new();
        r.on_key(press(KeyCode::Char('S')));
        r.on_key(press(KeyCode::Char('E')));
        r.on_key(press(KeyCode::Char('L')));
        r.on_key(press(KeyCode::Enter));
        r.on_key(press(KeyCode::Char('1')));
        match r.on_key(ctrl(KeyCode::Enter)) {
            KeyOutcome::Submit(sql) => {
                assert_eq!(sql, "SEL\n1");
            }
            _ => panic!("expected submit"),
        }
        // And the editor resets.
        assert!(r.is_editor_empty());
    }

    #[test]
    fn alt_enter_also_submits() {
        let mut r = ReplState::new();
        r.on_key(press(KeyCode::Char('x')));
        match r.on_key(with_mods(KeyCode::Enter, KeyModifiers::ALT)) {
            KeyOutcome::Submit(sql) => assert_eq!(sql, "x"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn submit_with_whitespace_only_ignored() {
        let mut r = ReplState::new();
        r.on_key(press(KeyCode::Char(' ')));
        r.on_key(press(KeyCode::Enter));
        let out = r.on_key(ctrl(KeyCode::Enter));
        assert!(matches!(out, KeyOutcome::None));
    }

    #[test]
    fn ctrl_u_clears_input() {
        let mut r = ReplState::new();
        r.on_key(press(KeyCode::Char('a')));
        r.on_key(press(KeyCode::Enter));
        r.on_key(press(KeyCode::Char('b')));
        r.on_key(ctrl(KeyCode::Char('u')));
        assert!(r.is_editor_empty());
    }

    #[test]
    fn up_arrow_recalls_history_when_input_empty() {
        let mut r = ReplState::new();
        r.on_submit(1, "SELECT 1".into());
        r.on_submit(2, "SELECT 2\nFROM slice".into());
        let _ = r.on_key(press(KeyCode::Up));
        assert_eq!(r.editor.lines(), vec!["SELECT 2", "FROM slice"]);
        let _ = r.on_key(press(KeyCode::Up));
        assert_eq!(r.editor.lines(), vec!["SELECT 1"]);
        // Clear recall → empty
        r.on_key(ctrl(KeyCode::Char('u')));
        assert!(r.is_editor_empty());
    }

    #[test]
    fn up_arrow_with_text_moves_cursor_not_history() {
        let mut r = ReplState::new();
        r.on_submit(1, "OLD".into());
        // Type something so input is non-empty.
        r.on_key(press(KeyCode::Char('x')));
        // Up should go to textarea (cursor movement), NOT trigger history.
        let _ = r.on_key(press(KeyCode::Up));
        // Editor still contains "x" (cursor may have moved but we didn't
        // recall history).
        assert_eq!(r.editor.lines(), vec!["x"]);
        assert!(r.recall_idx.is_none());
    }

    #[test]
    fn shift_arrow_scrolls_result() {
        let mut r = ReplState::new();
        let _ = r.on_key(with_mods(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(r.scroll, 1);
        let _ = r.on_key(with_mods(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(r.scroll, 0);
    }

    #[test]
    fn esc_clears_input() {
        let mut r = ReplState::new();
        r.on_key(press(KeyCode::Char('a')));
        r.on_key(press(KeyCode::Char('b')));
        r.on_key(press(KeyCode::Esc));
        assert!(r.is_editor_empty());
    }

    #[test]
    fn paste_inserts_multiline_atomically() {
        let mut r = ReplState::new();
        r.on_paste("SELECT ts, dur, name\nFROM slice\nLIMIT 10");
        assert_eq!(
            r.editor.lines(),
            vec!["SELECT ts, dur, name", "FROM slice", "LIMIT 10"]
        );
    }

    #[test]
    fn paste_after_recall_clears_recall_marker() {
        let mut r = ReplState::new();
        r.on_submit(1, "SELECT 1".into());
        // Enter recall mode.
        let _ = r.on_key(press(KeyCode::Up));
        assert!(r.recall_idx.is_some());
        r.on_paste(" AS n");
        // We no longer treat the editor as recalled content.
        assert!(r.recall_idx.is_none());
    }
}
