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

use crate::trace_processor::{Cell, QueryResult, Row};
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
/// to averaging. Scoped to the target package via the usual
/// `counter → process_counter_track → process` join.
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
    custom: CustomMetricsState,
    /// When true, custom-metric tables collapse to a single-line
    /// "N rows" tile. Toggled by `c` on the Summary tab so a dense
    /// dashboard can shrink to a scannable overview on demand.
    compact_custom: bool,
}

/// Per-app saved queries plus their most recent result state. Mirrors the
/// canned-metric `CellState`/`cells` pattern but keyed by user-provided
/// names instead of an enum.
#[derive(Debug, Default)]
pub struct CustomMetricsState {
    /// Ordered list of queries to dispatch on each `RunSummary`. Source
    /// of truth is the DB; this is a per-run snapshot taken by the
    /// screen and handed in at construction / reset.
    queries: Vec<super::worker::CustomQuery>,
    /// Most recent result per query name. Pending until the first
    /// `CustomResult` event for a given name arrives.
    results: HashMap<String, CustomResultState>,
}

#[derive(Debug, Clone)]
pub enum CustomResultState {
    Pending,
    Done(QueryResult),
    Error(String),
}

impl SummaryState {
    pub fn new(
        package_name: String,
        captured_at: String,
        custom_queries: Vec<super::worker::CustomQuery>,
    ) -> Self {
        let ctx = SummaryContext {
            package_name: package_name.clone(),
        };
        let mut cells = HashMap::new();
        for sq in SummaryKey::all_queries(&ctx) {
            cells.insert(sq.key, CellState::Pending);
        }
        let custom = CustomMetricsState::new(custom_queries);
        Self {
            cells,
            package_name,
            captured_at,
            custom,
            compact_custom: false,
        }
    }

    /// Flip between expanded (tables show up to 4 rows) and compact
    /// (tables collapse to a 1-line tile). Preserves all cached results.
    pub fn toggle_compact_custom(&mut self) {
        self.compact_custom = !self.compact_custom;
    }

    pub fn compact_custom(&self) -> bool {
        self.compact_custom
    }

    /// Reset every canned cell back to Pending and replace the custom-query
    /// list with a fresh snapshot from the caller. The custom snapshot is
    /// passed in (not re-fetched) so the screen controls when DB reads
    /// happen — keeps this struct DB-free.
    pub fn reset(&mut self, custom_queries: Vec<super::worker::CustomQuery>) {
        let ctx = SummaryContext {
            package_name: self.package_name.clone(),
        };
        for sq in SummaryKey::all_queries(&ctx) {
            self.cells.insert(sq.key, CellState::Pending);
        }
        self.custom = CustomMetricsState::new(custom_queries);
    }

    pub fn on_custom_result(&mut self, name: String, result: Result<QueryResult, String>) {
        self.custom.on_result(name, result);
    }

    /// Replace only the custom-queries snapshot (leaves canned cells
    /// untouched). Used when a `:save` adds a new query mid-session and
    /// we want to re-render the custom section without re-running the
    /// canned pipeline.
    pub fn reset_custom(&mut self, custom_queries: Vec<super::worker::CustomQuery>) {
        self.custom = CustomMetricsState::new(custom_queries);
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
        // Startup card, Memory section, and Custom metrics section are all
        // conditional: captures without the underlying data / queries
        // collapse those regions entirely instead of rendering stubs.
        // This keeps narrow terminals from losing vertical space to
        // empty panels.
        let show_startup = matches!(
            self.cells.get(&SummaryKey::StartupInfo),
            Some(CellState::Rows(rows)) if !rows.is_empty()
        );
        let show_memory = matches!(
            self.cells.get(&SummaryKey::RssOverTime),
            Some(CellState::Rows(rows)) if !rows.is_empty()
        );
        // Budget for the Custom metrics section: claim everything left
        // after the always-present regions (context + health + ribbon +
        // min hotspots) and any conditional sections. Hotspots is a
        // `Min(3)` so it still gets its minimum even when custom fills
        // the rest. `c` toggles a compact mode that collapses tables to
        // one-line tiles for a high-density overview.
        let mut reserved: u16 = 3 /* context */ + 3 /* health */ + 3 /* ribbon */ + 3 /* min hotspots */;
        if show_startup {
            reserved = reserved.saturating_add(3);
        }
        if show_memory {
            reserved = reserved.saturating_add(5);
        }
        let custom_max = area.height.saturating_sub(reserved);
        let custom_height = self
            .custom
            .rendered_height_with(custom_max, self.compact_custom);

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
        if custom_height > 0 {
            constraints.push(Constraint::Length(custom_height));
        }
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
        if custom_height > 0 {
            self.custom.render(frame, chunks[idx], self.compact_custom);
            idx += 1;
        }
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
            .title(Span::styled(" Context ", theme::title()));
        let para = Paragraph::new(line).block(block).alignment(Alignment::Left);
        frame.render_widget(para, area);
    }

    fn render_health_tiles(&self, frame: &mut Frame, area: Rect) {
        // Three equal-width tiles. Peak RSS used to live here but the
        // memory section's header already surfaces the peak inline, so
        // the dedicated tile was duplicative.
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(34),
                Constraint::Percentage(33),
                Constraint::Percentage(33),
            ])
            .split(area);

        self.render_jank_rate_tile(frame, cols[0]);
        self.render_frame_percentile_tile(frame, cols[1]);
        self.render_main_busy_tile(frame, cols[2]);
    }

    fn render_jank_rate_tile(&self, frame: &mut Frame, area: Rect) {
        let jank = self.cells.get(&SummaryKey::JankFrameCount);
        let total = self.cells.get(&SummaryKey::TotalFrameCount);
        let (text, severity) = format_jank_rate(jank, total);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Jank rate ", theme::title()));
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
            .title(Span::styled(" Frame times ", theme::title()));
        let para = Paragraph::new(Line::from(Span::styled(text, value_style)))
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
            .title(Span::styled(" Main-thread busy ", theme::title()));
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
            .title(Span::styled(" Startup ", theme::title()));

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
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, theme::title()));

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
            .title(Span::styled(" Trace contents ", theme::title()));
        let para = Paragraph::new(Line::from(spans))
            .block(block)
            .alignment(Alignment::Center);
        frame.render_widget(para, area);
    }
}

/// Number of body rows we show in a single custom-metric table card
/// before truncating. Beyond this, a footer line points at the REPL.
const CUSTOM_TABLE_VISIBLE_ROWS: usize = 4;

/// How many tile-shaped (1×1) custom-metric cards we pack into one
/// horizontal row. Three matches the canned health-tile grid (~33%
/// each), gives single-value KPIs comfortable breathing room.
const CUSTOM_TILES_PER_ROW: usize = 3;

/// Vertical size of one tile / status card (border + value + border).
const CUSTOM_TILE_HEIGHT: u16 = 3;

/// Cell-display truncation in custom-metric tables — matches the
/// existing main-thread hotspots cap.
const CUSTOM_CELL_MAX_CHARS: usize = 40;

/// Per-shape card classification. Computed at render time from the
/// live `QueryResult`, not stored — the same query can change shape
/// across traces (a result with 1 row on one capture might be 5 rows
/// on another).
#[derive(Debug)]
enum CardShape<'a> {
    /// 1×1 result — render as a small tile, like the canned health
    /// tiles.
    Tile { value: String },
    /// Multi-row tabular result — render as an inline Table card.
    /// `total_rows` lets the renderer decide whether to show an
    /// overflow footer.
    Table { qr: &'a QueryResult, total_rows: usize },
    /// Empty result — render as a tile of `—`. Same height as a Tile,
    /// keeps the section tidy.
    Empty,
    /// Worker result hasn't arrived yet — render as a `…` tile.
    Pending,
    /// Worker returned an error (typically: query references a missing
    /// table). Render as a red status card.
    Error(&'a str),
}

impl<'a> CardShape<'a> {
    /// Vertical row count this card occupies when rendered.
    fn height(&self) -> u16 {
        match self {
            CardShape::Tile { .. } | CardShape::Empty | CardShape::Pending | CardShape::Error(_) => {
                CUSTOM_TILE_HEIGHT
            }
            CardShape::Table { total_rows, .. } => {
                let visible = (*total_rows).min(CUSTOM_TABLE_VISIBLE_ROWS) as u16;
                let overflow = if *total_rows > CUSTOM_TABLE_VISIBLE_ROWS { 1 } else { 0 };
                // borders (2) + header (1) + visible rows + optional overflow line
                3 + visible + overflow
            }
        }
    }

    fn is_tile_like(&self) -> bool {
        !matches!(self, CardShape::Table { .. })
    }
}

impl CustomMetricsState {
    fn new(queries: Vec<super::worker::CustomQuery>) -> Self {
        let mut results = HashMap::with_capacity(queries.len());
        for q in &queries {
            results.insert(q.name.clone(), CustomResultState::Pending);
        }
        Self { queries, results }
    }

    fn on_result(&mut self, name: String, result: Result<QueryResult, String>) {
        let state = match result {
            Ok(qr) => CustomResultState::Done(qr),
            Err(e) => CustomResultState::Error(e),
        };
        self.results.insert(name, state);
    }

    #[cfg(test)]
    fn shape_for(state: Option<&CustomResultState>) -> CardShape<'_> {
        Self::shape_for_with(state, false)
    }

    /// Classify a single result into a `CardShape`. When `compact` is
    /// true, multi-row `Table` shapes collapse to a `Tile` displaying
    /// the row count — tiles, empty, pending, and error shapes are
    /// unaffected (they're already single-line friendly).
    fn shape_for_with(state: Option<&CustomResultState>, compact: bool) -> CardShape<'_> {
        match state {
            None | Some(CustomResultState::Pending) => CardShape::Pending,
            Some(CustomResultState::Error(msg)) => CardShape::Error(msg),
            Some(CustomResultState::Done(qr)) => {
                if qr.rows.is_empty() {
                    CardShape::Empty
                } else if qr.rows.len() == 1 && qr.columns.len() == 1 {
                    let value = qr
                        .rows
                        .first()
                        .and_then(|r| r.cells().first())
                        .map(cell_display)
                        .unwrap_or_else(|| "—".into());
                    CardShape::Tile { value }
                } else if compact {
                    let n = qr.rows.len();
                    let value = if n == 1 {
                        "1 row".into()
                    } else {
                        format!("{n} rows")
                    };
                    CardShape::Tile { value }
                } else {
                    CardShape::Table {
                        qr,
                        total_rows: qr.rows.len(),
                    }
                }
            }
        }
    }

    /// Walk cards in render order, accumulating heights. Tiles pack
    /// `CUSTOM_TILES_PER_ROW` per row (one row = `CUSTOM_TILE_HEIGHT`
    /// regardless of how many tiles are in it). Tables take a full
    /// row each. Returns 0 when the section is empty so the parent
    /// collapses the constraint. Caps at `max_height` — anything
    /// beyond becomes the overflow indicator.
    #[cfg(test)]
    fn rendered_height(&self, max_height: u16) -> u16 {
        self.rendered_height_with(max_height, false)
    }

    fn rendered_height_with(&self, max_height: u16, compact: bool) -> u16 {
        if self.queries.is_empty() {
            return 0;
        }
        let cards = self.classify_with(compact);
        let mut h: u16 = 0;
        let mut tile_in_progress = 0u16; // tiles staged for the current row
        for (_, shape) in &cards {
            if shape.is_tile_like() {
                tile_in_progress += 1;
                if tile_in_progress as usize >= CUSTOM_TILES_PER_ROW {
                    h = h.saturating_add(CUSTOM_TILE_HEIGHT);
                    tile_in_progress = 0;
                }
            } else {
                if tile_in_progress > 0 {
                    h = h.saturating_add(CUSTOM_TILE_HEIGHT);
                    tile_in_progress = 0;
                }
                h = h.saturating_add(shape.height());
            }
        }
        if tile_in_progress > 0 {
            h = h.saturating_add(CUSTOM_TILE_HEIGHT);
        }
        h.min(max_height)
    }

    fn classify_with(&self, compact: bool) -> Vec<(&str, CardShape<'_>)> {
        self.queries
            .iter()
            .map(|q| {
                let state = self.results.get(&q.name);
                (q.name.as_str(), Self::shape_for_with(state, compact))
            })
            .collect()
    }

    fn render(&self, frame: &mut Frame, area: Rect, compact: bool) {
        if self.queries.is_empty() || area.height < CUSTOM_TILE_HEIGHT {
            return;
        }
        let dim = Style::default().fg(theme::dim());
        let cards = self.classify_with(compact);

        // Pack tiles into rows of CUSTOM_TILES_PER_ROW (preserving the
        // declaration order across tile/table classes), then track how
        // much vertical area each rendered chunk needs. We render in
        // two passes: first compute the chunk list with heights, then
        // emit until we hit `area.height` and append an overflow line
        // for whatever didn't fit.
        let mut chunks: Vec<RenderChunk<'_>> = Vec::new();
        let mut tile_buf: Vec<(&str, CardShape<'_>)> = Vec::new();
        for (name, shape) in cards {
            if shape.is_tile_like() {
                tile_buf.push((name, shape));
                if tile_buf.len() >= CUSTOM_TILES_PER_ROW {
                    chunks.push(RenderChunk::TileRow(std::mem::take(&mut tile_buf)));
                }
            } else {
                if !tile_buf.is_empty() {
                    chunks.push(RenderChunk::TileRow(std::mem::take(&mut tile_buf)));
                }
                chunks.push(RenderChunk::TableCard { name, shape });
            }
        }
        if !tile_buf.is_empty() {
            chunks.push(RenderChunk::TileRow(tile_buf));
        }

        let mut used: u16 = 0;
        let mut rendered_chunks = Vec::new();
        let mut dropped_cards = 0usize;
        for chunk in chunks {
            let h = chunk.height();
            // Reserve 1 row for an overflow indicator if anything later
            // has to be skipped.
            let remaining = area.height.saturating_sub(used);
            if h > remaining || (h + 1 > remaining && rendered_chunks.iter().any(|_| true) && /* we may need overflow line */ false)
            {
                dropped_cards += chunk.card_count();
                continue;
            }
            used = used.saturating_add(h);
            rendered_chunks.push(chunk);
        }
        // Reserve a row for the overflow indicator if needed and if
        // there's room — otherwise drop the last chunk so it fits.
        if dropped_cards > 0 {
            while used >= area.height && !rendered_chunks.is_empty() {
                let last = rendered_chunks.pop().unwrap();
                dropped_cards += last.card_count();
                used = used.saturating_sub(last.height());
            }
        }

        // Compute layout constraints from final chunk list.
        let mut constraints: Vec<Constraint> =
            rendered_chunks.iter().map(|c| Constraint::Length(c.height())).collect();
        if dropped_cards > 0 {
            constraints.push(Constraint::Length(1));
        }
        if constraints.is_empty() {
            return;
        }
        let regions = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        for (i, chunk) in rendered_chunks.iter().enumerate() {
            chunk.render(frame, regions[i]);
        }
        if dropped_cards > 0 {
            let idx = rendered_chunks.len();
            let line = Paragraph::new(Line::from(Span::styled(
                format!(
                    "+{dropped_cards} metric{plural} not shown — switch to SQL tab to see them",
                    plural = if dropped_cards == 1 { "" } else { "s" }
                ),
                dim,
            )));
            frame.render_widget(line, regions[idx]);
        }
    }
}

enum RenderChunk<'a> {
    /// One horizontal row of up to `CUSTOM_TILES_PER_ROW` tile-like
    /// cards (Tile / Empty / Pending / Error all qualify).
    TileRow(Vec<(&'a str, CardShape<'a>)>),
    /// One full-width table card.
    TableCard { name: &'a str, shape: CardShape<'a> },
}

impl<'a> RenderChunk<'a> {
    fn height(&self) -> u16 {
        match self {
            RenderChunk::TileRow(_) => CUSTOM_TILE_HEIGHT,
            RenderChunk::TableCard { shape, .. } => shape.height(),
        }
    }

    fn card_count(&self) -> usize {
        match self {
            RenderChunk::TileRow(tiles) => tiles.len(),
            RenderChunk::TableCard { .. } => 1,
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        match self {
            RenderChunk::TileRow(tiles) => {
                let cols = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(
                        (0..CUSTOM_TILES_PER_ROW)
                            .map(|_| Constraint::Percentage(100 / CUSTOM_TILES_PER_ROW as u16))
                            .collect::<Vec<_>>(),
                    )
                    .split(area);
                for (i, (name, shape)) in tiles.iter().enumerate() {
                    if i >= cols.len() {
                        break;
                    }
                    render_tile_like(frame, cols[i], name, shape);
                }
            }
            RenderChunk::TableCard { name, shape } => render_table_card(frame, area, name, shape),
        }
    }
}

fn render_tile_like(frame: &mut Frame, area: Rect, name: &str, shape: &CardShape<'_>) {
    let dim = Style::default().fg(theme::dim());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(format!(" {name} "), theme::title()));
    let (text, style) = match shape {
        CardShape::Tile { value } => (
            value.clone(),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        CardShape::Empty => ("—".into(), dim),
        CardShape::Pending => ("…".into(), dim),
        CardShape::Error(msg) => (
            format!("✗ {}", truncate(msg, CUSTOM_CELL_MAX_CHARS)),
            Style::default().fg(theme::err()),
        ),
        // Tables shouldn't reach this path (router gates them out), but
        // be defensive: render a one-line "see table" placeholder.
        CardShape::Table { .. } => ("table".into(), dim),
    };
    let para = Paragraph::new(Line::from(Span::styled(text, style)))
        .block(block)
        .alignment(Alignment::Center);
    frame.render_widget(para, area);
}

fn render_table_card(frame: &mut Frame, area: Rect, name: &str, shape: &CardShape<'_>) {
    let dim = Style::default().fg(theme::dim());
    let CardShape::Table { qr, total_rows } = shape else {
        return;
    };
    let visible = (*total_rows).min(CUSTOM_TABLE_VISIBLE_ROWS);
    let header_cells: Vec<String> = qr.columns.iter().cloned().collect();
    let header = TableRow::new(header_cells).style(
        Style::default()
            .fg(theme::dim())
            .add_modifier(Modifier::BOLD),
    );
    let body: Vec<TableRow> = qr
        .rows
        .iter()
        .take(visible)
        .map(|r| {
            TableRow::new(
                r.cells()
                    .iter()
                    .map(|c| truncate(&cell_display(c), CUSTOM_CELL_MAX_CHARS).to_string())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let ncols = qr.columns.len().max(1);
    let widths: Vec<Constraint> = (0..ncols)
        .map(|_| Constraint::Percentage((100 / ncols as u16).max(1)))
        .collect();
    let title = format!(" {name} · {total_rows} rows ");
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, theme::title()));
    let table = Table::new(body, widths).header(header).block(block);
    frame.render_widget(table, area);

    if *total_rows > CUSTOM_TABLE_VISIBLE_ROWS {
        // Overflow footer: render a 1-row strip at the bottom of the
        // table area, just inside the bottom border.
        let footer_y = area.y + area.height.saturating_sub(2);
        if footer_y >= area.y + 1 && area.height >= 4 {
            let footer_area = Rect {
                x: area.x + 1,
                y: footer_y,
                width: area.width.saturating_sub(2),
                height: 1,
            };
            let extra = total_rows - CUSTOM_TABLE_VISIBLE_ROWS;
            let footer = Paragraph::new(Line::from(Span::styled(
                format!("  +{extra} more rows · open in REPL for full view"),
                dim,
            )));
            frame.render_widget(footer, footer_area);
        }
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
        let mut s = SummaryState::new("com.example".into(), "now".into(), Vec::new());
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
    fn thread_state_queries_embed_package() {
        let ctx = SummaryContext {
            package_name: "com.foo.bar".into(),
        };
        let qs = SummaryKey::all_queries(&ctx);

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

    fn qr_1x1(val: Cell) -> QueryResult {
        QueryResult {
            columns: vec!["v".into()],
            rows: vec![Row::new_for_test(vec![val])],
            elapsed_ms: None,
        }
    }

    fn custom_query(name: &str, sql: &str) -> super::super::worker::CustomQuery {
        super::super::worker::CustomQuery {
            name: name.into(),
            sql: sql.into(),
        }
    }

    fn qr_multi_row(n: usize) -> QueryResult {
        QueryResult {
            columns: vec!["x".into()],
            rows: (0..n)
                .map(|i| Row::new_for_test(vec![Cell::Int(i as i64)]))
                .collect(),
            elapsed_ms: None,
        }
    }

    #[test]
    fn shape_for_classifies_each_result_kind() {
        // 1×1 → Tile
        let tile_state = CustomResultState::Done(qr_1x1(Cell::Int(42)));
        assert!(matches!(
            CustomMetricsState::shape_for(Some(&tile_state)),
            CardShape::Tile { .. }
        ));

        // 1×N → Table (single-row-multi-col still goes to table for v1)
        let srmc = CustomResultState::Done(QueryResult {
            columns: vec!["a".into(), "b".into()],
            rows: vec![Row::new_for_test(vec![Cell::Int(1), Cell::Int(2)])],
            elapsed_ms: None,
        });
        assert!(matches!(
            CustomMetricsState::shape_for(Some(&srmc)),
            CardShape::Table { .. }
        ));

        // N×M → Table
        let nm = CustomResultState::Done(qr_multi_row(5));
        assert!(matches!(
            CustomMetricsState::shape_for(Some(&nm)),
            CardShape::Table { .. }
        ));

        // Empty rows → Empty
        let empty = CustomResultState::Done(QueryResult {
            columns: vec!["x".into()],
            rows: Vec::new(),
            elapsed_ms: None,
        });
        assert!(matches!(
            CustomMetricsState::shape_for(Some(&empty)),
            CardShape::Empty
        ));

        // Error
        let err = CustomResultState::Error("boom".into());
        assert!(matches!(
            CustomMetricsState::shape_for(Some(&err)),
            CardShape::Error(_)
        ));

        // Pending (no entry, or Pending state)
        assert!(matches!(
            CustomMetricsState::shape_for(None),
            CardShape::Pending
        ));
        let pending = CustomResultState::Pending;
        assert!(matches!(
            CustomMetricsState::shape_for(Some(&pending)),
            CardShape::Pending
        ));
    }

    #[test]
    fn rendered_height_for_empty_section_is_zero() {
        let empty = CustomMetricsState::new(Vec::new());
        assert_eq!(empty.rendered_height(100), 0);
    }

    #[test]
    fn rendered_height_packs_tiles_into_rows_of_three() {
        // 5 tile-shaped cards (all Pending → tile-like) → 2 rows of tiles.
        let cs = CustomMetricsState::new(
            (0..5)
                .map(|i| custom_query(&format!("q{i}"), "SELECT 1"))
                .collect(),
        );
        // 2 rows × CUSTOM_TILE_HEIGHT
        assert_eq!(cs.rendered_height(100), 2 * CUSTOM_TILE_HEIGHT);
    }

    #[test]
    fn rendered_height_each_table_takes_borders_plus_visible_rows() {
        // One query with a 10-row result → Table card.
        // Expected: 2 borders + 1 header + CUSTOM_TABLE_VISIBLE_ROWS rows + 1 overflow line.
        let mut cs = CustomMetricsState::new(vec![custom_query("a", "SELECT 1")]);
        cs.on_result("a".into(), Ok(qr_multi_row(10)));
        let expected = 2 + 1 + CUSTOM_TABLE_VISIBLE_ROWS as u16 + 1;
        assert_eq!(cs.rendered_height(100), expected);
    }

    #[test]
    fn compact_collapses_tables_into_tiles_with_row_count() {
        // In compact mode, a multi-row result that would render as a
        // Table becomes a Tile displaying "N rows".
        let done = CustomResultState::Done(qr_multi_row(7));
        match CustomMetricsState::shape_for_with(Some(&done), true) {
            CardShape::Tile { value } => assert_eq!(value, "7 rows"),
            other => panic!("expected Tile, got {other:?}"),
        }
        // Compact with exactly one row uses singular.
        let one = CustomResultState::Done(QueryResult {
            columns: vec!["a".into(), "b".into()],
            rows: vec![Row::new_for_test(vec![Cell::Int(1), Cell::Int(2)])],
            elapsed_ms: None,
        });
        match CustomMetricsState::shape_for_with(Some(&one), true) {
            CardShape::Tile { value } => assert_eq!(value, "1 row"),
            other => panic!("expected Tile, got {other:?}"),
        }
        // Compact doesn't affect 1×1 Tile results.
        let tile = CustomResultState::Done(qr_1x1(Cell::Int(42)));
        assert!(matches!(
            CustomMetricsState::shape_for_with(Some(&tile), true),
            CardShape::Tile { .. }
        ));
    }

    #[test]
    fn compact_mode_fits_in_less_space() {
        // Three table-shaped queries expanded vs compact.
        let mut cs = CustomMetricsState::new(vec![
            custom_query("a", "1"),
            custom_query("b", "2"),
            custom_query("c", "3"),
        ]);
        cs.on_result("a".into(), Ok(qr_multi_row(8)));
        cs.on_result("b".into(), Ok(qr_multi_row(8)));
        cs.on_result("c".into(), Ok(qr_multi_row(8)));
        let expanded = cs.rendered_height_with(100, false);
        let compact = cs.rendered_height_with(100, true);
        assert!(
            compact < expanded,
            "compact ({compact}) should be strictly smaller than expanded ({expanded})"
        );
    }

    #[test]
    fn rendered_height_capped_at_max() {
        // 20 tile-shaped cards — unbounded would be 7 rows × 3 = 21. Cap at 10.
        let cs = CustomMetricsState::new(
            (0..20)
                .map(|i| custom_query(&format!("q{i}"), "SELECT 1"))
                .collect(),
        );
        assert_eq!(cs.rendered_height(10), 10);
    }

    #[test]
    fn custom_metrics_records_ok_and_error_results() {
        let mut s = CustomMetricsState::new(vec![
            custom_query("a", "SELECT 1"),
            custom_query("b", "SELECT * FROM missing"),
        ]);
        s.on_result("a".into(), Ok(qr_1x1(Cell::Int(1))));
        s.on_result("b".into(), Err("no such table".into()));
        assert!(matches!(
            s.results.get("a"),
            Some(CustomResultState::Done(_))
        ));
        assert!(matches!(
            s.results.get("b"),
            Some(CustomResultState::Error(_))
        ));
    }
}
