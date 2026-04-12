use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};

// ---------------------------------------------------------------------------
// Color accessors — read from the opaline global theme so hot-swapping works.
// ---------------------------------------------------------------------------

pub fn accent() -> Color {
    Color::from(opaline::current().color("accent.primary"))
}

pub fn accent_secondary() -> Color {
    Color::from(opaline::current().color("accent.secondary"))
}

pub fn dim() -> Color {
    Color::from(opaline::current().color("text.dim"))
}

#[allow(dead_code)]
pub fn ok() -> Color {
    Color::from(opaline::current().color("success"))
}

#[allow(dead_code)]
pub fn warn() -> Color {
    Color::from(opaline::current().color("warning"))
}

#[allow(dead_code)]
pub fn err() -> Color {
    Color::from(opaline::current().color("error"))
}

// ---------------------------------------------------------------------------
// Style helpers
// ---------------------------------------------------------------------------

pub fn title() -> Style {
    Style::default()
        .fg(accent())
        .add_modifier(Modifier::BOLD)
}

pub fn hint() -> Style {
    Style::default().fg(Color::from(opaline::current().color("text.muted")))
}

// ---------------------------------------------------------------------------
// Transient status message (unchanged — not color-related)
// ---------------------------------------------------------------------------

const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    created: Instant,
}

impl StatusMessage {
    pub fn new(text: String) -> Self {
        Self {
            text,
            created: Instant::now(),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.created.elapsed() >= STATUS_TIMEOUT
    }
}

/// Holds an auto-expiring status. Call `get()` on each render — it returns
/// `None` once the timeout has passed and clears itself.
#[derive(Debug, Clone, Default)]
pub struct Status(Option<StatusMessage>);

impl Status {
    pub fn set(&mut self, text: String) {
        self.0 = Some(StatusMessage::new(text));
    }

    /// Returns the message if it hasn't expired yet. Automatically clears
    /// expired messages so subsequent calls return `None`.
    pub fn get(&mut self) -> Option<&str> {
        if self.0.as_ref().is_some_and(|m| m.is_expired()) {
            self.0 = None;
        }
        self.0.as_ref().map(|m| m.text.as_str())
    }

    pub fn clear(&mut self) {
        self.0 = None;
    }
}
