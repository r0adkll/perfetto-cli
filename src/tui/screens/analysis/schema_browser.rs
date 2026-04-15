//! Right-side schema browser for the REPL tab.
//!
//! Rendered at terminal width ≥ 120 cols (hidden below to leave room
//! for the main pane). Displays the loaded trace's tables
//! alphabetically, expandable to their column lists. Focus toggles via
//! `Alt+B`; while focused:
//!
//! * Arrow keys / Enter — navigate and expand / collapse.
//! * `Alt+I` — insert the highlighted name at the REPL cursor (auto-
//!   unfocuses after insertion so the user can see what they got).
//!   `i` with no modifier types into the filter instead — the two
//!   would collide otherwise.
//! * Printable chars (letters / digits / `_` / `.`) — append to the
//!   type-to-filter buffer; the tree narrows to names containing the
//!   filter (substring, case-insensitive). Backspace pops.
//! * `Esc` — clear a non-empty filter, otherwise unfocus.
//!
//! Tables currently referenced in the REPL's FROM-scope render in
//! `accent_secondary` so users can see at a glance which branches
//! their query is touching.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::tui::theme;

/// Outcome of a key press handled while the browser is focused.
pub enum BrowserAction {
    None,
    /// User asked to drop focus — the screen should return focus to
    /// whichever tab is active.
    Unfocus,
    /// Drop this text at the current REPL cursor position. Parent is
    /// also expected to unfocus the browser so the editor takes keys
    /// again.
    Insert(String),
}

#[derive(Debug, Clone)]
enum VisibleRow {
    Table { name: String, expanded: bool, has_cols: bool },
    Column { table: String, name: String },
}

pub struct SchemaBrowser {
    tables: Vec<String>,
    columns: HashMap<String, Vec<String>>,
    expanded: HashSet<String>,
    cursor: usize,
    focused: bool,
    /// Type-to-filter buffer. Non-empty filters narrow `visible_rows`
    /// to substring matches (case-insensitive) on either table or
    /// column name; tables with any matching column auto-expand so
    /// the hit is visible without further input.
    filter: String,
    /// Tables in the REPL editor's current FROM-scope. Updated each
    /// frame by the parent screen via [`set_scoped`]. Rendering uses
    /// this to distinguish tables the user's query already references.
    scoped: HashSet<String>,
}

impl SchemaBrowser {
    pub fn new() -> Self {
        Self {
            tables: Vec::new(),
            columns: HashMap::new(),
            expanded: HashSet::new(),
            cursor: 0,
            focused: false,
            filter: String::new(),
            scoped: HashSet::new(),
        }
    }

    pub fn set_scoped(&mut self, scoped: HashSet<String>) {
        self.scoped = scoped;
    }

    pub fn set_schema(&mut self, mut tables: Vec<String>) {
        tables.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
        self.tables = tables;
        self.clamp_cursor();
    }

    pub fn set_columns(&mut self, mut by_table: HashMap<String, Vec<String>>) {
        // Sort columns within each table for consistent browsing.
        for cols in by_table.values_mut() {
            cols.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
        }
        self.columns = by_table;
        self.clamp_cursor();
    }

    pub fn set_focused(&mut self, f: bool) {
        self.focused = f;
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Handle a key event while the browser has focus. Unhandled keys
    /// return `BrowserAction::None` so the parent can decide whether
    /// to do anything else with them (we currently don't).
    pub fn on_key(&mut self, key: KeyEvent) -> BrowserAction {
        if key.kind != KeyEventKind::Press {
            return BrowserAction::None;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Alt+B always unfocuses. Escape clears a non-empty filter
        // first, then unfocuses on the next press.
        if alt && matches!(key.code, KeyCode::Char('b') | KeyCode::Char('B')) {
            return BrowserAction::Unfocus;
        }
        if matches!(key.code, KeyCode::Esc) {
            if !self.filter.is_empty() {
                self.filter.clear();
                self.cursor = 0;
                return BrowserAction::None;
            }
            return BrowserAction::Unfocus;
        }

        // `Alt+I` inserts the highlighted label at the REPL cursor.
        // Plain `i` is reserved for the filter buffer below.
        if alt && matches!(key.code, KeyCode::Char('i') | KeyCode::Char('I')) {
            let visible = self.visible_rows();
            if let Some(row) = visible.get(self.cursor) {
                let label = match row {
                    VisibleRow::Table { name, .. } => name.clone(),
                    VisibleRow::Column { name, .. } => name.clone(),
                };
                return BrowserAction::Insert(label);
            }
            return BrowserAction::None;
        }

        // Filter editing. Backspace pops the filter; printable chars
        // (letters, digits, `_`, `.`, `-`) append. Any non-modifier
        // char that matches this set is consumed as filter input
        // rather than falling through to navigation.
        if matches!(key.code, KeyCode::Backspace) && !self.filter.is_empty() {
            self.filter.pop();
            self.cursor = 0;
            return BrowserAction::None;
        }
        if !alt && !ctrl {
            if let KeyCode::Char(c) = key.code {
                if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                    self.filter.push(c);
                    self.cursor = 0;
                    return BrowserAction::None;
                }
            }
        }

        let visible = self.visible_rows();
        if visible.is_empty() {
            return BrowserAction::None;
        }

        match key.code {
            KeyCode::Up => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
            }
            KeyCode::Down => {
                if self.cursor + 1 < visible.len() {
                    self.cursor += 1;
                }
            }
            KeyCode::PageUp => {
                self.cursor = self.cursor.saturating_sub(10);
            }
            KeyCode::PageDown => {
                self.cursor = (self.cursor + 10).min(visible.len() - 1);
            }
            KeyCode::Home => {
                self.cursor = 0;
            }
            KeyCode::End => {
                self.cursor = visible.len() - 1;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.toggle_current();
            }
            KeyCode::Right => {
                self.expand_or_descend(&visible);
            }
            KeyCode::Left => {
                self.collapse_or_ascend(&visible);
            }
            _ => {}
        }
        BrowserAction::None
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let border_style = if self.focused {
            Style::default().fg(theme::accent())
        } else {
            Style::default().fg(theme::dim())
        };

        let title_text = if self.filter.is_empty() {
            format!(" Schema · {} tables ", self.tables.len())
        } else {
            format!(
                " Schema · {} tables · filter: \"{}\" ",
                self.tables.len(),
                self.filter,
            )
        };
        let title = Span::styled(title_text, theme::title());
        let hints: Line<'_> = if self.focused {
            Line::from(vec![
                Span::styled(" ", Style::default().fg(theme::dim())),
                Span::styled("[Alt+I]", theme::title()),
                Span::styled(" insert · ", Style::default().fg(theme::dim())),
                Span::styled("[type]", theme::title()),
                Span::styled(" filter · ", Style::default().fg(theme::dim())),
                Span::styled("[Esc]", theme::title()),
                Span::styled(
                    if self.filter.is_empty() { " unfocus " } else { " clear filter " },
                    Style::default().fg(theme::dim()),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled(" ", Style::default().fg(theme::dim())),
                Span::styled("[Alt+B]", theme::title()),
                Span::styled(" focus ", Style::default().fg(theme::dim())),
            ])
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(title)
            .title_bottom(hints);

        if self.tables.is_empty() {
            let p = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                "  (loading…)",
                Style::default().fg(theme::dim()),
            )))
            .block(block);
            frame.render_widget(p, area);
            return;
        }

        let visible = self.visible_rows();
        if visible.is_empty() {
            let p = ratatui::widgets::Paragraph::new(Line::from(Span::styled(
                format!("  no matches for \"{}\"", self.filter),
                Style::default().fg(theme::dim()),
            )))
            .block(block);
            frame.render_widget(p, area);
            return;
        }
        let inner_rows = area.height.saturating_sub(2) as usize;
        let start = if visible.len() <= inner_rows {
            0
        } else {
            let half = inner_rows / 2;
            let max_start = visible.len().saturating_sub(inner_rows);
            self.cursor.saturating_sub(half).min(max_start)
        };
        let items: Vec<ListItem> = visible
            .iter()
            .enumerate()
            .skip(start)
            .take(inner_rows)
            .map(|(i, row)| {
                let selected = i == self.cursor && self.focused;
                let in_scope = match row {
                    VisibleRow::Table { name, .. } => self.scoped.contains(name),
                    VisibleRow::Column { table, .. } => self.scoped.contains(table),
                };
                row_to_list_item(row, selected, in_scope)
            })
            .collect();
        let list = List::new(items).block(block);
        frame.render_widget(list, area);
    }

    // ── internals ────────────────────────────────────────────────────────

    fn visible_rows(&self) -> Vec<VisibleRow> {
        let needle = self.filter.to_ascii_lowercase();
        let filter_active = !needle.is_empty();
        let mut out = Vec::new();
        for t in &self.tables {
            let table_match = !filter_active || t.to_ascii_lowercase().contains(&needle);
            let cols_for_table = self.columns.get(t);
            let matching_cols: Vec<&String> = if filter_active {
                cols_for_table
                    .map(|cs| {
                        cs.iter()
                            .filter(|c| c.to_ascii_lowercase().contains(&needle))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            // Skip tables that don't match and have no matching columns.
            if filter_active && !table_match && matching_cols.is_empty() {
                continue;
            }
            let has_cols = cols_for_table.map(|c| !c.is_empty()).unwrap_or(false);
            // Filter mode auto-expands tables with matching columns so
            // the hit is visible without further keypresses. User-set
            // expanded state still applies when no filter is active.
            let effectively_expanded = if filter_active {
                !matching_cols.is_empty()
            } else {
                self.expanded.contains(t)
            };
            out.push(VisibleRow::Table {
                name: t.clone(),
                expanded: effectively_expanded,
                has_cols,
            });
            if effectively_expanded {
                let cols_to_show: Box<dyn Iterator<Item = &String>> = if filter_active {
                    if table_match {
                        // Whole table matches — show all columns.
                        Box::new(cols_for_table.into_iter().flatten())
                    } else {
                        Box::new(matching_cols.into_iter())
                    }
                } else {
                    Box::new(cols_for_table.into_iter().flatten())
                };
                for c in cols_to_show {
                    out.push(VisibleRow::Column {
                        table: t.clone(),
                        name: c.clone(),
                    });
                }
            }
        }
        out
    }

    fn clamp_cursor(&mut self) {
        let n = self.visible_rows().len();
        if n == 0 {
            self.cursor = 0;
        } else if self.cursor >= n {
            self.cursor = n - 1;
        }
    }

    fn toggle_current(&mut self) {
        let visible = self.visible_rows();
        let Some(row) = visible.get(self.cursor) else {
            return;
        };
        match row {
            VisibleRow::Table { name, has_cols, .. } => {
                if !*has_cols {
                    return;
                }
                if self.expanded.contains(name) {
                    self.expanded.remove(name);
                } else {
                    self.expanded.insert(name.clone());
                }
                self.clamp_cursor();
            }
            VisibleRow::Column { .. } => {
                // Toggling on a column does nothing; user likely wants
                // to collapse the parent — handle via Left instead.
            }
        }
    }

    fn expand_or_descend(&mut self, visible: &[VisibleRow]) {
        let Some(row) = visible.get(self.cursor) else {
            return;
        };
        if let VisibleRow::Table { name, expanded, has_cols } = row {
            if !*has_cols {
                return;
            }
            if !*expanded {
                self.expanded.insert(name.clone());
            } else if self.cursor + 1 < visible.len() {
                self.cursor += 1;
            }
        }
    }

    fn collapse_or_ascend(&mut self, visible: &[VisibleRow]) {
        let Some(row) = visible.get(self.cursor) else {
            return;
        };
        match row {
            VisibleRow::Table { name, expanded, .. } => {
                if *expanded {
                    self.expanded.remove(name);
                }
            }
            VisibleRow::Column { table, .. } => {
                // Jump back to the parent table row.
                let parent_idx = visible.iter().position(|r| {
                    matches!(r, VisibleRow::Table { name, .. } if name == table)
                });
                if let Some(i) = parent_idx {
                    self.cursor = i;
                }
            }
        }
    }

    #[cfg(test)]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    #[cfg(test)]
    pub fn visible_len_for_test(&self) -> usize {
        self.visible_rows().len()
    }

    #[cfg(test)]
    pub fn is_expanded_for_test(&self, name: &str) -> bool {
        self.expanded.contains(name)
    }
}

fn row_to_list_item(row: &VisibleRow, selected: bool, in_scope: bool) -> ListItem<'_> {
    // Style precedence: selected > in_scope > default.
    let label_style = if selected {
        Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD)
    } else if in_scope {
        Style::default()
            .fg(theme::accent_secondary())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let marker = if selected { "▶ " } else { "  " };
    let marker_style = if selected {
        Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::dim())
    };
    match row {
        VisibleRow::Table { name, expanded, has_cols } => {
            let disclosure = if !*has_cols {
                "  "
            } else if *expanded {
                "▾ "
            } else {
                "▸ "
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(
                    disclosure,
                    Style::default().fg(theme::accent_secondary()),
                ),
                Span::styled(name.clone(), label_style),
            ]))
        }
        VisibleRow::Column { name, .. } => {
            let col_style = if selected {
                label_style
            } else if in_scope {
                Style::default().fg(theme::accent_secondary())
            } else {
                Style::default().fg(theme::dim())
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::raw("      "),
                Span::styled(name.clone(), col_style),
            ]))
        }
    }
}

// ── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }
    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn seeded(tables: &[&str], cols: &[(&str, &[&str])]) -> SchemaBrowser {
        let mut sb = SchemaBrowser::new();
        sb.set_schema(tables.iter().map(|s| (*s).to_string()).collect());
        let mut map = HashMap::new();
        for (t, cs) in cols {
            map.insert(
                (*t).to_string(),
                cs.iter().map(|s| (*s).to_string()).collect(),
            );
        }
        sb.set_columns(map);
        sb
    }

    #[test]
    fn set_schema_sorts_alphabetically_case_insensitive() {
        let mut sb = SchemaBrowser::new();
        sb.set_schema(vec![
            "slice".into(),
            "Actual".into(),
            "process".into(),
        ]);
        assert_eq!(sb.tables, vec!["Actual", "process", "slice"]);
    }

    #[test]
    fn enter_expands_table_and_shows_columns() {
        let mut sb = seeded(
            &["slice", "thread"],
            &[("slice", &["ts", "dur", "name"])],
        );
        sb.set_focused(true);
        assert_eq!(sb.visible_len_for_test(), 2);
        // Cursor is on the first row ("slice").
        sb.on_key(key(KeyCode::Enter));
        assert!(sb.is_expanded_for_test("slice"));
        assert_eq!(sb.visible_len_for_test(), 2 + 3);
    }

    #[test]
    fn right_arrow_expands_then_descends_into_first_column() {
        let mut sb = seeded(&["slice"], &[("slice", &["ts", "dur"])]);
        sb.set_focused(true);
        // First Right: expand.
        sb.on_key(key(KeyCode::Right));
        assert!(sb.is_expanded_for_test("slice"));
        assert_eq!(sb.cursor(), 0);
        // Second Right: descend to first column.
        sb.on_key(key(KeyCode::Right));
        assert_eq!(sb.cursor(), 1);
    }

    #[test]
    fn left_on_column_jumps_to_parent_table() {
        let mut sb = seeded(&["slice"], &[("slice", &["ts", "dur"])]);
        sb.set_focused(true);
        sb.on_key(key(KeyCode::Enter)); // expand
        sb.on_key(key(KeyCode::Down)); // onto "ts"
        assert_eq!(sb.cursor(), 1);
        sb.on_key(key(KeyCode::Left));
        assert_eq!(sb.cursor(), 0);
        // Parent still expanded — Left on column shouldn't collapse.
        assert!(sb.is_expanded_for_test("slice"));
    }

    #[test]
    fn left_on_expanded_table_collapses_it() {
        let mut sb = seeded(&["slice"], &[("slice", &["ts"])]);
        sb.set_focused(true);
        sb.on_key(key(KeyCode::Enter)); // expand
        sb.on_key(key(KeyCode::Left));
        assert!(!sb.is_expanded_for_test("slice"));
    }

    #[test]
    fn up_down_bounded_at_edges() {
        let mut sb = seeded(&["a", "b"], &[]);
        sb.set_focused(true);
        sb.on_key(key(KeyCode::Up));
        assert_eq!(sb.cursor(), 0);
        sb.on_key(key(KeyCode::Down));
        sb.on_key(key(KeyCode::Down));
        sb.on_key(key(KeyCode::Down));
        assert_eq!(sb.cursor(), 1);
    }

    #[test]
    fn esc_returns_unfocus_action() {
        let mut sb = seeded(&["a"], &[]);
        sb.set_focused(true);
        let out = sb.on_key(key(KeyCode::Esc));
        assert!(matches!(out, BrowserAction::Unfocus));
    }

    #[test]
    fn alt_b_also_returns_unfocus_action() {
        let mut sb = seeded(&["a"], &[]);
        sb.set_focused(true);
        let out = sb.on_key(alt(KeyCode::Char('b')));
        assert!(matches!(out, BrowserAction::Unfocus));
    }

    #[test]
    fn toggle_does_nothing_on_table_without_columns() {
        let mut sb = seeded(&["slice"], &[]);
        sb.set_focused(true);
        sb.on_key(key(KeyCode::Enter));
        assert!(!sb.is_expanded_for_test("slice"));
    }

    #[test]
    fn alt_i_emits_insert_of_current_label() {
        let mut sb = seeded(&["slice"], &[("slice", &["ts"])]);
        sb.set_focused(true);
        let out = sb.on_key(alt(KeyCode::Char('i')));
        match out {
            BrowserAction::Insert(name) => assert_eq!(name, "slice"),
            _ => panic!("expected Insert action"),
        }
    }

    #[test]
    fn alt_i_on_column_inserts_column_name() {
        let mut sb = seeded(&["slice"], &[("slice", &["ts"])]);
        sb.set_focused(true);
        sb.on_key(key(KeyCode::Enter)); // expand
        sb.on_key(key(KeyCode::Down)); // onto `ts`
        match sb.on_key(alt(KeyCode::Char('i'))) {
            BrowserAction::Insert(name) => assert_eq!(name, "ts"),
            _ => panic!("expected Insert on column"),
        }
    }

    #[test]
    fn plain_i_types_into_filter_not_insert() {
        let mut sb = seeded(&["slice"], &[("slice", &["id", "ts"])]);
        sb.set_focused(true);
        let out = sb.on_key(key(KeyCode::Char('i')));
        assert!(
            matches!(out, BrowserAction::None),
            "plain `i` must not emit Insert when filter typing is possible",
        );
        assert_eq!(sb.filter, "i");
    }

    #[test]
    fn typing_letters_filters_the_tree() {
        let mut sb = seeded(
            &["slice", "thread", "process", "android_startups"],
            &[],
        );
        sb.set_focused(true);
        sb.on_key(key(KeyCode::Char('s')));
        sb.on_key(key(KeyCode::Char('t')));
        let rows = sb.visible_rows();
        let names: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                VisibleRow::Table { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        // `st` substring matches android_startups only (as a substring
        // of the full name) — `slice`, `thread`, `process` don't
        // contain "st".
        assert_eq!(names, vec!["android_startups"]);
    }

    #[test]
    fn filter_auto_expands_tables_with_matching_columns() {
        let mut sb = seeded(&["slice"], &[("slice", &["ts", "dur", "name"])]);
        sb.set_focused(true);
        // Type a substring that matches a column, not the table.
        sb.on_key(key(KeyCode::Char('d')));
        sb.on_key(key(KeyCode::Char('u')));
        sb.on_key(key(KeyCode::Char('r')));
        let rows = sb.visible_rows();
        let has_col = rows.iter().any(|r| matches!(r, VisibleRow::Column { name, .. } if name == "dur"));
        assert!(has_col, "filter should auto-expand tables to show matching columns");
    }

    #[test]
    fn backspace_pops_filter() {
        let mut sb = seeded(&["slice", "thread"], &[]);
        sb.set_focused(true);
        sb.on_key(key(KeyCode::Char('s')));
        sb.on_key(key(KeyCode::Char('l')));
        assert_eq!(sb.filter, "sl");
        sb.on_key(key(KeyCode::Backspace));
        assert_eq!(sb.filter, "s");
    }

    #[test]
    fn esc_clears_non_empty_filter_without_unfocusing() {
        let mut sb = seeded(&["slice"], &[]);
        sb.set_focused(true);
        sb.on_key(key(KeyCode::Char('x')));
        let out = sb.on_key(key(KeyCode::Esc));
        assert!(matches!(out, BrowserAction::None));
        assert_eq!(sb.filter, "");
    }

    #[test]
    fn esc_on_empty_filter_unfocuses() {
        let mut sb = seeded(&["slice"], &[]);
        sb.set_focused(true);
        let out = sb.on_key(key(KeyCode::Esc));
        assert!(matches!(out, BrowserAction::Unfocus));
    }

    #[test]
    fn set_scoped_marks_tables_visibly_in_style() {
        let mut sb = seeded(&["slice", "thread"], &[]);
        let mut scope = HashSet::new();
        scope.insert("slice".into());
        sb.set_scoped(scope);
        assert!(sb.scoped.contains("slice"));
        assert!(!sb.scoped.contains("thread"));
    }

    #[test]
    fn set_columns_sorts_each_table_case_insensitive() {
        let mut sb = seeded(
            &["slice"],
            &[("slice", &["name", "Ts", "dur"])],
        );
        // Expand and read column order from visible rows.
        sb.set_focused(true);
        sb.on_key(key(KeyCode::Enter));
        let rows = sb.visible_rows();
        let col_names: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                VisibleRow::Column { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(col_names, vec!["dur", "name", "Ts"]);
    }
}
