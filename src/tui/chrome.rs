use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::theme;

const APP_NAME: &str = "Perfetto CLI";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Home-screen logo ASCII art, embedded from `assets/home_logo.txt` at
/// build time. Swap that file and rebuild to change the banner — no Rust
/// edits needed. Trailing whitespace is trimmed per line at render time so
/// editors that auto-pad don't knock `Alignment::Center` off balance.
const HOME_LOGO: &str = include_str!("../../assets/home_logo.txt");

/// Standard bordered header used on every screen. The app name + version go
/// in the top border so branding stays consistent, and the screen's own
/// context line lives inside the box.
pub fn app_header<'a>(subtitle: Line<'a>) -> Paragraph<'a> {
    let title = format!(" 📊 {APP_NAME}  v{APP_VERSION} ");
    // Pad the subtitle with a blank row above and below so the content
    // doesn't hug the box borders. Screens render the header into a
    // 5-row Length constraint: top border + pad + subtitle + pad + bottom
    // border.
    let body = vec![Line::from(""), subtitle, Line::from("")];
    Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::ACCENT))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            )),
    )
}

/// Layout constraint height for `app_header` — 5 rows. Use this in screen
/// layouts so the header always gets the right amount of space.
pub const HEADER_HEIGHT: u16 = 5;

/// Welcome banner rendered on the sessions list when there are no sessions
/// yet. Returned as raw `Line`s — the caller centers them via
/// `Paragraph::alignment(Alignment::Center)`.
pub fn home_banner() -> Vec<Line<'static>> {
    let accent = Style::default()
        .fg(theme::ACCENT)
        .add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));
    for row in HOME_LOGO.lines() {
        let trimmed = row.trim_end();
        if trimmed.is_empty() && lines.last().is_some_and(|l| l.spans.is_empty()) {
            // Collapse runs of blank rows so stray trailing newlines in the
            // asset file don't inflate the banner.
            continue;
        }
        lines.push(Line::from(Span::styled(trimmed.to_string(), accent)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Android trace sessions  ·  v{APP_VERSION}"),
        theme::hint(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press [n] to create your first session",
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Each session targets one Android app on one device.",
        theme::hint(),
    )));
    lines.push(Line::from(Span::styled(
        "You'll pick a package name and a connected adb device,",
        theme::hint(),
    )));
    lines.push(Line::from(Span::styled(
        "then every capture runs against that pair.",
        theme::hint(),
    )));
    lines
}
