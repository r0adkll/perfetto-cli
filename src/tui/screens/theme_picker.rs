use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use opaline::{Theme, ThemeInfo, ThemeVariant};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::chrome;
use crate::tui::theme;

pub struct ThemePickerScreen {
    themes: Vec<ThemeInfo>,
    cache: Vec<Theme>,
    search_cache: Vec<(String, String)>,
    filter: String,
    filtered: Vec<usize>,
    cursor: usize,
    scroll: usize,
    original: Theme,
}

pub enum ThemePickerAction {
    None,
    Back,
    Selected(String),
}

impl ThemePickerScreen {
    pub fn new(themes_dir: PathBuf) -> Self {
        let mut themes = opaline::list_available_themes_in_dirs(vec![themes_dir]);
        themes.sort_by(|a, b| {
            let v = match (&a.variant, &b.variant) {
                (ThemeVariant::Dark, ThemeVariant::Light) => std::cmp::Ordering::Less,
                (ThemeVariant::Light, ThemeVariant::Dark) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            };
            v.then_with(|| a.display_name.cmp(&b.display_name))
        });

        let cache: Vec<Theme> = themes
            .iter()
            .map(|info| info.load().unwrap_or_default())
            .collect();
        let search_cache: Vec<(String, String)> = themes
            .iter()
            .map(|info| (info.display_name.to_lowercase(), info.author.to_lowercase()))
            .collect();
        let filtered: Vec<usize> = (0..themes.len()).collect();
        let original = (*opaline::current()).clone();

        // Pre-select the current theme.
        let current_name = &original.meta.name;
        let mut cursor = 0;
        let mut scroll = 0;
        if let Some(pos) = themes
            .iter()
            .position(|i| &i.display_name == current_name || &i.name == current_name)
        {
            if let Some(cp) = filtered.iter().position(|&i| i == pos) {
                cursor = cp;
                scroll = cp.saturating_sub(8);
            }
        }

        Self {
            themes,
            cache,
            search_cache,
            filter: String::new(),
            filtered,
            cursor,
            scroll,
            original,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> ThemePickerAction {
        if key.kind != KeyEventKind::Press {
            return ThemePickerAction::None;
        }

        match key.code {
            KeyCode::Esc => {
                opaline::set_theme(self.original.clone());
                ThemePickerAction::Back
            }
            KeyCode::Enter => {
                if let Some(&idx) = self.filtered.get(self.cursor) {
                    ThemePickerAction::Selected(self.themes[idx].name.clone())
                } else {
                    ThemePickerAction::None
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.apply_preview();
                }
                ThemePickerAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.cursor + 1 < self.filtered.len() {
                    self.cursor += 1;
                    self.apply_preview();
                }
                ThemePickerAction::None
            }
            KeyCode::Char(c) if !c.is_ascii_control() => {
                self.filter.push(c);
                self.recompute_filter();
                self.apply_preview();
                ThemePickerAction::None
            }
            KeyCode::Backspace => {
                if self.filter.pop().is_some() {
                    self.recompute_filter();
                    self.apply_preview();
                }
                ThemePickerAction::None
            }
            _ => ThemePickerAction::None,
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        let header = chrome::app_header(Line::from(Span::styled(
            "  Choose a theme",
            theme::hint(),
        )));
        frame.render_widget(header, chunks[0]);

        self.render_body(frame, chunks[1]);

        let footer = Line::from(vec![
            Span::styled(" [Enter]", theme::title()),
            Span::raw(" select  "),
            Span::styled("[Esc]", theme::title()),
            Span::raw(" cancel  "),
            Span::styled("[type]", theme::title()),
            Span::raw(" filter"),
        ]);
        frame.render_widget(Paragraph::new(footer), chunks[2]);
    }

    // ── internals ────────────────────────────────────────────────────

    fn render_body(&mut self, frame: &mut Frame, area: Rect) {
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::accent()))
            .title(Span::styled(" Themes ", theme::title()));
        let inner = outer.inner(area);
        frame.render_widget(outer, area);

        if inner.height < 6 || inner.width < 20 {
            return;
        }

        // Two-pane: 55% list, 45% preview
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(inner);

        self.render_list(frame, panes[0]);
        self.render_preview(frame, panes[1]);
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        // Filter input
        let cursor_ch = if self.filter.is_empty() { "\u{2502}" } else { "\u{2588}" };
        let filter_line = Line::from(vec![
            Span::styled(" Filter: ", theme::hint()),
            Span::styled(&self.filter, Style::default().fg(theme::accent())),
            Span::styled(cursor_ch, Style::default().fg(theme::accent())),
        ]);
        frame.render_widget(Paragraph::new(filter_line), rows[0]);

        // Theme list
        let list_area = rows[1];
        let visible = list_area.height as usize;
        self.clamp_scroll(visible);

        let mut y = list_area.y;
        let max_y = list_area.y + list_area.height;
        let mut items_drawn = 0;
        let mut last_variant: Option<ThemeVariant> = None;

        for (fi, &ti) in self.filtered.iter().enumerate() {
            if y >= max_y {
                break;
            }

            let info = &self.themes[ti];

            // Section header on variant change
            if last_variant != Some(info.variant) {
                if items_drawn >= self.scroll || last_variant.is_none() {
                    if y < max_y {
                        let label = match info.variant {
                            ThemeVariant::Dark => " Dark Themes",
                            ThemeVariant::Light => " Light Themes",
                        };
                        let hdr = Line::from(Span::styled(
                            label,
                            Style::default()
                                .fg(theme::dim())
                                .add_modifier(Modifier::ITALIC),
                        ));
                        frame.render_widget(
                            Paragraph::new(hdr),
                            Rect::new(list_area.x, y, list_area.width, 1),
                        );
                        y += 1;
                    }
                }
                last_variant = Some(info.variant);
            }

            if items_drawn < self.scroll {
                items_drawn += 1;
                continue;
            }
            if y >= max_y {
                break;
            }

            let selected = fi == self.cursor;
            let mut spans = Vec::new();
            if selected {
                spans.push(Span::styled(
                    "  > ",
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    info.display_name.as_str(),
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::raw("    "));
                spans.push(Span::raw(info.display_name.as_str()));
            }
            if info.variant == ThemeVariant::Light {
                spans.push(Span::styled(
                    " \u{2600}",
                    Style::default().fg(theme::warn()),
                ));
            }
            if !info.builtin {
                spans.push(Span::styled(" *", theme::hint()));
            }

            let line = Line::from(spans);
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(list_area.x, y, list_area.width, 1),
            );
            y += 1;
            items_drawn += 1;
        }

        // Position indicator
        let total = self.filtered.len();
        let pos_text = if total > 0 {
            format!(" {}/{}", self.cursor + 1, total)
        } else {
            String::new()
        };
        let hint_line = Line::from(Span::styled(pos_text, theme::hint()));
        frame.render_widget(Paragraph::new(hint_line), rows[2]);
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        let border = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(theme::dim()));
        let inner = border.inner(area);
        frame.render_widget(border, area);

        let Some(&ti) = self.filtered.get(self.cursor) else {
            let msg = Line::from(Span::styled(" No themes match", theme::hint()));
            frame.render_widget(
                Paragraph::new(msg),
                Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(1), 1),
            );
            return;
        };

        let info = &self.themes[ti];
        let theme_obj = &self.cache[ti];

        let x = inner.x + 1;
        let w = inner.width.saturating_sub(2);
        let mut y = inner.y + 1;
        let max_y = inner.y + inner.height;

        // Theme name
        if y < max_y {
            let line = Line::from(Span::styled(
                info.display_name.as_str(),
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ));
            frame.render_widget(
                Paragraph::new(line),
                Rect::new(x, y, w, 1),
            );
            y += 1;
        }

        // Author
        if y < max_y && !info.author.is_empty() {
            let line = Line::from(vec![
                Span::styled("by ", theme::hint()),
                Span::raw(info.author.as_str()),
            ]);
            frame.render_widget(Paragraph::new(line), Rect::new(x, y, w, 1));
            y += 1;
        }

        y += 1; // spacer

        // Description
        if y < max_y && !info.description.is_empty() {
            for word_line in wrap(&info.description, w as usize) {
                if y >= max_y {
                    break;
                }
                frame.render_widget(
                    Paragraph::new(Line::from(word_line)),
                    Rect::new(x, y, w, 1),
                );
                y += 1;
            }
        }

        y += 1; // spacer

        // Variant
        if y < max_y {
            let label = match info.variant {
                ThemeVariant::Dark => "Dark",
                ThemeVariant::Light => "Light",
            };
            let line = Line::from(Span::styled(
                label,
                Style::default()
                    .fg(theme::dim())
                    .add_modifier(Modifier::ITALIC),
            ));
            frame.render_widget(Paragraph::new(line), Rect::new(x, y, w, 1));
            y += 1;
        }

        y += 1; // spacer

        // Color swatches
        if y < max_y {
            let tokens = [
                "accent.primary",
                "accent.secondary",
                "accent.tertiary",
                "success",
                "warning",
                "error",
            ];
            let swatches: Vec<Span> = tokens
                .iter()
                .flat_map(|tok| {
                    let c = Color::from(theme_obj.color(tok));
                    [
                        Span::styled("\u{2588}\u{2588}", Style::default().fg(c)),
                        Span::raw(" "),
                    ]
                })
                .collect();
            frame.render_widget(
                Paragraph::new(Line::from(swatches)),
                Rect::new(x, y, w, 1),
            );
        }
    }

    fn recompute_filter(&mut self) {
        let q = self.filter.to_lowercase();
        self.filtered = if q.is_empty() {
            (0..self.themes.len()).collect()
        } else {
            self.search_cache
                .iter()
                .enumerate()
                .filter(|(_, (name, author))| name.contains(&q) || author.contains(&q))
                .map(|(i, _)| i)
                .collect()
        };
        self.cursor = 0;
        self.scroll = 0;
    }

    fn apply_preview(&self) {
        if let Some(&idx) = self.filtered.get(self.cursor) {
            opaline::set_theme(self.cache[idx].clone());
        }
    }

    fn clamp_scroll(&mut self, visible: usize) {
        if visible == 0 {
            return;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        if self.cursor >= self.scroll + visible {
            self.scroll = self.cursor - visible + 1;
        }
    }
}

fn wrap(text: &str, max_w: usize) -> Vec<String> {
    if max_w == 0 {
        return vec![];
    }
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0;
    for word in text.split_whitespace() {
        let ww = word.len();
        if cur.is_empty() {
            cur = word.to_string();
            cur_w = ww;
        } else if cur_w + 1 + ww > max_w {
            lines.push(cur);
            cur = word.to_string();
            cur_w = ww;
        } else {
            cur.push(' ');
            cur.push_str(word);
            cur_w += 1 + ww;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}
