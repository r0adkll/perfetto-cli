//! Two-trace diff screen: load two captures in parallel, run the Summary
//! pipeline against each, and show a side-by-side diff of every metric.
//!
//! This screen reuses the [`crate::tui::screens::analysis`] machinery
//! entirely — same `SummaryState`, same queries, same soft-fail behaviour.
//! The only novelty is a `DiffSide` discriminator on the event stream and
//! a row-oriented comparison renderer (see [`row`]).

mod row;
mod worker;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Wrap};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Paths;
use crate::db::Database;
use crate::perfetto::capture::Cancel;
use crate::trace_processor::LoadProgress;
use crate::tui::chrome;
use crate::tui::event::{AppEvent, DiffSide};
use crate::tui::screens::analysis::summary::SummaryState;
use crate::tui::screens::analysis::worker::{AnalysisEvent, WorkerRequest};
use crate::tui::theme;

use worker::spawn_diff_worker;

pub enum DiffAction {
    None,
    Back,
}

pub struct DiffScreen {
    #[allow(dead_code)]
    db: Database,
    #[allow(dead_code)]
    paths: Paths,
    package_name: String,
    session_id: i64,
    left: Side,
    right: Side,
    cancel: Arc<Cancel>,
    status: theme::Status,
    error: Option<String>,
}

struct Side {
    #[allow(dead_code)]
    trace_path: PathBuf,
    display_name: String,
    captured_at: String,
    worker_tx: Option<UnboundedSender<WorkerRequest>>,
    state: SideState,
}

enum SideState {
    Preparing { phase: Phase },
    Ready { summary: SummaryState },
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

impl DiffScreen {
    pub fn new(
        db: Database,
        paths: Paths,
        session_id: i64,
        package_name: String,
        left_trace: PathBuf,
        left_name: String,
        right_trace: PathBuf,
        right_name: String,
        app_tx: UnboundedSender<AppEvent>,
    ) -> Self {
        let cancel = Cancel::new();

        let left_captured = captured_at_string(&left_trace);
        let right_captured = captured_at_string(&right_trace);

        let left_tx = spawn_diff_worker(
            DiffSide::Left,
            paths.clone(),
            left_trace.clone(),
            cancel.clone(),
            app_tx.clone(),
            package_name.clone(),
        );
        let right_tx = spawn_diff_worker(
            DiffSide::Right,
            paths.clone(),
            right_trace.clone(),
            cancel.clone(),
            app_tx,
            package_name.clone(),
        );

        Self {
            db,
            paths,
            package_name,
            session_id,
            left: Side {
                trace_path: left_trace,
                display_name: left_name,
                captured_at: left_captured,
                worker_tx: Some(left_tx),
                state: SideState::Preparing { phase: Phase::Spawning },
            },
            right: Side {
                trace_path: right_trace,
                display_name: right_name,
                captured_at: right_captured,
                worker_tx: Some(right_tx),
                state: SideState::Preparing { phase: Phase::Spawning },
            },
            cancel,
            status: theme::Status::default(),
            error: None,
        }
    }

    pub fn session_id(&self) -> i64 {
        self.session_id
    }

    pub fn set_status(&mut self, msg: String) {
        self.status.set(msg);
        self.error = None;
    }

    #[allow(dead_code)]
    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    pub fn on_event(&mut self, side: DiffSide, event: AnalysisEvent) {
        let target = match side {
            DiffSide::Left => &mut self.left,
            DiffSide::Right => &mut self.right,
        };
        apply_side_event(target, &self.package_name, event);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> DiffAction {
        if key.kind != KeyEventKind::Press {
            return DiffAction::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => DiffAction::Back,
            KeyCode::Char('r') => {
                // Reset both sides' summary cells; the worker re-runs the
                // same RunSummary request to repopulate. Has no effect on
                // sides still in Preparing / Failed.
                if let SideState::Ready { summary, .. } = &mut self.left.state {
                    summary.reset();
                    if let Some(tx) = &self.left.worker_tx {
                        let _ = tx.send(WorkerRequest::RunSummary);
                    }
                }
                if let SideState::Ready { summary, .. } = &mut self.right.state {
                    summary.reset();
                    if let Some(tx) = &self.right.worker_tx {
                        let _ = tx.send(WorkerRequest::RunSummary);
                    }
                }
                self.set_status("refreshing diff".into());
                DiffAction::None
            }
            _ => DiffAction::None,
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Length(3),   // side banner
                Constraint::Min(3),      // body — progress or diff table
                Constraint::Length(1),   // status/error
                Constraint::Length(1),   // footer
            ])
            .split(area);

        let subtitle = Line::from(vec![
            Span::styled("Diff · ", theme::hint()),
            Span::styled(
                self.package_name.clone(),
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(chrome::app_header(subtitle), chunks[0]);

        self.render_side_banner(frame, chunks[1]);
        self.render_body(frame, chunks[2]);
        self.render_status(frame, chunks[3]);
        self.render_footer(frame, chunks[4]);
    }

    fn render_side_banner(&self, frame: &mut Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        self.render_side_label(frame, cols[0], &self.left, "A", DiffSide::Left);
        self.render_side_label(frame, cols[1], &self.right, "B", DiffSide::Right);
    }

    fn render_side_label(&self, frame: &mut Frame, area: Rect, side: &Side, tag: &str, _which: DiffSide) {
        let dim = Style::default().fg(theme::dim());
        let value = Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD);
        let state_word = match &side.state {
            SideState::Preparing { .. } => Span::styled("loading", dim),
            SideState::Ready { .. } => Span::styled("ready", Style::default().fg(theme::ok())),
            SideState::Failed(_) => Span::styled("failed", Style::default().fg(theme::err())),
        };
        let line = Line::from(vec![
            Span::styled(format!("{tag}  "), dim),
            Span::styled(side.display_name.clone(), value),
            Span::styled("   ", dim),
            Span::styled(side.captured_at.clone(), dim),
            Span::styled("   ", dim),
            state_word,
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dim)
            .title(Span::styled(format!(" Side {tag} "), dim));
        let para = Paragraph::new(line).block(block).alignment(Alignment::Left);
        frame.render_widget(para, area);
    }

    fn render_body(&mut self, frame: &mut Frame, area: Rect) {
        let either_preparing = matches!(&self.left.state, SideState::Preparing { .. })
            || matches!(&self.right.state, SideState::Preparing { .. });
        let either_failed =
            matches!(&self.left.state, SideState::Failed(_)) || matches!(&self.right.state, SideState::Failed(_));

        if either_failed {
            self.render_failure_body(frame, area);
            return;
        }
        if either_preparing {
            self.render_progress_body(frame, area);
            return;
        }
        self.render_diff_table(frame, area);
    }

    fn render_progress_body(&self, frame: &mut Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        render_side_progress(frame, cols[0], &self.left, "A");
        render_side_progress(frame, cols[1], &self.right, "B");
    }

    fn render_failure_body(&self, frame: &mut Frame, area: Rect) {
        let errors: Vec<Line> = [&self.left, &self.right]
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match &s.state {
                SideState::Failed(msg) => Some(Line::from(Span::styled(
                    format!("side {}: {msg}", if i == 0 { "A" } else { "B" }),
                    Style::default().fg(theme::err()),
                ))),
                _ => None,
            })
            .collect();
        let mut lines = vec![
            Line::from(Span::styled(
                "one or both traces failed to load",
                Style::default()
                    .fg(theme::err())
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        lines.extend(errors);
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "press q to go back",
            theme::hint(),
        )));

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::err()));
        let body = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
        frame.render_widget(body, area);
    }

    fn render_diff_table(&self, frame: &mut Frame, area: Rect) {
        let left_summary = match &self.left.state {
            SideState::Ready { summary } => summary,
            _ => return,
        };
        let right_summary = match &self.right.state {
            SideState::Ready { summary } => summary,
            _ => return,
        };
        row::render_diff_table(frame, area, left_summary, right_summary);
    }

    fn render_status(&mut self, frame: &mut Frame, area: Rect) {
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

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let hints = ["[r] refresh", "[q] back"];
        let spans: Vec<Span> = hints
            .iter()
            .enumerate()
            .flat_map(|(i, h)| {
                let sep = if i == 0 { "" } else { "  " };
                vec![
                    Span::styled(sep, theme::hint()),
                    Span::styled(*h, theme::hint()),
                ]
            })
            .collect();
        frame.render_widget(
            Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
            area,
        );
    }
}

impl Drop for DiffScreen {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.left.worker_tx.take();
        self.right.worker_tx.take();
    }
}

fn apply_side_event(target: &mut Side, _package_name: &str, event: AnalysisEvent) {
    match event {
        AnalysisEvent::LoadProgress(p) => {
            if let SideState::Preparing { phase } = &mut target.state {
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
        AnalysisEvent::LoadReady { .. } => {
            target.state = SideState::Ready {
                summary: SummaryState::new(
                    target.display_name.clone(),
                    target.captured_at.clone(),
                ),
            };
            if let Some(tx) = &target.worker_tx {
                let _ = tx.send(WorkerRequest::RunSummary);
            }
        }
        AnalysisEvent::LoadFailed(msg) => {
            target.state = SideState::Failed(msg);
        }
        AnalysisEvent::SummaryCell { key, result } => {
            if let SideState::Ready { summary } = &mut target.state {
                summary.on_cell(key, result);
            }
        }
        AnalysisEvent::SummaryRows { key, result } => {
            if let SideState::Ready { summary } = &mut target.state {
                summary.on_rows(key, result);
            }
        }
        AnalysisEvent::QueryResult { .. } => {
            // Diff doesn't use the REPL; QueryResult events shouldn't
            // arrive here, but ignore defensively.
        }
    }
}

fn render_side_progress(frame: &mut Frame, area: Rect, side: &Side, tag: &str) {
    let (label, ratio) = match &side.state {
        SideState::Preparing { phase } => match phase {
            Phase::Spawning => (format!("{tag}: starting…"), 0.05),
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
                        "{tag}: {:.1} / {:.1} MB",
                        *bytes_so_far as f64 / 1024.0 / 1024.0,
                        *total_bytes as f64 / 1024.0 / 1024.0,
                    ),
                    ratio,
                )
            }
            Phase::Finalizing => (format!("{tag}: finalising…"), 1.0),
        },
        SideState::Ready { .. } => (format!("{tag}: ready"), 1.0),
        SideState::Failed(e) => (format!("{tag}: failed — {e}"), 0.0),
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
                .title(Span::styled(format!(" Loading {tag} "), theme::hint())),
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

/// Parse `YYYY-MM-DD_HH-MM-SS.pftrace` into `"YYYY-MM-DD HH:MM:SS UTC"`.
/// Fallback to the filename stem when the pattern doesn't match (e.g. a
/// user-renamed trace).
fn captured_at_string(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if let Some((date, time)) = stem.split_once('_') {
        let date_parts: Vec<&str> = date.split('-').collect();
        let time_parts: Vec<&str> = time.split('-').collect();
        if date_parts.len() == 3
            && date_parts[0].len() == 4
            && time_parts.len() == 3
            && date_parts
                .iter()
                .chain(time_parts.iter())
                .all(|p| p.chars().all(|c| c.is_ascii_digit()))
        {
            let time = time.replace('-', ":");
            return format!("{date} {time} UTC");
        }
    }
    stem.to_string()
}
