//! Diff row computation and rendering.
//!
//! Given two [`SummaryState`]s (left / right), produce a sequence of
//! [`DiffRow`]s that line up metric-by-metric. The Δ column renders with a
//! sign and a severity bucket so regressions visually pop. Metrics with a
//! clear "lower is better" or "higher is worse" direction carry that in
//! their bucket; neutral metrics (duration, device fingerprint, captured
//! time) stay dim regardless of sign.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Row as TableRow, Table};

use crate::trace_processor::Cell;
use crate::tui::screens::analysis::summary::{
    CellState, SummaryKey, SummaryState, cell_display, format_bytes, format_duration_ns,
};
use crate::tui::theme;

/// One row of the diff table. `label` is static; `left` and `right` are
/// already-formatted value strings; `delta` is absent when a meaningful
/// numeric delta can't be computed.
#[derive(Debug, Clone)]
pub struct DiffRow {
    pub label: &'static str,
    pub left: String,
    pub right: String,
    pub delta: Option<DeltaDisplay>,
}

#[derive(Debug, Clone)]
pub struct DeltaDisplay {
    pub text: String,
    pub style: DeltaStyle,
}

/// Bucket for colouring the Δ cell. `Improved` / `Regressed` are
/// direction-aware per metric (lower-is-better vs higher-is-better);
/// `Neutral` covers "no change" and metrics without a clear direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaStyle {
    Improved,
    Regressed,
    Neutral,
}

/// Which direction of change is "better" for a given metric. Most of what
/// we show is lower-is-better (jank rate, frame times, RSS, startup,
/// main-busy%). A couple of rows (trace duration, device) are neutral —
/// users shouldn't read green/red into them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    LowerIsBetter,
    Neutral,
}

/// Compute the ordered list of rows the diff table should render. Both
/// sides are expected to be in `Ready`; callers should gate on that.
pub fn compute_rows(left: &SummaryState, right: &SummaryState) -> Vec<DiffRow> {
    let mut out = Vec::new();

    // ── context ──────────────────────────────────────────────────────
    out.push(DiffRow {
        label: "Device",
        left: cell_string(left.cell(SummaryKey::DeviceFingerprint)),
        right: cell_string(right.cell(SummaryKey::DeviceFingerprint)),
        delta: text_delta(
            left.cell(SummaryKey::DeviceFingerprint),
            right.cell(SummaryKey::DeviceFingerprint),
        ),
    });
    out.push(DiffRow {
        label: "Captured",
        left: left.captured_at().to_string(),
        right: right.captured_at().to_string(),
        delta: None,
    });
    out.push(DiffRow {
        label: "Duration",
        left: duration_cell_display(left.cell(SummaryKey::TraceDurationNs)),
        right: duration_cell_display(right.cell(SummaryKey::TraceDurationNs)),
        delta: duration_delta(
            left.cell(SummaryKey::TraceDurationNs),
            right.cell(SummaryKey::TraceDurationNs),
            Direction::Neutral,
        ),
    });

    // ── health ───────────────────────────────────────────────────────
    out.push(diff_jank_rate(left, right));
    out.push(diff_frame_percentile(
        "p50 frame",
        left,
        right,
        PercentileIndex::P50,
    ));
    out.push(diff_frame_percentile(
        "p95 frame",
        left,
        right,
        PercentileIndex::P95,
    ));
    out.push(diff_peak_rss(left, right));
    out.push(diff_main_busy(left, right));

    // ── startup ──────────────────────────────────────────────────────
    out.push(diff_startup(left, right));

    out
}

/// Main entry from DiffScreen: compute rows and render as a ratatui Table.
pub fn render_diff_table(
    frame: &mut Frame,
    area: Rect,
    left: &SummaryState,
    right: &SummaryState,
) {
    let rows = compute_rows(left, right);

    let header_style = Style::default()
        .fg(theme::dim())
        .add_modifier(Modifier::BOLD);
    let header = TableRow::new(vec!["metric", "A (left)", "B (right)", "Δ"])
        .style(header_style);

    let body: Vec<TableRow> = rows
        .iter()
        .map(|r| {
            let delta_span = match &r.delta {
                Some(d) => Span::styled(d.text.clone(), delta_style(d.style)),
                None => Span::styled("—", Style::default().fg(theme::dim())),
            };
            TableRow::new(vec![
                Line::from(Span::styled(r.label, Style::default().fg(theme::dim()))),
                Line::from(Span::raw(r.left.clone())),
                Line::from(Span::raw(r.right.clone())),
                Line::from(delta_span),
            ])
        })
        .collect();

    let widths = [
        Constraint::Percentage(20),
        Constraint::Percentage(30),
        Constraint::Percentage(30),
        Constraint::Percentage(20),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::dim()))
        .title(Span::styled(
            " Diff ",
            Style::default().fg(theme::dim()),
        ));
    let table = Table::new(body, widths).header(header).block(block);
    frame.render_widget(table, area);
}

fn delta_style(style: DeltaStyle) -> Style {
    match style {
        DeltaStyle::Improved => Style::default()
            .fg(theme::ok())
            .add_modifier(Modifier::BOLD),
        DeltaStyle::Regressed => Style::default()
            .fg(theme::err())
            .add_modifier(Modifier::BOLD),
        DeltaStyle::Neutral => Style::default().fg(theme::dim()),
    }
}

// ── per-metric helpers ───────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum PercentileIndex {
    P50,
    P95,
}

fn diff_frame_percentile(
    label: &'static str,
    left: &SummaryState,
    right: &SummaryState,
    which: PercentileIndex,
) -> DiffRow {
    let idx = match which {
        PercentileIndex::P50 => 0,
        PercentileIndex::P95 => 1,
    };
    let l = frame_percentile_ns(left.cell(SummaryKey::FrameDurPercentiles), idx);
    let r = frame_percentile_ns(right.cell(SummaryKey::FrameDurPercentiles), idx);
    DiffRow {
        label,
        left: ns_display(l),
        right: ns_display(r),
        delta: numeric_delta_ns(l, r, Direction::LowerIsBetter),
    }
}

fn diff_jank_rate(left: &SummaryState, right: &SummaryState) -> DiffRow {
    let l = jank_ratio(
        left.cell(SummaryKey::JankFrameCount),
        left.cell(SummaryKey::TotalFrameCount),
    );
    let r = jank_ratio(
        right.cell(SummaryKey::JankFrameCount),
        right.cell(SummaryKey::TotalFrameCount),
    );
    DiffRow {
        label: "Jank rate",
        left: jank_display(l, left),
        right: jank_display(r, right),
        delta: percentage_point_delta(l, r, Direction::LowerIsBetter),
    }
}

fn jank_ratio(jank: Option<&CellState>, total: Option<&CellState>) -> Option<f64> {
    let j = cell_as_i64(jank)?;
    let t = cell_as_i64(total)?;
    if t <= 0 {
        return None;
    }
    Some(j as f64 * 100.0 / t as f64)
}

fn jank_display(pct: Option<f64>, side: &SummaryState) -> String {
    let j = cell_as_i64(side.cell(SummaryKey::JankFrameCount));
    let t = cell_as_i64(side.cell(SummaryKey::TotalFrameCount));
    match (pct, j, t) {
        (Some(pct), Some(j), Some(t)) => format!("{pct:.1}% · {j}/{t}"),
        _ => "—".into(),
    }
}

fn diff_peak_rss(left: &SummaryState, right: &SummaryState) -> DiffRow {
    let l = cell_as_u64_positive(left.cell(SummaryKey::PeakRssBytes));
    let r = cell_as_u64_positive(right.cell(SummaryKey::PeakRssBytes));
    DiffRow {
        label: "Peak RSS",
        left: bytes_display(l),
        right: bytes_display(r),
        delta: bytes_delta(l, r, Direction::LowerIsBetter),
    }
}

fn diff_main_busy(left: &SummaryState, right: &SummaryState) -> DiffRow {
    let l = main_busy_pct(
        left.cell(SummaryKey::MainThreadRunningNs),
        left.cell(SummaryKey::MainThreadTotalNs),
    );
    let r = main_busy_pct(
        right.cell(SummaryKey::MainThreadRunningNs),
        right.cell(SummaryKey::MainThreadTotalNs),
    );
    DiffRow {
        label: "Main busy",
        left: pct_display(l),
        right: pct_display(r),
        delta: percentage_point_delta(l, r, Direction::LowerIsBetter),
    }
}

fn main_busy_pct(running: Option<&CellState>, total: Option<&CellState>) -> Option<f64> {
    let r = cell_as_i64(running)?;
    let t = cell_as_i64(total)?;
    if t <= 0 || r < 0 {
        return None;
    }
    Some(r as f64 * 100.0 / t as f64)
}

fn diff_startup(left: &SummaryState, right: &SummaryState) -> DiffRow {
    let l = startup_info(left.cell(SummaryKey::StartupInfo));
    let r = startup_info(right.cell(SummaryKey::StartupInfo));
    let (l_type, l_dur) = l.clone().unzip_parts();
    let (r_type, r_dur) = r.clone().unzip_parts();
    let left_s = match (&l_type, l_dur) {
        (Some(ty), Some(dur)) => format!("{ty} · {}", format_duration_ns(dur)),
        _ => "—".into(),
    };
    let right_s = match (&r_type, r_dur) {
        (Some(ty), Some(dur)) => format!("{ty} · {}", format_duration_ns(dur)),
        _ => "—".into(),
    };
    DiffRow {
        label: "Startup",
        left: left_s,
        right: right_s,
        delta: numeric_delta_ns(l_dur, r_dur, Direction::LowerIsBetter),
    }
}

#[derive(Debug, Clone, Default)]
struct StartupParts {
    startup_type: Option<String>,
    dur_ns: Option<i64>,
}

impl StartupParts {
    fn unzip_parts(self) -> (Option<String>, Option<i64>) {
        (self.startup_type, self.dur_ns)
    }
}

fn startup_info(state: Option<&CellState>) -> StartupParts {
    let rows = match state {
        Some(CellState::Rows(rows)) if !rows.is_empty() => rows,
        _ => return StartupParts::default(),
    };
    let r = &rows[0];
    let startup_type = r.cells().first().and_then(|c| match c {
        Cell::String(s) => Some(title_case_first(s)),
        _ => None,
    });
    let dur_ns = r.cells().get(1).and_then(|c| match c {
        Cell::Int(v) if *v >= 0 => Some(*v),
        Cell::Float(v) if *v >= 0.0 => Some(*v as i64),
        _ => None,
    });
    StartupParts {
        startup_type,
        dur_ns,
    }
}

fn title_case_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => {
            let mut out = String::with_capacity(s.len());
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
            out
        }
        None => s.to_string(),
    }
}

// ── generic cell helpers ─────────────────────────────────────────────────

fn cell_as_i64(state: Option<&CellState>) -> Option<i64> {
    match state {
        Some(CellState::Ready(Cell::Int(v))) => Some(*v),
        _ => None,
    }
}

fn cell_as_u64_positive(state: Option<&CellState>) -> Option<u64> {
    match state {
        Some(CellState::Ready(Cell::Int(v))) if *v > 0 => Some(*v as u64),
        Some(CellState::Ready(Cell::Float(v))) if *v > 0.0 => Some(*v as u64),
        _ => None,
    }
}

fn frame_percentile_ns(state: Option<&CellState>, idx: usize) -> Option<i64> {
    match state {
        Some(CellState::Rows(rows)) if !rows.is_empty() => {
            rows[0].cells().get(idx).and_then(|c| match c {
                Cell::Int(v) if *v >= 0 => Some(*v),
                Cell::Float(v) if *v >= 0.0 => Some(*v as i64),
                _ => None,
            })
        }
        _ => None,
    }
}

fn cell_string(state: Option<&CellState>) -> String {
    match state {
        Some(CellState::Ready(cell)) => cell_display(cell),
        Some(CellState::Pending) => "…".into(),
        _ => "—".into(),
    }
}

// ── formatters (display side) ────────────────────────────────────────────

fn ns_display(ns: Option<i64>) -> String {
    match ns {
        Some(ns) => format!("{:.0} ms", ns as f64 / 1e6),
        None => "—".into(),
    }
}

fn bytes_display(b: Option<u64>) -> String {
    match b {
        Some(b) => format_bytes(b),
        None => "—".into(),
    }
}

fn pct_display(p: Option<f64>) -> String {
    match p {
        Some(p) => format!("{p:.0}%"),
        None => "—".into(),
    }
}

fn duration_cell_display(state: Option<&CellState>) -> String {
    match state {
        Some(CellState::Ready(Cell::Int(ns))) if *ns > 0 => format_duration_ns(*ns),
        Some(CellState::Pending) => "…".into(),
        _ => "—".into(),
    }
}

// ── delta formatters (signed + directional) ──────────────────────────────

pub(crate) fn format_signed_duration_ns(delta_ns: i64) -> String {
    let abs = delta_ns.unsigned_abs() as i64;
    let sign = if delta_ns >= 0 { "+" } else { "-" };
    // Use the existing duration formatter on the absolute value; zero
    // renders as "—" via `format_duration_ns(0) = "—"` which we don't
    // want for a zero-delta (we want "+0 ms"), so special-case zero.
    if delta_ns == 0 {
        return "+0 ms".into();
    }
    format!("{sign}{}", format_duration_ns(abs))
}

pub(crate) fn format_signed_bytes(delta: i64) -> String {
    if delta == 0 {
        return "+0 B".into();
    }
    let sign = if delta >= 0 { "+" } else { "-" };
    format!("{sign}{}", format_bytes(delta.unsigned_abs()))
}

fn numeric_delta_ns(
    l: Option<i64>,
    r: Option<i64>,
    dir: Direction,
) -> Option<DeltaDisplay> {
    let l = l?;
    let r = r?;
    let diff = r - l;
    Some(DeltaDisplay {
        text: format_signed_duration_ns(diff),
        style: direction_style(diff, dir),
    })
}

fn bytes_delta(
    l: Option<u64>,
    r: Option<u64>,
    dir: Direction,
) -> Option<DeltaDisplay> {
    let l = l? as i64;
    let r = r? as i64;
    let diff = r - l;
    Some(DeltaDisplay {
        text: format_signed_bytes(diff),
        style: direction_style(diff, dir),
    })
}

fn percentage_point_delta(
    l: Option<f64>,
    r: Option<f64>,
    dir: Direction,
) -> Option<DeltaDisplay> {
    let l = l?;
    let r = r?;
    let diff_pp = r - l;
    let sign = if diff_pp >= 0.0 { "+" } else { "" };
    Some(DeltaDisplay {
        text: format!("{sign}{diff_pp:.1}pp"),
        style: direction_style(diff_pp as i64, dir),
    })
}

fn text_delta(l: Option<&CellState>, r: Option<&CellState>) -> Option<DeltaDisplay> {
    let left = cell_string(l);
    let right = cell_string(r);
    if left == "—" || right == "—" || left == "…" || right == "…" {
        return None;
    }
    let same = left == right;
    Some(DeltaDisplay {
        text: if same { "same".into() } else { "different".into() },
        style: DeltaStyle::Neutral,
    })
}

fn duration_delta(
    l: Option<&CellState>,
    r: Option<&CellState>,
    dir: Direction,
) -> Option<DeltaDisplay> {
    let l = cell_as_i64(l)?;
    let r = cell_as_i64(r)?;
    numeric_delta_ns(Some(l), Some(r), dir)
}

/// Map a signed difference to a style, taking the metric's direction into
/// account. Zero always renders Neutral regardless of direction.
fn direction_style(diff: i64, dir: Direction) -> DeltaStyle {
    if diff == 0 {
        return DeltaStyle::Neutral;
    }
    match dir {
        Direction::Neutral => DeltaStyle::Neutral,
        Direction::LowerIsBetter => {
            if diff < 0 {
                DeltaStyle::Improved
            } else {
                DeltaStyle::Regressed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::screens::analysis::summary::{SummaryKey, SummaryState};
    use crate::tui::screens::analysis::worker::{SummaryCellOutcome, SummaryRowsOutcome};
    use crate::trace_processor::{Cell, Row};

    fn state_with(
        pkg: &str,
        captured_at: &str,
        cells: &[(SummaryKey, Cell)],
        rows: &[(SummaryKey, Vec<Vec<Cell>>)],
    ) -> SummaryState {
        let mut s = SummaryState::new(pkg.into(), captured_at.into());
        for (k, c) in cells {
            s.on_cell(*k, SummaryCellOutcome::Ok(c.clone()));
        }
        for (k, row_cells) in rows {
            let rs: Vec<Row> = row_cells
                .iter()
                .map(|c| Row::new_for_test(c.clone()))
                .collect();
            s.on_rows(*k, SummaryRowsOutcome::Ok(rs));
        }
        s
    }

    #[test]
    fn compute_rows_detects_regression_and_improvement() {
        // Build two states: right regresses on jank, improves on p50.
        let left = state_with(
            "com.x",
            "2026-04-13 12:00:00 UTC",
            &[
                (SummaryKey::JankFrameCount, Cell::Int(10)),
                (SummaryKey::TotalFrameCount, Cell::Int(100)),
            ],
            &[(
                SummaryKey::FrameDurPercentiles,
                vec![vec![Cell::Int(16_000_000), Cell::Int(24_000_000)]],
            )],
        );
        let right = state_with(
            "com.x",
            "2026-04-14 12:00:00 UTC",
            &[
                (SummaryKey::JankFrameCount, Cell::Int(25)),
                (SummaryKey::TotalFrameCount, Cell::Int(100)),
            ],
            &[(
                SummaryKey::FrameDurPercentiles,
                vec![vec![Cell::Int(12_000_000), Cell::Int(20_000_000)]],
            )],
        );

        let rows = compute_rows(&left, &right);
        let jank = rows.iter().find(|r| r.label == "Jank rate").unwrap();
        assert_eq!(jank.left, "10.0% · 10/100");
        assert_eq!(jank.right, "25.0% · 25/100");
        let d = jank.delta.as_ref().unwrap();
        assert!(d.text.starts_with("+15"));
        assert_eq!(d.style, DeltaStyle::Regressed);

        let p50 = rows.iter().find(|r| r.label == "p50 frame").unwrap();
        let d = p50.delta.as_ref().unwrap();
        assert!(d.text.starts_with("-"));
        assert_eq!(d.style, DeltaStyle::Improved);
    }

    #[test]
    fn compute_rows_neutral_delta_on_missing_side() {
        let left = state_with(
            "com.x",
            "t",
            &[(SummaryKey::PeakRssBytes, Cell::Int(100 * 1024 * 1024))],
            &[],
        );
        // Right doesn't populate PeakRssBytes — it stays Pending.
        let right = SummaryState::new("com.x".into(), "t".into());

        let rows = compute_rows(&left, &right);
        let rss = rows.iter().find(|r| r.label == "Peak RSS").unwrap();
        assert!(rss.delta.is_none());
        assert_eq!(rss.right, "—");
    }

    #[test]
    fn compute_rows_same_device_says_same() {
        let left = state_with(
            "com.x",
            "t",
            &[(
                SummaryKey::DeviceFingerprint,
                Cell::String("google/husky/husky:14".into()),
            )],
            &[],
        );
        let right = state_with(
            "com.x",
            "t",
            &[(
                SummaryKey::DeviceFingerprint,
                Cell::String("google/husky/husky:14".into()),
            )],
            &[],
        );
        let rows = compute_rows(&left, &right);
        let dev = rows.iter().find(|r| r.label == "Device").unwrap();
        let d = dev.delta.as_ref().unwrap();
        assert_eq!(d.text, "same");
        assert_eq!(d.style, DeltaStyle::Neutral);
    }

    #[test]
    fn compute_rows_different_device_says_different() {
        let left = state_with(
            "com.x",
            "t",
            &[(
                SummaryKey::DeviceFingerprint,
                Cell::String("pixel 8".into()),
            )],
            &[],
        );
        let right = state_with(
            "com.x",
            "t",
            &[(
                SummaryKey::DeviceFingerprint,
                Cell::String("pixel 9".into()),
            )],
            &[],
        );
        let rows = compute_rows(&left, &right);
        let dev = rows.iter().find(|r| r.label == "Device").unwrap();
        let d = dev.delta.as_ref().unwrap();
        assert_eq!(d.text, "different");
    }

    #[test]
    fn format_signed_duration_handles_zero_and_signs() {
        assert_eq!(format_signed_duration_ns(0), "+0 ms");
        assert_eq!(format_signed_duration_ns(500_000), "+0.5 ms");
        assert_eq!(format_signed_duration_ns(-500_000), "-0.5 ms");
        assert_eq!(format_signed_duration_ns(1_500_000_000), "+1.50 s");
        assert_eq!(format_signed_duration_ns(-1_500_000_000), "-1.50 s");
    }

    #[test]
    fn format_signed_bytes_handles_zero_and_signs() {
        assert_eq!(format_signed_bytes(0), "+0 B");
        assert_eq!(format_signed_bytes(25 * 1024 * 1024), "+25 MB");
        assert_eq!(format_signed_bytes(-25 * 1024 * 1024), "-25 MB");
    }

    #[test]
    fn direction_style_respects_metric_direction() {
        // Lower is better: negative diff = improved, positive = regressed.
        assert_eq!(
            direction_style(-10, Direction::LowerIsBetter),
            DeltaStyle::Improved
        );
        assert_eq!(
            direction_style(10, Direction::LowerIsBetter),
            DeltaStyle::Regressed
        );
        // Zero always neutral.
        assert_eq!(
            direction_style(0, Direction::LowerIsBetter),
            DeltaStyle::Neutral
        );
        assert_eq!(direction_style(0, Direction::Neutral), DeltaStyle::Neutral);
        // Neutral metric ignores sign.
        assert_eq!(direction_style(-10, Direction::Neutral), DeltaStyle::Neutral);
        assert_eq!(direction_style(10, Direction::Neutral), DeltaStyle::Neutral);
    }
}
