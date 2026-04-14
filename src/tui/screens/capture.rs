use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Wrap};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::perfetto::capture::{
    self, Cancel, CaptureEvent, CaptureRequest, CaptureResult, LogEntry, LogLevel,
};
use crate::session::Session;
use crate::tui::chrome;
use crate::tui::event::AppEvent;
use crate::tui::theme;

pub struct CaptureScreen {
    session_id: i64,
    session_name: String,
    logs: Vec<LogEntry>,
    device_pid: Option<u32>,
    target_ms: u64,
    start_time: Instant,
    stop_time: Option<Instant>,
    cancelling: bool,
    cancel: Arc<Cancel>,
    result: Option<Result<CaptureResult, String>>,
}

pub enum CaptureAction {
    None,
    Back,
}

impl CaptureScreen {
    pub fn new(session: &Session, app_tx: UnboundedSender<AppEvent>) -> Self {
        let session_id = session.id.unwrap_or(0);
        let session_name = session.name.clone();
        let target_ms = session.config.duration_ms as u64;

        let Some(device_serial) = session.device_serial.clone() else {
            return Self {
                session_id,
                session_name,
                logs: Vec::new(),
                device_pid: None,
                target_ms,
                start_time: Instant::now(),
                stop_time: Some(Instant::now()),
                cancelling: false,
                cancel: Cancel::new(),
                result: Some(Err(
                    "Session has no device. Pick one in the devices screen first.".into(),
                )),
            };
        };

        let cancel = Cancel::new();
        let request = CaptureRequest {
            session_id,
            session_folder: session.folder_path.clone(),
            device_serial,
            package_name: session.package_name.clone(),
            config: session.config.clone(),
        };

        let (cap_tx, mut cap_rx) = mpsc::unbounded_channel::<CaptureEvent>();
        let forward_tx = app_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = cap_rx.recv().await {
                if forward_tx.send(AppEvent::Capture(ev)).is_err() {
                    break;
                }
            }
        });

        let done_tx = app_tx;
        let cancel_for_task = cancel.clone();
        tokio::spawn(async move {
            let result = capture::run(request, cap_tx, cancel_for_task).await;
            let _ = done_tx.send(AppEvent::CaptureDone(result.map_err(|e| format!("{e:#}"))));
        });

        Self {
            session_id,
            session_name,
            logs: Vec::new(),
            device_pid: None,
            target_ms,
            start_time: Instant::now(),
            stop_time: None,
            cancelling: false,
            cancel,
            result: None,
        }
    }

    pub fn session_id(&self) -> i64 {
        self.session_id
    }

    pub fn on_capture_event(&mut self, event: CaptureEvent) {
        match event {
            CaptureEvent::Log(entry) => self.logs.push(entry),
            CaptureEvent::DeviceProcess(pid) => self.device_pid = Some(pid),
        }
    }

    pub fn on_done(&mut self, result: Result<CaptureResult, String>) {
        self.stop_time = Some(Instant::now());
        self.result = Some(result);
    }

    /// Append a synthetic log entry from outside the engine — used by the
    /// app to surface the auto-open outcome alongside the engine's own logs.
    pub fn push_log(&mut self, level: LogLevel, message: String) {
        self.logs.push(LogEntry { level, message });
    }

    pub fn is_done(&self) -> bool {
        self.result.is_some()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> CaptureAction {
        if key.kind != KeyEventKind::Press {
            return CaptureAction::None;
        }

        if self.is_done() {
            return match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => CaptureAction::Back,
                _ => CaptureAction::None,
            };
        }

        // While a capture is in flight, allow the user to stop it.
        let ctrl_c = matches!(
            (key.code, key.modifiers),
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL)
        );
        if ctrl_c || key.code == KeyCode::Esc {
            if !self.cancelling {
                self.cancelling = true;
                self.cancel.cancel();
            }
        }
        CaptureAction::None
    }

    fn elapsed_ms(&self) -> u64 {
        let end = self.stop_time.unwrap_or_else(Instant::now);
        end.saturating_duration_since(self.start_time).as_millis() as u64
    }

    fn progress_ratio(&self) -> f64 {
        if self.target_ms == 0 {
            return 0.0;
        }
        (self.elapsed_ms() as f64 / self.target_ms as f64).clamp(0.0, 1.0)
    }

    fn status_text(&self) -> &'static str {
        if let Some(result) = &self.result {
            return match result {
                Ok(res) if res.cancelled => "stopped early",
                Ok(_) => "complete",
                Err(_) => "failed",
            };
        }
        if self.cancelling {
            "stopping…"
        } else if self.device_pid.is_some() {
            "capturing"
        } else {
            "warming up"
        }
    }

    fn status_style(&self) -> Style {
        if let Some(result) = &self.result {
            return match result {
                Ok(res) if res.cancelled => Style::default().fg(theme::warn()),
                Ok(_) => Style::default().fg(theme::ok()),
                Err(_) => Style::default().fg(theme::err()),
            };
        }
        if self.cancelling {
            Style::default().fg(theme::warn())
        } else {
            Style::default().fg(theme::accent())
        }
    }

    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(1),
            ])
            .split(area);

        // Header
        let header = chrome::app_header(Line::from(vec![
            Span::styled("  🎬 Capture ", theme::title()),
            Span::styled(format!("— {}", self.session_name), theme::hint()),
        ]));
        frame.render_widget(header, rows[0]);

        // Status strip: state + pid + elapsed/target. Cap elapsed at target so
        // the pull/flush phase doesn't make the timer overshoot — the full
        // duration is reported in the log on completion.
        let target_s = self.target_ms as f64 / 1000.0;
        let elapsed_s = (self.elapsed_ms().min(self.target_ms)) as f64 / 1000.0;
        let pid_text = self
            .device_pid
            .map(|p| format!("  pid {p}"))
            .unwrap_or_default();
        let status_line = Line::from(vec![
            Span::raw(" "),
            Span::styled(self.status_text(), self.status_style()),
            Span::styled(pid_text, theme::hint()),
            Span::raw("   "),
            Span::styled(
                format!("{elapsed_s:.1}s / {target_s:.1}s"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(status_line)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(Span::styled(" Status ", theme::title())),
                ),
            rows[1],
        );

        // Progress bar
        let gauge_color = if self.cancelling || matches!(self.result, Some(Ok(ref r)) if r.cancelled)
        {
            theme::warn()
        } else if matches!(self.result, Some(Err(_))) {
            theme::err()
        } else {
            theme::accent()
        };
        let gauge = Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(" Progress ", theme::title())),
            )
            .gauge_style(Style::default().fg(gauge_color).bg(Color::Black))
            .ratio(self.progress_ratio())
            .label(format!(
                "{:.0}%",
                (self.progress_ratio() * 100.0).min(100.0)
            ));
        frame.render_widget(gauge, rows[2]);

        // Log
        let mut log_lines: Vec<Line> = self
            .logs
            .iter()
            .map(|entry| {
                let (icon, style) = match entry.level {
                    LogLevel::Info => ("•", Style::default().fg(theme::accent())),
                    LogLevel::Ok => ("✓", Style::default().fg(theme::ok())),
                    LogLevel::Warn => ("⚠", Style::default().fg(theme::warn())),
                    LogLevel::Err => ("✗", Style::default().fg(theme::err())),
                };
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(icon, style),
                    Span::raw(" "),
                    Span::raw(entry.message.clone()),
                ])
            })
            .collect();

        match self.result.as_ref() {
            Some(Ok(result)) => {
                log_lines.push(Line::from(""));
                let headline = if result.cancelled {
                    format!(
                        "  ⚠ Stopped early — {} KB partial trace in {:.1}s",
                        result.size_bytes / 1024,
                        result.duration_ms as f64 / 1000.0
                    )
                } else {
                    format!(
                        "  ✓ Capture complete — {} KB in {:.1}s",
                        result.size_bytes / 1024,
                        result.duration_ms as f64 / 1000.0
                    )
                };
                let color = if result.cancelled { theme::warn() } else { theme::ok() };
                log_lines.push(Line::from(Span::styled(
                    headline,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )));
                log_lines.push(Line::from(Span::styled(
                    format!("    {}", result.trace_path.display()),
                    theme::hint(),
                )));
            }
            Some(Err(msg)) => {
                log_lines.push(Line::from(""));
                log_lines.push(Line::from(Span::styled(
                    format!("  ✗ Capture failed: {msg}"),
                    Style::default().fg(theme::err()),
                )));
            }
            None => {}
        }

        let logs = Paragraph::new(log_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(" Log ", theme::title())),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(logs, rows[3]);

        // Footer help
        let footer = if self.is_done() {
            Line::from(vec![
                Span::styled(" [Enter/Esc]", theme::title()),
                Span::raw(" back to session"),
            ])
        } else if self.cancelling {
            Line::from(Span::styled(
                " stopping — waiting for perfetto to flush and exit",
                Style::default().fg(theme::warn()),
            ))
        } else {
            Line::from(vec![
                Span::styled(" [Ctrl-C / Esc]", theme::title()),
                Span::raw(" stop early"),
            ])
        };
        frame.render_widget(Paragraph::new(footer), rows[4]);
    }
}
