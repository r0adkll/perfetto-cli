use std::sync::atomic::{AtomicU64, Ordering};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::tui::{text_input, theme};

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

pub(super) struct ActivityPicker {
    pub request_id: u64,
    package: String,
    query: String,
    activities: Vec<String>,
    matches: Vec<Option<String>>,
    state: ListState,
    current: Option<String>,
    loading: bool,
    error: Option<String>,
}

pub(super) enum PickerAction {
    None,
    Select(Option<String>),
    Cancel,
    Manual,
}

impl ActivityPicker {
    pub fn new(package: String, current: Option<String>) -> Self {
        let mut picker = Self {
            request_id: NEXT_REQUEST.fetch_add(1, Ordering::Relaxed),
            package,
            query: String::new(),
            activities: current.iter().cloned().collect(),
            matches: Vec::new(),
            state: ListState::default(),
            current,
            loading: true,
            error: None,
        };
        picker.refilter();
        picker
    }

    pub fn on_loaded(&mut self, result: Result<Vec<String>, String>) {
        self.loading = false;
        match result {
            Ok(mut activities) => {
                // Preserve a saved override even if it is absent on this device.
                if let Some(current) = &self.current {
                    activities.push(current.clone());
                }
                activities.sort();
                activities.dedup();
                self.activities = activities;
            }
            Err(error) => self.error = Some(error),
        }
        self.refilter();
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PickerAction {
        match (key.code, key.modifiers) {
            (KeyCode::Char('m'), mods) if mods.contains(KeyModifiers::ALT) => {
                return PickerAction::Manual;
            }
            (KeyCode::Down | KeyCode::Tab, _) => self.move_selection(1),
            (KeyCode::Up | KeyCode::BackTab, _) => self.move_selection(-1),
            _ => match text_input::apply(&mut self.query, &key) {
                text_input::TextAction::Edited => self.refilter(),
                text_input::TextAction::Cancel => return PickerAction::Cancel,
                text_input::TextAction::Submit => {
                    if let Some(selected) = self.state.selected().and_then(|i| self.matches.get(i))
                    {
                        return PickerAction::Select(selected.clone());
                    }
                }
                _ => {}
            },
        }
        PickerAction::None
    }

    pub fn on_paste(&mut self, text: &str) {
        self.query.push_str(&text.replace(['\n', '\r'], " "));
        self.refilter();
    }

    fn move_selection(&mut self, delta: isize) {
        if !self.matches.is_empty() {
            let index = self.state.selected().unwrap_or(0) as isize;
            self.state.select(Some(
                (index + delta).rem_euclid(self.matches.len() as isize) as usize,
            ));
        }
    }

    fn refilter(&mut self) {
        let previous = self
            .state
            .selected()
            .and_then(|i| self.matches.get(i))
            .cloned();
        let mut ranked: Vec<_> = self
            .activities
            .iter()
            .filter_map(|activity| {
                let class = activity
                    .split_once('/')
                    .map_or(activity.as_str(), |(_, class)| class);
                fuzzy_score(class, &self.query).map(|score| (score, activity.clone()))
            })
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        self.matches = ranked
            .into_iter()
            .map(|(_, activity)| Some(activity))
            .collect();
        if self.query.trim().is_empty() {
            self.matches.insert(0, None);
        }
        // New searches start at the best match. Async loads preserve navigation.
        let selected = if self.query.is_empty() {
            previous
                .or_else(|| Some(self.current.clone()))
                .and_then(|selected| self.matches.iter().position(|item| item == &selected))
                .unwrap_or(0)
        } else {
            0
        };
        self.state
            .select((!self.matches.is_empty()).then_some(selected));
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let height = area.height.min(22);
        let width = area.width.min(100);
        let popup = ratatui::layout::Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::accent()))
            .title(Span::styled(
                format!(" Launch activity — {} ", self.package),
                theme::title(),
            ));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(inner);
        frame.render_widget(
            Paragraph::new(format!(" {}█", self.query)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Fuzzy search "),
            ),
            rows[0],
        );

        if self.matches.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    " No matches. Edit the search or use Alt+M for manual entry.",
                    theme::hint(),
                ))
                .wrap(Wrap { trim: false }),
                rows[1],
            );
        } else {
            let items: Vec<_> = self
                .matches
                .iter()
                .map(|activity| {
                    let label =
                        activity
                            .as_deref()
                            .map_or("(auto) — default launcher", |component| {
                                component
                                    .split_once('/')
                                    .map_or(component, |(_, class)| class)
                            });
                    let suffix = if activity.is_some() && activity == &self.current {
                        "  [current]"
                    } else {
                        ""
                    };
                    ListItem::new(format!(" {label}{suffix}"))
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(Style::default().bg(theme::accent()).fg(Color::Black))
                    .highlight_symbol("▶ "),
                rows[1],
                &mut self.state,
            );
        }
        let status = if self.loading {
            " Loading activities via ADB…".to_string()
        } else if let Some(error) = &self.error {
            format!(" ADB: {error}")
        } else if self.activities.is_empty() {
            " No activities found for this package. Alt+M for manual entry.".to_string()
        } else {
            format!(" {} activities", self.activities.len())
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                status,
                if self.error.is_some() {
                    Style::default().fg(theme::err())
                } else {
                    theme::hint()
                },
            ))
            .wrap(Wrap { trim: false }),
            rows[2],
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " ↑/↓ move • Enter select • Esc cancel • Alt+M manual",
                theme::hint(),
            )))
            .wrap(Wrap { trim: false }),
            rows[3],
        );
    }
}

/// Case-insensitive subsequence matching, rewarding adjacent characters and
/// word/CamelCase boundaries. Each whitespace-separated term must match.
fn fuzzy_score(candidate: &str, query: &str) -> Option<i64> {
    if query.trim().is_empty() {
        return Some(0);
    }
    let chars: Vec<_> = candidate.chars().collect();
    let mut total = -(chars.len() as i64);
    for term in query.split_whitespace() {
        let mut previous = vec![None::<i64>; chars.len()];
        for (query_index, needle) in term.chars().enumerate() {
            let mut next = vec![None; chars.len()];
            let mut best_gap = None::<i64>;
            for (i, &ch) in chars.iter().enumerate() {
                if i > 0
                    && let Some(score) = previous[i - 1]
                {
                    let gap_score = score + i as i64 - 1;
                    best_gap = Some(best_gap.map_or(gap_score, |best| best.max(gap_score)));
                }
                if !ch.to_lowercase().eq(needle.to_lowercase()) {
                    continue;
                }
                let boundary = i == 0
                    || !chars[i - 1].is_alphanumeric()
                    || (ch.is_uppercase() && chars[i - 1].is_lowercase());
                let bonus = if boundary { 16 } else { 0 };
                next[i] = if query_index == 0 {
                    Some(32 + bonus - i as i64)
                } else {
                    let adjacent = i
                        .checked_sub(1)
                        .and_then(|j| previous[j])
                        .map(|score| score + 24);
                    let gap = best_gap.map(|score| score - i as i64 + 1);
                    adjacent
                        .into_iter()
                        .chain(gap)
                        .max()
                        .map(|score| score + 32 + bonus)
                };
            }
            previous = next;
        }
        total += previous.into_iter().flatten().max()?;
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn fuzzy_search_supports_initials_case_and_multiple_terms() {
        assert!(fuzzy_score(".MainActivity", "MA").is_some());
        assert!(fuzzy_score(".MainActivity", "mnact").is_some());
        assert!(fuzzy_score(".MainActivity", "main act").is_some());
        assert!(fuzzy_score(".MainActivity", "z").is_none());
        assert!(fuzzy_score(".MainActivity", "aaam").is_none());
        assert!(
            fuzzy_score(".MainActivity", "main")
                > fuzzy_score(".MyAnotherInternalNameActivity", "main")
        );
    }

    #[test]
    fn typing_filters_and_enter_returns_full_component() {
        let mut picker = ActivityPicker::new("com.example".into(), None);
        picker.on_loaded(Ok(vec![
            "com.example/.MainActivity".into(),
            "com.example/.SettingsActivity".into(),
        ]));
        picker.on_paste("sa");
        assert_eq!(
            picker.matches,
            vec![Some("com.example/.SettingsActivity".into())]
        );
        assert!(
            matches!(picker.on_key(key(KeyCode::Enter)), PickerAction::Select(Some(activity)) if activity == "com.example/.SettingsActivity")
        );
        picker.on_paste("zz");
        assert!(matches!(
            picker.on_key(key(KeyCode::Enter)),
            PickerAction::None
        ));
        picker.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(picker.matches.len(), 3);
        assert!(matches!(
            picker.on_key(key(KeyCode::Enter)),
            PickerAction::Select(None)
        ));
    }

    #[test]
    fn navigation_cancel_manual_and_failed_load_preserve_current() {
        let mut picker =
            ActivityPicker::new("com.example".into(), Some("com.example/.Main".into()));
        picker.on_loaded(Err("device offline".into()));
        assert_eq!(picker.state.selected(), Some(1));
        picker.on_key(key(KeyCode::Down));
        assert_eq!(picker.state.selected(), Some(0));
        picker.on_key(key(KeyCode::Up));
        assert_eq!(picker.state.selected(), Some(1));
        assert!(matches!(
            picker.on_key(key(KeyCode::Esc)),
            PickerAction::Cancel
        ));
        assert!(matches!(
            picker.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT)),
            PickerAction::Manual
        ));
    }
}
