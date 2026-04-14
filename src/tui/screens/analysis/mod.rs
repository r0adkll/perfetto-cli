//! Analysis screen: SQL-backed trace insights.
//!
//! The screen owns a background [`worker`] that spawns `trace_processor_shell`
//! under the hood. Two tabs sit on top: a pre-canned summary and a raw SQL
//! REPL.

mod repl;
pub(crate) mod summary;
pub(crate) mod worker;

// Re-exported for the client.rs smoke test to share the soft-fail
// heuristic rather than duplicating it.
#[cfg(test)]
pub(crate) use worker::is_missing_table;

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Tabs, Wrap};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Paths;
use crate::db::Database;
use crate::perfetto::capture::Cancel;
use crate::trace_processor::LoadProgress;
use crate::tui::chrome;
use crate::tui::event::AppEvent;
use crate::tui::theme;

pub use worker::AnalysisEvent;

use repl::{KeyOutcome as ReplOutcome, ReplState};
use summary::SummaryState;
use worker::{CustomQuery, WorkerRequest, spawn_worker};

/// Action the screen asks the `App` router to perform. Everything else is
/// handled internally.
pub enum AnalysisAction {
    None,
    Back,
    /// User pressed `o`: re-open this trace in `ui.perfetto.dev`. The path is
    /// already known to the screen; we let `app.rs` reuse its existing
    /// `open_trace` helper so startup-command handoff and the local UI server
    /// don't need to be duplicated here.
    OpenInBrowser(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Summary,
    Repl,
}

impl Tab {
    fn index(self) -> usize {
        match self {
            Tab::Summary => 0,
            Tab::Repl => 1,
        }
    }
    fn labels() -> [&'static str; 2] {
        ["Summary", "SQL"]
    }
    fn toggle(self) -> Self {
        match self {
            Tab::Summary => Tab::Repl,
            Tab::Repl => Tab::Summary,
        }
    }
}

enum State {
    Preparing {
        phase: Phase,
    },
    Ready {
        version: Option<String>,
        summary: SummaryState,
        repl: ReplState,
    },
    Failed(String),
}

#[derive(Debug, Clone)]
enum Phase {
    Spawning,
    Parsing {
        bytes_so_far: u64,
        total_bytes: u64,
    },
    Finalizing,
}

pub struct AnalysisScreen {
    #[allow(dead_code)]
    db: Database,
    #[allow(dead_code)]
    paths: Paths,
    trace_path: PathBuf,
    session_id: i64,
    package_name: String,
    worker_tx: Option<UnboundedSender<WorkerRequest>>,
    cancel: Arc<Cancel>,
    state: State,
    tab: Tab,
    status: theme::Status,
    error: Option<String>,
    next_query_id: u64,
}

impl AnalysisScreen {
    pub fn new(
        db: Database,
        paths: Paths,
        trace_path: PathBuf,
        session_id: i64,
        app_tx: UnboundedSender<AppEvent>,
        package_name: String,
    ) -> Self {
        let cancel = Cancel::new();
        let worker_tx = spawn_worker(
            paths.clone(),
            trace_path.clone(),
            cancel.clone(),
            app_tx,
            package_name.clone(),
            |ev| AppEvent::Analysis(ev),
        );

        Self {
            db,
            paths,
            trace_path,
            session_id,
            package_name,
            worker_tx: Some(worker_tx),
            cancel,
            state: State::Preparing {
                phase: Phase::Spawning,
            },
            tab: Tab::Summary,
            status: theme::Status::default(),
            error: None,
            next_query_id: 1,
        }
    }

    pub fn session_id(&self) -> i64 {
        self.session_id
    }

    pub fn set_status(&mut self, msg: String) {
        self.status.set(msg);
        self.error = None;
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    /// Deliver a bracketed-paste payload to the active tab. Only meaningful
    /// on the REPL tab today — the Summary tab has no text input.
    pub fn on_paste(&mut self, text: &str) {
        if let State::Ready { repl, .. } = &mut self.state {
            if self.tab == Tab::Repl {
                repl.on_paste(text);
            }
        }
    }

    pub fn on_event(&mut self, ev: AnalysisEvent) {
        match ev {
            AnalysisEvent::LoadProgress(p) => self.apply_progress(p),
            AnalysisEvent::LoadReady { version } => {
                let captured_at = parse_captured_at_from_filename(&self.trace_path)
                    .unwrap_or_else(|| "—".into());
                let custom_queries = self.load_custom_queries();
                self.state = State::Ready {
                    version,
                    summary: SummaryState::new(
                        self.package_name.clone(),
                        captured_at,
                        custom_queries.clone(),
                    ),
                    repl: ReplState::new(),
                };
                // Kick off the summary immediately so the default tab fills in.
                if let Some(tx) = &self.worker_tx {
                    let _ = tx.send(WorkerRequest::RunSummary { custom_queries });
                }
            }
            AnalysisEvent::LoadFailed(msg) => {
                self.state = State::Failed(msg);
            }
            AnalysisEvent::SummaryCell { key, result } => {
                if let State::Ready { summary, .. } = &mut self.state {
                    summary.on_cell(key, result);
                }
            }
            AnalysisEvent::SummaryRows { key, result } => {
                if let State::Ready { summary, .. } = &mut self.state {
                    summary.on_rows(key, result);
                }
            }
            AnalysisEvent::QueryResult { id, sql, result } => {
                if let State::Ready { repl, .. } = &mut self.state {
                    repl.on_result(id, sql, result);
                }
            }
            AnalysisEvent::CustomResult { name, result } => {
                if let State::Ready { summary, .. } = &mut self.state {
                    summary.on_custom_result(name, result);
                }
            }
        }
    }

    /// Read the current package's saved queries from the DB. Silent on
    /// error (empty vec) because a dashboard-population failure
    /// shouldn't block the trace load — the error would surface on
    /// subsequent SQL submits anyway.
    fn load_custom_queries(&self) -> Vec<CustomQuery> {
        self.db
            .list_saved_queries(&self.package_name)
            .unwrap_or_default()
            .into_iter()
            .map(|sq| CustomQuery {
                name: sq.name,
                sql: sq.sql,
            })
            .collect()
    }

    fn apply_progress(&mut self, p: LoadProgress) {
        if let State::Preparing { phase } = &mut self.state {
            *phase = match p {
                LoadProgress::Parse {
                    bytes_so_far,
                    total_bytes,
                } => Phase::Parsing {
                    bytes_so_far,
                    total_bytes,
                },
                LoadProgress::Finalized => Phase::Finalizing,
            };
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> AnalysisAction {
        if key.kind != KeyEventKind::Press {
            return AnalysisAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // "Text focus" means a text input on the active tab wants to consume
        // every printable character. Right now only the REPL has one. In this
        // mode, single-char quick keys (q, o, 1, 2, r) are disabled so the
        // user can type them into SQL — we require `Ctrl+…` chords instead.
        // Tab/BackTab still switch tabs because users pasting indented SQL
        // don't usually also type literal tabs.
        let text_focused = matches!(&self.state, State::Ready { .. }) && self.tab == Tab::Repl;

        // Tab switching works in any Ready state.
        if matches!(self.state, State::Ready { .. }) {
            match key.code {
                KeyCode::Tab if !shift => {
                    self.tab = self.tab.toggle();
                    return AnalysisAction::None;
                }
                KeyCode::BackTab => {
                    self.tab = self.tab.toggle();
                    return AnalysisAction::None;
                }
                // Digit tab switches only when a text input isn't focused.
                KeyCode::Char('1') if !text_focused => {
                    self.tab = Tab::Summary;
                    return AnalysisAction::None;
                }
                KeyCode::Char('2') if !text_focused => {
                    self.tab = Tab::Repl;
                    return AnalysisAction::None;
                }
                _ => {}
            }
        }

        // Exit / open-in-browser. When text input is focused the plain
        // single-char versions would clobber typing, so require a Ctrl chord.
        // `Ctrl+C` also maps to Back for muscle memory from terminals.
        if text_focused {
            if ctrl {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Char('c') => {
                        return AnalysisAction::Back;
                    }
                    KeyCode::Char('o') => {
                        return AnalysisAction::OpenInBrowser(self.trace_path.clone());
                    }
                    _ => {}
                }
            }
        } else {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return AnalysisAction::Back,
                KeyCode::Char('o') => {
                    return AnalysisAction::OpenInBrowser(self.trace_path.clone());
                }
                _ => {}
            }
        }

        // Tab-specific routing.
        match (&mut self.state, self.tab) {
            (State::Ready { .. }, Tab::Summary) => {
                if key.code == KeyCode::Char('r') {
                    let custom_queries = self.load_custom_queries();
                    if let State::Ready { summary, .. } = &mut self.state {
                        summary.reset(custom_queries.clone());
                        if let Some(tx) = &self.worker_tx {
                            let _ = tx.send(WorkerRequest::RunSummary { custom_queries });
                        }
                        self.set_status("refreshing summary".into());
                    }
                }
                AnalysisAction::None
            }
            (State::Ready { repl, .. }, Tab::Repl) => {
                // REPL sees everything not intercepted above, including Esc
                // (which clears the input buffer).
                let outcome = repl.on_key(key);
                // Surface any `:save` validation error from the REPL before
                // processing the outcome, so the status line updates on the
                // same tick.
                let cmd_err = repl.take_command_error();
                if let Some(err) = cmd_err {
                    self.set_error(err);
                }
                match outcome {
                    ReplOutcome::Submit(sql) => {
                        let id = self.next_query_id;
                        self.next_query_id += 1;
                        if let State::Ready { repl, .. } = &mut self.state {
                            repl.on_submit(id, sql.clone());
                        }
                        if let Some(tx) = &self.worker_tx {
                            let _ = tx.send(WorkerRequest::RunQuery { id, sql });
                        }
                    }
                    ReplOutcome::SaveQuery { name, sql } => {
                        match self.db.upsert_saved_query(&self.package_name, &name, &sql) {
                            Ok(_) => {
                                self.set_status(format!("saved as `{name}`"));
                                // Refresh the custom-metrics section so the new
                                // query renders immediately.
                                let custom_queries = self.load_custom_queries();
                                if let State::Ready { summary, .. } = &mut self.state {
                                    summary.reset_custom(custom_queries.clone());
                                }
                                if let Some(tx) = &self.worker_tx {
                                    let _ = tx.send(WorkerRequest::RunSummary { custom_queries });
                                }
                            }
                            Err(e) => self.set_error(format!("save failed: {e:#}")),
                        }
                    }
                    ReplOutcome::None => {}
                }
                AnalysisAction::None
            }
            _ => AnalysisAction::None,
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        let subtitle = Line::from(vec![
            Span::styled("Analysis · ", theme::hint()),
            Span::styled(
                trace_filename(&self.trace_path),
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(self.version_label(), theme::hint()),
        ]);
        frame.render_widget(chrome::app_header(subtitle), chunks[0]);

        self.render_tab_strip(frame, chunks[1]);
        self.render_body(frame, chunks[2]);
        self.render_status_line(frame, chunks[3]);
        self.render_footer(frame, chunks[4]);
    }

    fn version_label(&self) -> String {
        match &self.state {
            State::Preparing { .. } => "(loading)".into(),
            State::Failed(_) => "(failed)".into(),
            State::Ready { version, .. } => version
                .as_deref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "(ready)".into()),
        }
    }

    fn render_tab_strip(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let labels = Tab::labels();
        let titles = labels
            .iter()
            .map(|l| Line::from(Span::raw(*l)))
            .collect::<Vec<_>>();
        let tabs = Tabs::new(titles)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::dim())),
            )
            .select(self.tab.index())
            .style(Style::default().fg(theme::dim()))
            .highlight_style(
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(tabs, area);
    }

    fn render_body(&mut self, frame: &mut Frame, area: ratatui::layout::Rect) {
        match &mut self.state {
            State::Preparing { phase } => render_preparing(frame, area, phase),
            State::Failed(msg) => {
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::err()));
                let body = Paragraph::new(vec![
                    Line::from(Span::styled(
                        "trace_processor failed to start",
                        Style::default()
                            .fg(theme::err())
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(msg.clone()),
                    Line::from(""),
                    Line::from(Span::styled(
                        "press q to go back",
                        theme::hint(),
                    )),
                ])
                .block(block)
                .wrap(Wrap { trim: true });
                frame.render_widget(body, area);
            }
            State::Ready { summary, repl, .. } => match self.tab {
                Tab::Summary => summary.render(frame, area),
                Tab::Repl => repl.render(frame, area),
            },
        }
    }

    fn render_status_line(&mut self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let line = if let Some(err) = &self.error {
            Line::from(Span::styled(
                format!("! {err}"),
                Style::default().fg(theme::err()),
            ))
        } else if let Some(msg) = self.status.get() {
            Line::from(Span::styled(
                msg.to_string(),
                Style::default().fg(theme::accent_secondary()),
            ))
        } else {
            Line::from("")
        };
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_footer(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        // Each hint is a (chord, verb) pair. Chords render in
        // `theme::title()` (accent + bold) to match the rest of the app's
        // footer style (see `session_detail::render`); verbs render in
        // the default style.
        let hints: &[(&str, &str)] = match (&self.state, self.tab) {
            (State::Ready { .. }, Tab::Summary) => &[
                ("[1/2]", " tab  "),
                ("[r]", " refresh  "),
                ("[o]", " open in UI  "),
                ("[q]", " back"),
            ],
            (State::Ready { .. }, Tab::Repl) => &[
                ("[Tab]", " switch pane  "),
                ("[Alt+Enter]", " run  "),
                ("[↑/↓]", " history (empty)  "),
                ("[Shift+↑/↓]", " scroll  "),
                ("[Ctrl+O]", " open in UI  "),
                ("[Ctrl+Q]", " back"),
            ],
            _ => &[("[o]", " open in UI  "), ("[q]", " back")],
        };
        let mut spans: Vec<Span> = Vec::with_capacity(hints.len() * 2 + 1);
        spans.push(Span::raw(" "));
        for (chord, verb) in hints {
            spans.push(Span::styled(*chord, theme::title()));
            spans.push(Span::raw(*verb));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
            area,
        );
    }
}

impl Drop for AnalysisScreen {
    fn drop(&mut self) {
        // Trigger cancellation first so any in-flight load bails immediately,
        // then drop the sender so the worker exits via rx.recv() returning
        // None and calls TraceProcessor::shutdown.
        self.cancel.cancel();
        self.worker_tx.take();
    }
}

fn render_preparing(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    phase: &Phase,
) {
    let (label, ratio) = match phase {
        Phase::Spawning => ("starting trace_processor…".to_string(), 0.05),
        Phase::Parsing {
            bytes_so_far,
            total_bytes,
        } => {
            let ratio = if *total_bytes == 0 {
                0.0
            } else {
                (*bytes_so_far as f64 / *total_bytes as f64).clamp(0.0, 1.0)
            };
            (
                format!(
                    "parsing trace · {:.1} / {:.1} MB",
                    *bytes_so_far as f64 / 1024.0 / 1024.0,
                    *total_bytes as f64 / 1024.0 / 1024.0,
                ),
                ratio,
            )
        }
        Phase::Finalizing => ("finalising…".to_string(), 1.0),
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" Loading trace ", theme::hint())),
        )
        .gauge_style(
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )
        .ratio(ratio)
        .label(format!("{:>3.0}%", (ratio * 100.0).clamp(0.0, 100.0)));
    frame.render_widget(gauge, rows[1]);

    let msg = Paragraph::new(Line::from(Span::styled(label, theme::hint())))
        .alignment(Alignment::Center);
    frame.render_widget(msg, rows[2]);
}

fn trace_filename(p: &std::path::Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| p.display().to_string())
}

/// Parse the standard trace filename (`YYYY-MM-DD_HH-MM-SS.pftrace`,
/// generated in UTC by `src/perfetto/capture.rs`) into a human display
/// string. Returns `None` for user-renamed files that no longer match the
/// scheme; the caller falls back to `"—"`.
fn parse_captured_at_from_filename(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let (date, time) = stem.split_once('_')?;

    // Date must be YYYY-MM-DD.
    let date_parts: Vec<&str> = date.split('-').collect();
    if date_parts.len() != 3 || date_parts[0].len() != 4 {
        return None;
    }
    // Time must be HH-MM-SS.
    let time_parts: Vec<&str> = time.split('-').collect();
    if time_parts.len() != 3 {
        return None;
    }
    if !date_parts
        .iter()
        .chain(time_parts.iter())
        .all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }

    let time = time.replace('-', ":");
    Some(format!("{date} {time} UTC"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_captured_at_roundtrips_standard_filename() {
        let p = Path::new("/x/y/2026-04-13_18-32-05.pftrace");
        assert_eq!(
            parse_captured_at_from_filename(p).as_deref(),
            Some("2026-04-13 18:32:05 UTC"),
        );
    }

    #[test]
    fn parse_captured_at_rejects_non_standard_names() {
        assert!(parse_captured_at_from_filename(Path::new("foo.pftrace")).is_none());
        assert!(parse_captured_at_from_filename(Path::new("my_trace.pftrace")).is_none());
        assert!(
            parse_captured_at_from_filename(Path::new("2026-04-13.pftrace")).is_none(),
            "missing time segment should not parse"
        );
        // Wrong-length year.
        assert!(
            parse_captured_at_from_filename(Path::new("26-04-13_18-32-05.pftrace")).is_none()
        );
        // Non-digit where a digit is expected.
        assert!(
            parse_captured_at_from_filename(Path::new("2026-AA-13_18-32-05.pftrace")).is_none()
        );
    }
}
