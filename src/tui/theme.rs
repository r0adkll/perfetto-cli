use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Cyan;
pub const DIM: Color = Color::DarkGray;
#[allow(dead_code)]
pub const OK: Color = Color::Green;
#[allow(dead_code)]
pub const WARN: Color = Color::Yellow;
#[allow(dead_code)]
pub const ERR: Color = Color::Red;

pub fn title() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn hint() -> Style {
    Style::default().fg(DIM)
}

/// A transient status message that auto-dismisses after a timeout.
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
