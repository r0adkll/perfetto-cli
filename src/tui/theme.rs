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
