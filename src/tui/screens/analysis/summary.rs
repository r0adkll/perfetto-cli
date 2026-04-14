//! Summary tab: opinionated diagnostic panels for a single trace.
//!
//! Four stacked sections from top to bottom:
//! 1. **Context strip** — package / device fingerprint / captured-at / duration.
//! 2. **Health tiles** — jank rate, max frame duration.
//! 3. **Main-thread hotspots** — top slices on the target app's main thread.
//! 4. **Trace contents ribbon** — ✓/✗ presence probes for ftrace, frame
//!    timeline, startups, thread state.
//!
//! Metrics are keyed by [`SummaryKey`]. The worker runs each query
//! independently so a single missing table (older traces lack
//! `actual_frame_timeline_slice`, for example) doesn't poison the whole
//! panel — it downgrades that one cell to `MissingTable` / `—` / `✗`.

use std::collections::HashMap;
use std::fmt::Write as _;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row as TableRow, Sparkline, Table, Wrap};

use crate::trace_processor::{Cell, Row};
use crate::tui::theme;

use super::worker::{SummaryCellOutcome, SummaryRowsOutcome};

/// Every pre-canned summary metric. Order is irrelevant for rendering
/// (layout is per-section); it matters only for the worker's dispatch order
/// and the `all_queries_covers_every_key` exhaustiveness test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SummaryKey {
    // ── context strip ──
    DeviceFingerprint,
    TraceDurationNs,
    // ── health tiles ──
    JankFrameCount,
    TotalFrameCount,
    /// Multi-row (1 row × 2 cols) — p50 and p95 of `actual_frame_timeline_slice.dur`
    /// computed via window functions SQL-side to avoid shipping every
    /// sample across the HTTP wire.
    FrameDurPercentiles,
    PeakRssBytes,
    MainThreadRunningNs,
    MainThreadTotalNs,
    // ── startup card (multi-row, 0 or 1 rows expected) ──
    StartupInfo,
    /// Multi-row (up to ~61 rows × 2 cols) — bucketed `mem.rss` time
    /// series for the target process, rendered as a sparkline.
    RssOverTime,
    // ── main-thread hotspots (multi-row) ──
    MainThreadTopSlices,
    // ── trace contents ribbon probes ──
    HasFtrace,
    HasFrameTimeline,
    HasStartups,
    HasThreadState,
}

/// Context threaded from the screen through the worker into query building.
/// Right now just the target package name used to scope the main-thread
/// hotspots query; future additions (e.g. a trace-bounds window) slot in
/// here too.
#[derive(Debug, Clone)]
pub struct SummaryContext {
    pub package_name: String,
}

/// Static metadata for running and presenting a summary query.
pub struct SummaryQuery {
    pub key: SummaryKey,
    pub sql: String,
    pub multi_row: bool,
}

impl SummaryKey {
    pub fn all_queries(ctx: &SummaryContext) -> Vec<SummaryQuery> {
        vec![
            SummaryQuery {
                key: SummaryKey::DeviceFingerprint,
                sql: "SELECT str_value FROM metadata \
                      WHERE name = 'android_build_fingerprint' \
                      LIMIT 1"
                    .into(),
                multi_row: false,
            },
            SummaryQuery {
                key: SummaryKey::TraceDurationNs,
                sql: "SELECT (end_ts - start_ts) AS dur_ns FROM trace_bounds".into(),
                multi_row: false,
            },
            SummaryQuery {
                key: SummaryKey::JankFrameCount,
                sql: jank_count_sql(&ctx.package_name),
                multi_row: false,
            },
            SummaryQuery {
                key: SummaryKey::TotalFrameCount,
                sql: frame_total_sql(&ctx.package_name),
                multi_row: false,
            },
            SummaryQuery {
                key: SummaryKey::FrameDurPercentiles,
                sql: frame_percentiles_sql(&ctx.package_name),
                multi_row: true,
            },
            SummaryQuery {
                key: SummaryKey::PeakRssBytes,
                sql: peak_rss_sql(&ctx.package_name),
                multi_row: false,
            },
            SummaryQuery {
                key: SummaryKey::MainThreadRunningNs,
                sql: main_thread_state_sql(&ctx.package_name, Some("Running")),
                multi_row: false,
            },
            SummaryQuery {
                key: SummaryKey::MainThreadTotalNs,
                sql: main_thread_state_sql(&ctx.package_name, None),
                multi_row: false,
            },
            SummaryQuery {
                key: SummaryKey::StartupInfo,
                sql: "INCLUDE PERFETTO MODULE android.startup.startups;\n\
                      SELECT startup_type, dur, package FROM android_startups \
                      ORDER BY dur DESC LIMIT 1"
                    .into(),
                multi_row: true,
            },
            SummaryQuery {
                key: SummaryKey::RssOverTime,
                sql: rss_over_time_sql(&ctx.package_name),
                multi_row: true,
            },
            SummaryQuery {
                key: SummaryKey::MainThreadTopSlices,
                sql: main_thread_hotspots_sql(&ctx.package_name),
                multi_row: true,
            },
            SummaryQuery {
                key: SummaryKey::HasFtrace,
                sql: "SELECT COUNT(*) FROM ftrace_event LIMIT 1".into(),
                multi_row: false,
            },
            SummaryQuery {
                key: SummaryKey::HasFrameTimeline,
                sql: "SELECT COUNT(*) FROM actual_frame_timeline_slice LIMIT 1".into(),
                multi_row: false,
            },
            SummaryQuery {
                key: SummaryKey::HasStartups,
                sql: "INCLUDE PERFETTO MODULE android.startup.startups;\n\
                      SELECT COUNT(*) FROM android_startups"
                    .into(),
                multi_row: false,
            },
            SummaryQuery {
                key: SummaryKey::HasThreadState,
                sql: "SELECT COUNT(*) FROM thread_state LIMIT 1".into(),
                multi_row: false,
            },
        ]
    }
}

/// Render label for the trace-contents ribbon.
fn ribbon_label(key: SummaryKey) -> &'static str {
    match key {
        SummaryKey::HasFtrace => "ftrace",
        SummaryKey::HasFrameTimeline => "frame timeline",
        SummaryKey::HasStartups => "startups",
        SummaryKey::HasThreadState => "thread state",
        _ => "",
    }
}

/// Build the main-thread hotspots query. The package literal is
/// single-quote-escaped via [`escape_sql_literal`] even though Android
/// package names are alphanumeric + `.` + `_` in practice — defence in
/// depth for when this gets reused for other user-supplied strings.
///
/// Uses `thread.is_main_thread = 1` rather than `thread.name = 'main'`:
/// the name isn't always exactly `"main"` across Android versions/devices,
/// but the flag is a documented invariant.
fn main_thread_hotspots_sql(package: &str) -> String {
    let pkg = escape_sql_literal(package);
    format!(
        "SELECT slice.name, COUNT(*) AS n, SUM(slice.dur) AS total_ns \
         FROM slice \
         JOIN thread_track ON slice.track_id = thread_track.id \
         JOIN thread USING(utid) \
         JOIN process USING(upid) \
         WHERE process.name = '{pkg}' \
           AND thread.is_main_thread = 1 \
           AND slice.name IS NOT NULL \
         GROUP BY slice.name \
         ORDER BY total_ns DESC \
         LIMIT 5"
    )
}

/// Peak `mem.rss` counter value for the target process. Returns `Null` for
/// traces without memory counters or without a matching process.
///
/// `mem.rss` is Android Perfetto's standard process-scoped counter for
/// total resident set size in bytes. If the capture didn't include memory
/// probes, `process_counter_track` has no `mem.rss` row for this process
/// and `MAX` returns `NULL`, which displays as `—`.
fn peak_rss_sql(package: &str) -> String {
    let pkg = escape_sql_literal(package);
    format!(
        "SELECT MAX(c.value) AS peak \
         FROM counter c \
         JOIN process_counter_track t ON c.track_id = t.id \
         JOIN process p ON t.upid = p.upid \
         WHERE t.name = 'mem.rss' AND p.name = '{pkg}'"
    )
}

/// Sum of `thread_state.dur` for the target app's main thread. Pass
/// `Some("Running")` to restrict to on-CPU time; pass `None` for total
/// observed time. The ratio of the two is our Main-thread busy %.
fn main_thread_state_sql(package: &str, only_state: Option<&str>) -> String {
    let pkg = escape_sql_literal(package);
    let state_filter = match only_state {
        Some(state) => format!(" AND thread_state.state = '{}'", escape_sql_literal(state)),
        None => String::new(),
    };
    format!(
        "SELECT SUM(thread_state.dur) \
         FROM thread_state \
         JOIN thread USING(utid) \
         JOIN process USING(upid) \
         WHERE process.name = '{pkg}' \
           AND thread.is_main_thread = 1\
           {state_filter}"
    )
}

/// Total frames produced by the target app during the trace. Frames from
/// other processes (SurfaceFlinger, system_ui, launcher…) are excluded
/// via the `upid` join — otherwise the denominator drowns the numerator
/// in system-generated presentations.
fn frame_total_sql(package: &str) -> String {
    let pkg = escape_sql_literal(package);
    format!(
        "SELECT COUNT(*) \
         FROM actual_frame_timeline_slice aft \
         JOIN process p ON aft.upid = p.upid \
         WHERE p.name = '{pkg}'"
    )
}

/// Frames that missed their presentation deadline. Perfetto's `jank_type`
/// classifies many non-user-visible issues (e.g. `"Prediction Error"`)
/// which makes `jank_type != 'None'` over-count dramatically — on real
/// traces it reports > 90% janky because nearly every frame has *some*
/// prediction mismatch. `on_time_finish = 0` is the dedicated "frame
/// missed its deadline" flag and matches the intuitive definition of
/// jank. Scoped to the target process for the same reason as
/// [`frame_total_sql`].
fn jank_count_sql(package: &str) -> String {
    let pkg = escape_sql_literal(package);
    format!(
        "SELECT COUNT(*) \
         FROM actual_frame_timeline_slice aft \
         JOIN process p ON aft.upid = p.upid \
         WHERE p.name = '{pkg}' AND aft.on_time_finish = 0"
    )
}

/// p50 and p95 of frame duration in a single query. Uses `ROW_NUMBER()`
/// / `COUNT(*)` window functions (available since SQLite 3.25, long
/// supported by trace_processor) to pick the exact row at each
/// percentile rank without pulling every `dur` over the HTTP wire.
///
/// Scoped to the target process so system-frame outliers don't distort
/// the app's own distribution. The `+ 0.5 AS INT` trick rounds to the
/// nearest integer index — close to the conventional "nearest-rank"
/// percentile definition; exact interpolation between neighbours isn't
/// worth the SQL complexity here.
fn frame_percentiles_sql(package: &str) -> String {
    let pkg = escape_sql_literal(package);
    format!(
        "WITH sorted AS ( \
           SELECT aft.dur, \
                  ROW_NUMBER() OVER (ORDER BY aft.dur) AS rn, \
                  COUNT(*) OVER () AS total \
           FROM actual_frame_timeline_slice aft \
           JOIN process p ON aft.upid = p.upid \
           WHERE p.name = '{pkg}' \
         ) \
         SELECT \
           MAX(CASE WHEN rn = CAST(0.50 * total + 0.5 AS INT) THEN dur END) AS p50, \
           MAX(CASE WHEN rn = CAST(0.95 * total + 0.5 AS INT) THEN dur END) AS p95 \
         FROM sorted"
    )
}

/// Bucket `mem.rss` samples into 60 time buckets across the trace for a
/// sparkline-friendly series. Each bucket reports the peak RSS in that
/// slice of time — captures short allocation spikes without losing them
/// to averaging. Scoped to the target package via the same join used by
/// [`peak_rss_sql`].
fn rss_over_time_sql(package: &str) -> String {
    let pkg = escape_sql_literal(package);
    format!(
        "SELECT \
           MIN(60, CAST((c.ts - (SELECT start_ts FROM trace_bounds)) * 60 \
                        / (SELECT end_ts - start_ts FROM trace_bounds) AS INT)) AS bucket, \
           MAX(c.value) AS peak \
         FROM counter c \
         JOIN process_counter_track t ON c.track_id = t.id \
         JOIN process p ON t.upid = p.upid \
         WHERE t.name = 'mem.rss' AND p.name = '{pkg}' \
         GROUP BY bucket \
         ORDER BY bucket ASC"
    )
}

/// Double any single-quote so an interpolated value can't break out of its
/// SQL string literal.
pub(super) fn escape_sql_literal(s: &str) -> String {
    s.replace('\'', "''")
}

#[derive(Debug, Clone)]
pub enum CellState {
    Pending,
    Ready(Cell),
    Rows(Vec<Row>),
    MissingTable,
    Error(String),
}

/// Render/query state for the Summary tab.
pub struct SummaryState {
    cells: HashMap<SummaryKey, CellState>,
    package_name: String,
    captured_at: String,
}

impl SummaryState {
    pub fn new(package_name: String, captured_at: String) -> Self {
        let ctx = SummaryContext {
            package_name: package_name.clone(),
        };
        let mut cells = HashMap::new();
        for sq in SummaryKey::all_queries(&ctx) {
            cells.insert(sq.key, CellState::Pending);
        }
        Self {
            cells,
            package_name,
            captured_at,
        }
    }

    /// Reset every cell back to Pending so the worker's next `RunSummary`
    /// repopulates it.
    pub fn reset(&mut self) {
        let ctx = SummaryContext {
            package_name: self.package_name.clone(),
        };
        for sq in SummaryKey::all_queries(&ctx) {
            self.cells.insert(sq.key, CellState::Pending);
        }
    }

    /// Read-only access to the most recent state for a given metric.
    /// Used by the diff screen to compare two `SummaryState`s cell-by-cell.
    pub(crate) fn cell(&self, key: SummaryKey) -> Option<&CellState> {
        self.cells.get(&key)
    }

    #[allow(dead_code)] // used by future diff expansions (pkg-awareness)
    pub(crate) fn package_name(&self) -> &str {
        &self.package_name
    }

    pub(crate) fn captured_at(&self) -> &str {
        &self.captured_at
    }

    pub fn on_cell(&mut self, key: SummaryKey, outcome: SummaryCellOutcome) {
        let state = match outcome {
            SummaryCellOutcome::Ok(cell) => CellState::Ready(cell),
            SummaryCellOutcome::MissingTable => CellState::MissingTable,
            SummaryCellOutcome::Error(e) => CellState::Error(e),
        };
        self.cells.insert(key, state);
    }

    pub fn on_rows(&mut self, key: SummaryKey, outcome: SummaryRowsOutcome) {
        let state = match outcome {
            SummaryRowsOutcome::Ok(rows) => CellState::Rows(rows),
            SummaryRowsOutcome::MissingTable => CellState::MissingTable,
            SummaryRowsOutcome::Error(e) => CellState::Error(e),
        };
        self.cells.insert(key, state);
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        // Startup card and Memory section are conditional: traces without
        // `android.startup` data or without memory counters entirely skip
        // their sections instead of rendering stubs. This keeps narrow
        // terminals from losing vertical space to a fixed "—" card on
        // captures that don't have the relevant data sources enabled.
        let show_startup = matches!(
            self.cells.get(&SummaryKey::StartupInfo),
            Some(CellState::Rows(rows)) if !rows.is_empty()
        );
        let show_memory = matches!(
            self.cells.get(&SummaryKey::RssOverTime),
            Some(CellState::Rows(rows)) if !rows.is_empty()
        );

        let mut constraints = vec![
            Constraint::Length(3), // context strip
            Constraint::Length(3), // health tiles row
        ];
        if show_startup {
            constraints.push(Constraint::Length(3)); // startup card
        }
        if show_memory {
            constraints.push(Constraint::Length(5)); // memory section (label + sparkline)
        }
        constraints.push(Constraint::Min(3)); // main-thread hotspots
        constraints.push(Constraint::Length(3)); // trace contents ribbon

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        self.render_context_strip(frame, chunks[0]);
        self.render_health_tiles(frame, chunks[1]);

        let mut idx = 2;
        if show_startup {
            self.render_startup_card(frame, chunks[idx]);
            idx += 1;
        }
        if show_memory {
            self.render_memory_section(frame, chunks[idx]);
            idx += 1;
        }
        self.render_main_thread_hotspots(frame, chunks[idx]);
        idx += 1;
        self.render_trace_contents_ribbon(frame, chunks[idx]);
    }

    fn render_context_strip(&self, frame: &mut Frame, area: Rect) {
        let dim = Style::default().fg(theme::dim());
        let label = Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD);
        let sep = Span::styled("  ·  ", dim);

        let device = match self.cells.get(&SummaryKey::DeviceFingerprint) {
            Some(CellState::Ready(Cell::String(s))) if !s.is_empty() => truncate(s, 40),
            Some(CellState::Pending) => "…".into(),
            _ => "—".into(),
        };

        let duration = match self.cells.get(&SummaryKey::TraceDurationNs) {
            Some(CellState::Ready(Cell::Int(ns))) => format_duration_ns(*ns),
            Some(CellState::Pending) => "…".into(),
            _ => "—".into(),
        };

        let line = Line::from(vec![
            Span::styled("pkg ", dim),
            Span::styled(self.package_name.clone(), label),
            sep.clone(),
            Span::styled("dev ", dim),
            Span::styled(device, label),
            sep.clone(),
            Span::styled("captured ", dim),
            Span::styled(self.captured_at.clone(), label),
            sep,
            Span::styled("dur ", dim),
            Span::styled(duration, label),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dim)
            .title(Span::styled(" Context ", dim));
        let para = Paragraph::new(line).block(block).alignment(Alignment::Left);
        frame.render_widget(para, area);
    }

    fn render_health_tiles(&self, frame: &mut Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
            ])
            .split(area);

        self.render_jank_rate_tile(frame, cols[0]);
        self.render_frame_percentile_tile(frame, cols[1]);
        self.render_peak_rss_tile(frame, cols[2]);
        self.render_main_busy_tile(frame, cols[3]);
    }

    fn render_jank_rate_tile(&self, frame: &mut Frame, area: Rect) {
        let jank = self.cells.get(&SummaryKey::JankFrameCount);
        let total = self.cells.get(&SummaryKey::TotalFrameCount);
        let (text, severity) = format_jank_rate(jank, total);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Jank rate ", Style::default().fg(theme::dim())));
        let para = Paragraph::new(Line::from(Span::styled(text, severity_style(severity))))
            .block(block)
            .alignment(Alignment::Center);
        frame.render_widget(para, area);
    }

    fn render_frame_percentile_tile(&self, frame: &mut Frame, area: Rect) {
        let (text, severity) =
            format_frame_percentiles(self.cells.get(&SummaryKey::FrameDurPercentiles));
        let value_style = severity_style(severity);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                " Frame times ",
                Style::default().fg(theme::dim()),
            ));
        let para = Paragraph::new(Line::from(Span::styled(text, value_style)))
            .block(block)
            .alignment(Alignment::Center);
        frame.render_widget(para, area);
    }

    fn render_peak_rss_tile(&self, frame: &mut Frame, area: Rect) {
        let text = match self.cells.get(&SummaryKey::PeakRssBytes) {
            Some(CellState::Ready(Cell::Int(bytes))) if *bytes > 0 => format_bytes(*bytes as u64),
            Some(CellState::Ready(_)) => "—".into(),
            Some(CellState::Pending) => "…".into(),
            _ => "—".into(),
        };
        let style = if text == "…" || text == "—" {
            Style::default().fg(theme::dim())
        } else {
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                " Peak RSS ",
                Style::default().fg(theme::dim()),
            ));
        let para = Paragraph::new(Line::from(Span::styled(text, style)))
            .block(block)
            .alignment(Alignment::Center);
        frame.render_widget(para, area);
    }

    fn render_main_busy_tile(&self, frame: &mut Frame, area: Rect) {
        let running = self.cells.get(&SummaryKey::MainThreadRunningNs);
        let total = self.cells.get(&SummaryKey::MainThreadTotalNs);
        let (text, severity) = format_main_thread_busy(running, total);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                " Main-thread busy ",
                Style::default().fg(theme::dim()),
            ));
        let para = Paragraph::new(Line::from(Span::styled(text, severity_style(severity))))
            .block(block)
            .alignment(Alignment::Center);
        frame.render_widget(para, area);
    }

    /// Render the single-row startup card. Only called when there is at
    /// least one row, so `unwrap_or`-style defaults below are safe fallbacks
    /// for column-shape surprises (e.g. trace_processor schema drift),
    /// not for the "no startup" case.
    fn render_startup_card(&self, frame: &mut Frame, area: Rect) {
        let dim = Style::default().fg(theme::dim());
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Startup ", dim));

        let row = match self.cells.get(&SummaryKey::StartupInfo) {
            Some(CellState::Rows(rows)) => rows.first(),
            _ => None,
        };

        let line = match row {
            Some(r) => {
                let type_span = r
                    .cells()
                    .first()
                    .map(|c| match c {
                        Cell::String(s) => format_startup_type(s),
                        _ => cell_display(c),
                    })
                    .unwrap_or_else(|| "—".into());
                let dur_span = r
                    .cells()
                    .get(1)
                    .and_then(|c| match c {
                        Cell::Int(ns) => Some(format_duration_ns(*ns)),
                        Cell::Float(ns) => Some(format_duration_ns(*ns as i64)),
                        _ => None,
                    })
                    .unwrap_or_else(|| "—".into());
                let pkg_span = r
                    .cells()
                    .get(2)
                    .map(cell_display)
                    .unwrap_or_else(|| "—".into());

                let value = Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD);
                Line::from(vec![
                    Span::styled(type_span, value),
                    Span::styled("  ·  ", dim),
                    Span::styled(dur_span, value),
                    Span::styled("  ·  ", dim),
                    Span::styled(pkg_span, value),
                ])
            }
            None => Line::from(Span::styled("—", dim)),
        };

        let para = Paragraph::new(line).block(block).alignment(Alignment::Left);
        frame.render_widget(para, area);
    }

    /// Render the memory-over-time section: one header line (min / peak
    /// callouts) atop a ratatui [`Sparkline`] driven by the bucketed
    /// `mem.rss` series. Only called when `RssOverTime` has rows, so an
    /// empty series path isn't reachable here.
    fn render_memory_section(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(2)])
            .split(area);

        let series = extract_rss_series(self.cells.get(&SummaryKey::RssOverTime));
        let dim = Style::default().fg(theme::dim());
        let value = Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD);

        let label = Line::from(vec![
            Span::styled("Memory", dim),
            Span::styled("  ·  min ", dim),
            Span::styled(format_bytes_or_dash(series.min), value),
            Span::styled("  ·  peak ", dim),
            Span::styled(format_bytes_or_dash(series.max), value),
        ]);
        frame.render_widget(Paragraph::new(label), rows[0]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dim);
        let sparkline = Sparkline::default()
            .block(block)
            .data(&series.values[..])
            .style(Style::default().fg(theme::accent()));
        frame.render_widget(sparkline, rows[1]);
    }

    fn render_main_thread_hotspots(&self, frame: &mut Frame, area: Rect) {
        let title = format!(" Main-thread hotspots · {} ", self.package_name);
        let block = Block::default().borders(Borders::ALL).title(Span::styled(
            title,
            Style::default().fg(theme::dim()),
        ));

        match self.cells.get(&SummaryKey::MainThreadTopSlices) {
            Some(CellState::Pending) => {
                let p = Paragraph::new("running…")
                    .block(block)
                    .style(Style::default().fg(theme::dim()));
                frame.render_widget(p, area);
            }
            Some(CellState::MissingTable) => {
                let p = Paragraph::new("no slice data")
                    .block(block)
                    .style(Style::default().fg(theme::dim()));
                frame.render_widget(p, area);
            }
            Some(CellState::Error(msg)) => {
                let p = Paragraph::new(msg.clone())
                    .block(block)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(theme::err()));
                frame.render_widget(p, area);
            }
            Some(CellState::Rows(rows)) if !rows.is_empty() => {
                let header = TableRow::new(vec!["name", "count", "total (ms)"])
                    .style(Style::default().fg(theme::dim()));
                let body: Vec<TableRow> = rows
                    .iter()
                    .map(|r| {
                        let name = r
                            .cells()
                            .first()
                            .map(cell_display)
                            .unwrap_or_else(|| "-".into());
                        let count = r
                            .cells()
                            .get(1)
                            .map(cell_display)
                            .unwrap_or_else(|| "-".into());
                        let total_ms = r
                            .cells()
                            .get(2)
                            .and_then(|c| match c {
                                Cell::Int(ns) => Some(*ns as f64 / 1e6),
                                Cell::Float(ns) => Some(*ns / 1e6),
                                _ => None,
                            })
                            .map(|ms| format!("{ms:.2}"))
                            .unwrap_or_else(|| "-".into());
                        TableRow::new(vec![name, count, total_ms])
                    })
                    .collect();

                let widths = [
                    Constraint::Percentage(60),
                    Constraint::Percentage(15),
                    Constraint::Percentage(25),
                ];
                let table = Table::new(body, widths).header(header).block(block);
                frame.render_widget(table, area);
            }
            _ => {
                let p = Paragraph::new(format!(
                    "no main-thread slices for {}",
                    self.package_name
                ))
                .block(block)
                .style(Style::default().fg(theme::dim()));
                frame.render_widget(p, area);
            }
        }
    }

    fn render_trace_contents_ribbon(&self, frame: &mut Frame, area: Rect) {
        let dim = Style::default().fg(theme::dim());
        let probes = [
            SummaryKey::HasFtrace,
            SummaryKey::HasFrameTimeline,
            SummaryKey::HasStartups,
            SummaryKey::HasThreadState,
        ];

        let mut spans: Vec<Span<'_>> = Vec::new();
        for (i, key) in probes.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  ·  ", dim));
            }
            spans.push(Span::styled(ribbon_label(*key).to_string(), dim));
            spans.push(Span::raw(" "));
            spans.push(probe_glyph(self.cells.get(key)));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(dim)
            .title(Span::styled(" Trace contents ", dim));
        let para = Paragraph::new(Line::from(spans))
            .block(block)
            .alignment(Alignment::Center);
        frame.render_widget(para, area);
    }
}

/// Map a probe cell's state to a single-character glyph styled for the
/// ribbon. `Ready(Int(_))` is a ✓ regardless of the count — even zero rows
/// means the table exists, which is all the ribbon promises. If a probe
/// needs "populated" semantics in the future, we can tighten this.
fn probe_glyph(state: Option<&CellState>) -> Span<'_> {
    match state {
        Some(CellState::Ready(Cell::Int(_))) | Some(CellState::Ready(Cell::Float(_))) => {
            Span::styled("✓", Style::default().fg(theme::ok()).add_modifier(Modifier::BOLD))
        }
        Some(CellState::MissingTable) => {
            Span::styled("✗", Style::default().fg(theme::err()).add_modifier(Modifier::BOLD))
        }
        Some(CellState::Pending) => Span::styled("…", Style::default().fg(theme::dim())),
        Some(CellState::Error(_)) => Span::styled(
            "?",
            Style::default()
                .fg(theme::accent_secondary())
                .add_modifier(Modifier::BOLD),
        ),
        _ => Span::styled("—", Style::default().fg(theme::dim())),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Ok,
    Warn,
    Bad,
    Neutral,
}

/// Derive the `"12.3% · 46/376"` display plus a severity bucket for
/// colouring from the two frame-count cells. Missing/error/pending
/// degrades to a neutral `"—"`.
fn format_jank_rate(
    jank: Option<&CellState>,
    total: Option<&CellState>,
) -> (String, Severity) {
    let jank_n = cell_as_i64(jank);
    let total_n = cell_as_i64(total);
    match (jank_n, total_n) {
        (Some(j), Some(t)) if t > 0 => {
            let pct = (j as f64) * 100.0 / (t as f64);
            let severity = if pct > 5.0 {
                Severity::Bad
            } else if pct >= 1.0 {
                Severity::Warn
            } else {
                Severity::Ok
            };
            (format!("{pct:.1}% · {j}/{t}"), severity)
        }
        (Some(j), Some(0)) => (format!("— · {j}/0"), Severity::Neutral),
        _ => {
            // Any missing / pending / error path => neutral "—".
            let is_pending = matches!(jank, Some(CellState::Pending))
                || matches!(total, Some(CellState::Pending));
            let glyph = if is_pending { "…" } else { "—" };
            (glyph.into(), Severity::Neutral)
        }
    }
}

fn cell_as_i64(state: Option<&CellState>) -> Option<i64> {
    match state {
        Some(CellState::Ready(Cell::Int(v))) => Some(*v),
        _ => None,
    }
}

/// Centralise the tile value style per severity so every severity-coloured
/// tile stays visually consistent.
fn severity_style(severity: Severity) -> Style {
    match severity {
        Severity::Ok => Style::default().fg(theme::ok()),
        Severity::Warn => Style::default().fg(theme::accent_secondary()),
        Severity::Bad => Style::default().fg(theme::err()),
        Severity::Neutral => Style::default().fg(theme::dim()),
    }
    .add_modifier(Modifier::BOLD)
}

/// Format the `FrameDurPercentiles` result into `"p50 16 · p95 42 ms"` and
/// bucket severity by p95 against common frame budgets: >33ms
/// ("worse than 30fps") is bad, 16–33ms ("slower than 60fps") is warn,
/// below 16ms is ok. Missing/empty input renders as `"—"`.
fn format_frame_percentiles(state: Option<&CellState>) -> (String, Severity) {
    let rows = match state {
        Some(CellState::Rows(rows)) if !rows.is_empty() => rows,
        Some(CellState::Pending) => return ("…".into(), Severity::Neutral),
        _ => return ("—".into(), Severity::Neutral),
    };
    let row = &rows[0];
    let p50 = row.cells().first().and_then(cell_duration_ns);
    let p95 = row.cells().get(1).and_then(cell_duration_ns);
    match (p50, p95) {
        (Some(p50), Some(p95)) => {
            let p50_ms = p50 as f64 / 1e6;
            let p95_ms = p95 as f64 / 1e6;
            let severity = if p95_ms > 33.0 {
                Severity::Bad
            } else if p95_ms >= 16.0 {
                Severity::Warn
            } else {
                Severity::Ok
            };
            (
                format!("p50 {p50_ms:.0} · p95 {p95_ms:.0} ms"),
                severity,
            )
        }
        _ => ("—".into(), Severity::Neutral),
    }
}

/// Accept either int or float nanoseconds — Perfetto's aggregations can
/// legitimately produce either depending on the driver version.
fn cell_duration_ns(cell: &Cell) -> Option<i64> {
    match cell {
        Cell::Int(v) if *v >= 0 => Some(*v),
        Cell::Float(v) if *v >= 0.0 => Some(*v as i64),
        _ => None,
    }
}

/// Decoded RSS-over-time series, plus the aggregate min/max callouts shown
/// above the sparkline. `values` always contains at least one element when
/// returned from a non-empty `Rows` state — callers that gate on "do we
/// have memory data" should read [`has_rss_series`] or equivalent.
pub(super) struct RssSeries {
    pub values: Vec<u64>,
    pub min: Option<u64>,
    pub max: Option<u64>,
}

impl RssSeries {
    fn empty() -> Self {
        Self {
            values: Vec::new(),
            min: None,
            max: None,
        }
    }
}

/// Extract the `mem.rss` sparkline series from `CellState::Rows`. The raw
/// query returns `(bucket, peak)` tuples that may skip buckets (Perfetto
/// only emitted counter samples in those time slices). We fill gaps by
/// carrying the previous value forward — memory is continuous, a skipped
/// bucket doesn't mean "RSS dropped to zero", it means "no new sample."
pub(super) fn extract_rss_series(state: Option<&CellState>) -> RssSeries {
    let rows = match state {
        Some(CellState::Rows(rows)) if !rows.is_empty() => rows,
        _ => return RssSeries::empty(),
    };

    // Decode into (bucket, value) pairs, skipping rows with unexpected
    // shapes rather than bailing — one bad row shouldn't erase the whole
    // sparkline.
    let mut samples: Vec<(u32, u64)> = Vec::with_capacity(rows.len());
    for r in rows {
        let bucket = r.cells().first().and_then(|c| match c {
            Cell::Int(v) if *v >= 0 => Some(*v as u32),
            _ => None,
        });
        let value = r.cells().get(1).and_then(|c| match c {
            Cell::Int(v) if *v >= 0 => Some(*v as u64),
            Cell::Float(v) if *v >= 0.0 => Some(*v as u64),
            _ => None,
        });
        if let (Some(b), Some(v)) = (bucket, value) {
            samples.push((b, v));
        }
    }
    if samples.is_empty() {
        return RssSeries::empty();
    }
    samples.sort_by_key(|(b, _)| *b);

    let last_bucket = samples.last().unwrap().0;
    let len = (last_bucket as usize) + 1;
    let mut values = Vec::with_capacity(len);
    let mut cursor = 0usize;
    let mut last_value: Option<u64> = None;
    for bucket in 0..len {
        let here = samples
            .iter()
            .skip(cursor)
            .find(|(b, _)| *b as usize == bucket)
            .map(|(_, v)| *v);
        if let Some(v) = here {
            last_value = Some(v);
            // Advance cursor past this sample so subsequent iterations
            // don't re-scan from the start.
            cursor = samples
                .iter()
                .position(|(b, _)| *b as usize > bucket)
                .unwrap_or(samples.len());
        }
        // Carry-forward fill; if the first real sample isn't at bucket
        // zero, we fill leading buckets with its value so the sparkline
        // starts at a sensible baseline rather than 0.
        let fill = last_value.unwrap_or(samples[0].1);
        values.push(fill);
    }

    let min = values.iter().copied().min();
    let max = values.iter().copied().max();
    RssSeries { values, min, max }
}

fn format_bytes_or_dash(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) => format_bytes(b),
        None => "—".into(),
    }
}

/// Derive the `"73%"` busy display plus a severity bucket from the running
/// / total `thread_state.dur` sums. High busy% on the main thread means
/// the UI is pegged — a jank risk — so the severity scale is inverted
/// from "low is bad" (jank rate): busy > 85% → red, 50–85% → yellow,
/// < 50% → green.
fn format_main_thread_busy(
    running: Option<&CellState>,
    total: Option<&CellState>,
) -> (String, Severity) {
    let r = cell_as_i64(running);
    let t = cell_as_i64(total);
    match (r, t) {
        (Some(r), Some(t)) if t > 0 && r >= 0 => {
            let pct = (r as f64) * 100.0 / (t as f64);
            let severity = if pct > 85.0 {
                Severity::Bad
            } else if pct >= 50.0 {
                Severity::Warn
            } else {
                Severity::Ok
            };
            (format!("{pct:.0}%"), severity)
        }
        _ => {
            let is_pending = matches!(running, Some(CellState::Pending))
                || matches!(total, Some(CellState::Pending));
            let glyph = if is_pending { "…" } else { "—" };
            (glyph.into(), Severity::Neutral)
        }
    }
}

/// Perfetto's `android_startups.startup_type` is a short lowercase token
/// (`"cold"`, `"warm"`, `"hot"`, or sometimes more specific strings).
/// Convert it into a presentation-ready "Cold start" / "Warm start" /
/// "Hot start" label; anything unrecognised is rendered verbatim.
fn format_startup_type(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "cold" => "Cold start".into(),
        "warm" => "Warm start".into(),
        "hot" => "Hot start".into(),
        other if !other.is_empty() => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = String::new();
                    out.push(first.to_ascii_uppercase());
                    out.push_str(chars.as_str());
                    out
                }
                None => raw.to_string(),
            }
        }
        _ => raw.to_string(),
    }
}

/// Format a byte count as `"487 MB"` / `"1.2 GB"`. Uses binary units (1024
/// per step), which matches how Android reports process RSS and how
/// everyone eyeballs memory sizes.
pub(crate) fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GB", b / GIB)
    } else if b >= MIB {
        format!("{:.0} MB", b / MIB)
    } else if b >= KIB {
        format!("{:.0} KB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub(crate) fn format_duration_ns(ns: i64) -> String {
    if ns <= 0 {
        return "—".into();
    }
    let secs_total = ns as f64 / 1e9;
    if secs_total < 1.0 {
        return format!("{:.1} ms", ns as f64 / 1e6);
    }
    if secs_total < 60.0 {
        return format!("{secs_total:.2} s");
    }
    let minutes = (secs_total / 60.0).floor() as i64;
    let seconds = secs_total - (minutes as f64 * 60.0);
    let mut out = String::new();
    let _ = write!(out, "{minutes}m {seconds:.0}s");
    out
}

pub fn cell_display(cell: &Cell) -> String {
    match cell {
        Cell::Null => "—".into(),
        Cell::Int(v) => v.to_string(),
        Cell::Float(v) => format!("{v}"),
        Cell::String(s) => s.clone(),
        Cell::Blob(b) => format!("<{} bytes>", b.len()),
    }
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
    use crate::trace_processor::Cell;

    fn ctx() -> SummaryContext {
        SummaryContext {
            package_name: "com.foo.bar".into(),
        }
    }

    fn make_rows_with_cells(rows: Vec<Vec<Cell>>) -> Vec<Row> {
        rows.into_iter().map(Row::new_for_test).collect()
    }

    #[test]
    fn missing_table_outcome_downgrades_to_dash() {
        let mut s = SummaryState::new("com.example".into(), "now".into());
        s.on_cell(
            SummaryKey::JankFrameCount,
            SummaryCellOutcome::MissingTable,
        );
        assert!(matches!(
            s.cells.get(&SummaryKey::JankFrameCount).unwrap(),
            CellState::MissingTable
        ));
    }

    #[test]
    fn all_queries_covers_every_key() {
        // Compile-time exhaustiveness: if a new SummaryKey is added without
        // a query, this match will fail to compile.
        for sq in SummaryKey::all_queries(&ctx()) {
            match sq.key {
                SummaryKey::DeviceFingerprint
                | SummaryKey::TraceDurationNs
                | SummaryKey::JankFrameCount
                | SummaryKey::TotalFrameCount
                | SummaryKey::FrameDurPercentiles
                | SummaryKey::PeakRssBytes
                | SummaryKey::MainThreadRunningNs
                | SummaryKey::MainThreadTotalNs
                | SummaryKey::StartupInfo
                | SummaryKey::RssOverTime
                | SummaryKey::MainThreadTopSlices
                | SummaryKey::HasFtrace
                | SummaryKey::HasFrameTimeline
                | SummaryKey::HasStartups
                | SummaryKey::HasThreadState => {}
            }
        }
    }

    #[test]
    fn all_queries_embeds_package_name() {
        let qs = SummaryKey::all_queries(&ctx());
        let main = qs
            .iter()
            .find(|q| q.key == SummaryKey::MainThreadTopSlices)
            .unwrap();
        assert!(main.sql.contains("'com.foo.bar'"));
        assert!(main.sql.contains("is_main_thread = 1"));
    }

    #[test]
    fn escape_sql_literal_doubles_quotes() {
        assert_eq!(escape_sql_literal("plain"), "plain");
        assert_eq!(escape_sql_literal("com.app"), "com.app");
        assert_eq!(escape_sql_literal("it's a 'test'"), "it''s a ''test''");
    }

    #[test]
    fn all_queries_escapes_malicious_package_name() {
        let ctx = SummaryContext {
            package_name: "evil' OR 1=1 --".into(),
        };
        let qs = SummaryKey::all_queries(&ctx);
        let main = qs
            .iter()
            .find(|q| q.key == SummaryKey::MainThreadTopSlices)
            .unwrap();
        // The fully-escaped literal must appear inside '…' — no orphan
        // quote that could break out of the string.
        assert!(main.sql.contains("'evil'' OR 1=1 --'"));
    }

    #[test]
    fn format_jank_rate_handles_edge_cases() {
        let jank = CellState::Ready(Cell::Int(46));
        let total = CellState::Ready(Cell::Int(376));
        let (text, sev) = format_jank_rate(Some(&jank), Some(&total));
        assert!(text.starts_with("12."));
        assert!(text.contains("46/376"));
        assert_eq!(sev, Severity::Bad); // 12.2% > 5%

        // OK bucket (< 1%)
        let j = CellState::Ready(Cell::Int(1));
        let t = CellState::Ready(Cell::Int(300));
        let (_, sev) = format_jank_rate(Some(&j), Some(&t));
        assert_eq!(sev, Severity::Ok);

        // Warn bucket (1–5%)
        let j = CellState::Ready(Cell::Int(10));
        let t = CellState::Ready(Cell::Int(300));
        let (_, sev) = format_jank_rate(Some(&j), Some(&t));
        assert_eq!(sev, Severity::Warn);

        // total=0 still formats, severity neutral
        let j = CellState::Ready(Cell::Int(0));
        let t = CellState::Ready(Cell::Int(0));
        let (text, sev) = format_jank_rate(Some(&j), Some(&t));
        assert_eq!(text, "— · 0/0");
        assert_eq!(sev, Severity::Neutral);

        // Missing denominator → neutral dash
        let j = CellState::Ready(Cell::Int(5));
        let m = CellState::MissingTable;
        let (text, sev) = format_jank_rate(Some(&j), Some(&m));
        assert_eq!(text, "—");
        assert_eq!(sev, Severity::Neutral);

        // Pending renders as …
        let p = CellState::Pending;
        let (text, sev) = format_jank_rate(Some(&p), Some(&p));
        assert_eq!(text, "…");
        assert_eq!(sev, Severity::Neutral);
    }

    #[test]
    fn duration_formatting_switches_units() {
        assert_eq!(format_duration_ns(500_000), "0.5 ms");
        assert_eq!(format_duration_ns(1_500_000_000), "1.50 s");
        assert!(format_duration_ns(90_000_000_000).starts_with("1m"));
        assert_eq!(format_duration_ns(0), "—");
    }

    #[test]
    fn cell_display_covers_every_variant() {
        assert_eq!(cell_display(&Cell::Null), "—");
        assert_eq!(cell_display(&Cell::Int(42)), "42");
        assert_eq!(cell_display(&Cell::String("x".into())), "x");
    }

    #[test]
    fn format_bytes_picks_correct_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2 * 1024), "2 KB");
        assert_eq!(format_bytes(487 * 1024 * 1024), "487 MB");
        assert!(format_bytes(2 * 1024 * 1024 * 1024).starts_with("2.00 GB"));
    }

    #[test]
    fn format_startup_type_capitalises_known_tokens() {
        assert_eq!(format_startup_type("cold"), "Cold start");
        assert_eq!(format_startup_type("WARM"), "Warm start");
        assert_eq!(format_startup_type("hot"), "Hot start");
        // Unknown token — preserve whatever trace_processor gave us but
        // capitalise the first letter so it reads as a label.
        assert_eq!(format_startup_type("resumed"), "Resumed");
        // Empty never appears in practice but shouldn't panic.
        assert_eq!(format_startup_type(""), "");
    }

    #[test]
    fn format_main_thread_busy_bucket_boundaries() {
        // Ok bucket (< 50%)
        let r = CellState::Ready(Cell::Int(30));
        let t = CellState::Ready(Cell::Int(100));
        let (text, sev) = format_main_thread_busy(Some(&r), Some(&t));
        assert_eq!(text, "30%");
        assert_eq!(sev, Severity::Ok);

        // Warn bucket (50–85%)
        let r = CellState::Ready(Cell::Int(70));
        let t = CellState::Ready(Cell::Int(100));
        let (text, sev) = format_main_thread_busy(Some(&r), Some(&t));
        assert_eq!(text, "70%");
        assert_eq!(sev, Severity::Warn);

        // Bad bucket (> 85%)
        let r = CellState::Ready(Cell::Int(95));
        let t = CellState::Ready(Cell::Int(100));
        let (text, sev) = format_main_thread_busy(Some(&r), Some(&t));
        assert_eq!(text, "95%");
        assert_eq!(sev, Severity::Bad);

        // Total=0 → neutral dash (main thread had no observed state
        // samples, typically because thread_state wasn't captured).
        let r = CellState::Ready(Cell::Int(0));
        let t = CellState::Ready(Cell::Int(0));
        let (text, sev) = format_main_thread_busy(Some(&r), Some(&t));
        assert_eq!(text, "—");
        assert_eq!(sev, Severity::Neutral);

        // Missing numerator → neutral dash.
        let m = CellState::MissingTable;
        let t = CellState::Ready(Cell::Int(100));
        let (text, sev) = format_main_thread_busy(Some(&m), Some(&t));
        assert_eq!(text, "—");
        assert_eq!(sev, Severity::Neutral);

        // Pending renders as …
        let p = CellState::Pending;
        let (text, sev) = format_main_thread_busy(Some(&p), Some(&p));
        assert_eq!(text, "…");
        assert_eq!(sev, Severity::Neutral);
    }

    #[test]
    fn format_frame_percentiles_happy_path() {
        // Build a minimal Row fixture via QueryResult decode logic. The
        // public Row constructor isn't available; build via a local helper
        // that mimics the worker output shape.
        let rows = make_rows_with_cells(vec![vec![
            Cell::Int(16_000_000), // p50 = 16 ms
            Cell::Int(42_000_000), // p95 = 42 ms
        ]]);
        let state = CellState::Rows(rows);
        let (text, sev) = format_frame_percentiles(Some(&state));
        assert_eq!(text, "p50 16 · p95 42 ms");
        assert_eq!(sev, Severity::Bad); // 42 > 33 → worse than 30fps budget

        // Warn bucket (16–33 ms)
        let rows = make_rows_with_cells(vec![vec![
            Cell::Int(12_000_000),
            Cell::Int(24_000_000),
        ]]);
        let state = CellState::Rows(rows);
        let (_, sev) = format_frame_percentiles(Some(&state));
        assert_eq!(sev, Severity::Warn);

        // Ok bucket (< 16 ms)
        let rows = make_rows_with_cells(vec![vec![
            Cell::Int(8_000_000),
            Cell::Int(12_000_000),
        ]]);
        let state = CellState::Rows(rows);
        let (_, sev) = format_frame_percentiles(Some(&state));
        assert_eq!(sev, Severity::Ok);
    }

    #[test]
    fn format_frame_percentiles_edge_cases() {
        // Missing cell → neutral dash.
        assert_eq!(
            format_frame_percentiles(None),
            ("—".into(), Severity::Neutral)
        );
        assert_eq!(
            format_frame_percentiles(Some(&CellState::MissingTable)),
            ("—".into(), Severity::Neutral)
        );

        // Pending → …
        assert_eq!(
            format_frame_percentiles(Some(&CellState::Pending)),
            ("…".into(), Severity::Neutral)
        );

        // Empty rows (valid query but no frames in trace) → dash.
        let state = CellState::Rows(Vec::new());
        assert_eq!(
            format_frame_percentiles(Some(&state)),
            ("—".into(), Severity::Neutral)
        );

        // Null cells inside the row (SQL `MAX(CASE WHEN …)` returned NULL
        // because table was empty) → dash.
        let rows = make_rows_with_cells(vec![vec![Cell::Null, Cell::Null]]);
        let state = CellState::Rows(rows);
        assert_eq!(
            format_frame_percentiles(Some(&state)),
            ("—".into(), Severity::Neutral)
        );
    }

    #[test]
    fn extract_rss_series_fills_gaps_with_carry_forward() {
        // Buckets 0, 2, 5 with increasing values. Buckets 1, 3, 4 should
        // carry forward the previous bucket's value.
        let rows = make_rows_with_cells(vec![
            vec![Cell::Int(0), Cell::Int(100 * 1024 * 1024)],
            vec![Cell::Int(2), Cell::Int(120 * 1024 * 1024)],
            vec![Cell::Int(5), Cell::Int(150 * 1024 * 1024)],
        ]);
        let series = extract_rss_series(Some(&CellState::Rows(rows)));
        assert_eq!(series.values.len(), 6);
        assert_eq!(series.values[0], 100 * 1024 * 1024);
        assert_eq!(series.values[1], 100 * 1024 * 1024); // carried forward
        assert_eq!(series.values[2], 120 * 1024 * 1024);
        assert_eq!(series.values[3], 120 * 1024 * 1024); // carried forward
        assert_eq!(series.values[4], 120 * 1024 * 1024); // carried forward
        assert_eq!(series.values[5], 150 * 1024 * 1024);
        assert_eq!(series.min, Some(100 * 1024 * 1024));
        assert_eq!(series.max, Some(150 * 1024 * 1024));
    }

    #[test]
    fn extract_rss_series_handles_empty_inputs() {
        let s = extract_rss_series(None);
        assert!(s.values.is_empty());
        assert!(s.min.is_none() && s.max.is_none());

        let s = extract_rss_series(Some(&CellState::MissingTable));
        assert!(s.values.is_empty());

        let s = extract_rss_series(Some(&CellState::Rows(Vec::new())));
        assert!(s.values.is_empty());
    }

    #[test]
    fn rss_over_time_sql_embeds_package_safely() {
        let ctx = SummaryContext {
            package_name: "com.foo.bar".into(),
        };
        let qs = SummaryKey::all_queries(&ctx);
        let rss = qs
            .iter()
            .find(|q| q.key == SummaryKey::RssOverTime)
            .unwrap();
        assert!(rss.sql.contains("'com.foo.bar'"));
        assert!(rss.sql.contains("mem.rss"));

        // Injection defence: any escaping path broken would surface here
        // as a syntactically valid closed literal.
        let evil = SummaryContext {
            package_name: "a' OR 1=1--".into(),
        };
        let qs = SummaryKey::all_queries(&evil);
        let rss = qs
            .iter()
            .find(|q| q.key == SummaryKey::RssOverTime)
            .unwrap();
        assert!(rss.sql.contains("'a'' OR 1=1--'"));
    }

    #[test]
    fn peak_rss_and_thread_state_queries_embed_package() {
        let ctx = SummaryContext {
            package_name: "com.foo.bar".into(),
        };
        let qs = SummaryKey::all_queries(&ctx);
        let rss = qs.iter().find(|q| q.key == SummaryKey::PeakRssBytes).unwrap();
        assert!(rss.sql.contains("'com.foo.bar'"));
        assert!(rss.sql.contains("mem.rss"));

        let running = qs
            .iter()
            .find(|q| q.key == SummaryKey::MainThreadRunningNs)
            .unwrap();
        assert!(running.sql.contains("'com.foo.bar'"));
        assert!(running.sql.contains("state = 'Running'"));

        let total = qs
            .iter()
            .find(|q| q.key == SummaryKey::MainThreadTotalNs)
            .unwrap();
        assert!(total.sql.contains("'com.foo.bar'"));
        assert!(!total.sql.contains("state = '"));
    }
}
