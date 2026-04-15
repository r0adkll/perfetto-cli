//! Completion for the REPL editor.
//!
//! Trigger with `Ctrl+Space` or `Alt+/` while the editor has focus. Opens
//! a popup anchored to the cursor with candidates filtered by the current
//! word-prefix. `Up`/`Down` to navigate, `Tab`/`Enter` to accept,
//! `Esc`/`Ctrl+Space` to dismiss. Typing while the popup is open
//! forwards to the editor and refreshes the filter; when the prefix
//! becomes empty the popup auto-closes.
//!
//! The static candidate set (SQL keywords, aggregates, core PerfettoSQL
//! tables) lives in [`static_candidates`]. [`ReplState`] merges those
//! with tables discovered at runtime from the loaded trace (fetched via
//! the worker's `LoadSchema` request) and passes the merged slice to
//! `CompletionState::open` / `refresh`. Column-level completion and
//! fuzzy matching remain deliberate follow-ups.
//!
//! Implementation note: the popup is positioned from `editor.cursor()`.
//! `ratatui_textarea`'s viewport is `pub(crate)`, so we can't read the
//! scroll offset from outside the crate — for now we assume the editor
//! doesn't scroll (the input pane is 14 rows and typical queries fit).
//! If the cursor would render below the editor inner area we anchor the
//! popup to the bottom edge instead.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui_textarea::{CursorMove, TextArea};

use crate::tui::theme;

// ── candidate database ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    Keyword,
    Function,
    Table,
    /// Runtime-discovered table (from the loaded trace's schema). Rendered
    /// with a distinct kind label so users can tell curated entries with
    /// doc strings apart from raw schema entries.
    SchemaTable,
    /// A column discovered via `PRAGMA table_info` on a loaded trace.
    /// Only offered when the editor's nearest FROM-scope lists the
    /// column's owning table.
    Column,
}

impl CandidateKind {
    fn label(self) -> &'static str {
        match self {
            CandidateKind::Keyword => "keyword",
            CandidateKind::Function => "fn",
            CandidateKind::Table => "table",
            CandidateKind::SchemaTable => "schema",
            CandidateKind::Column => "col",
        }
    }
}

/// A single completion entry. Fields use `Cow` so curated (static) entries
/// borrow string literals while schema-derived entries hold owned names
/// without a second allocation on each render pass. `origin` is set only
/// for columns — it records the owning table name so scope-aware
/// filtering can drop columns from tables not in the current FROM-scope.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub label: Cow<'static, str>,
    pub kind: CandidateKind,
    pub doc: Cow<'static, str>,
    pub origin: Option<Cow<'static, str>>,
}

use CandidateKind::{Function, Keyword, Table};

/// Build the curated static candidate list. Kept as a function (rather
/// than a `const`) because `Cow::Borrowed` isn't const-constructible; the
/// caller caches the result in a `LazyLock` or at `ReplState` startup.
pub fn static_candidates() -> Vec<Candidate> {
    macro_rules! c {
        ($label:expr, $kind:expr, $doc:expr) => {
            Candidate {
                label: Cow::Borrowed($label),
                kind: $kind,
                doc: Cow::Borrowed($doc),
                origin: None,
            }
        };
    }
    vec![
        // Core SQL keywords
        c!("SELECT", Keyword, "Start a query."),
        c!("FROM", Keyword, "Choose source table(s) for a SELECT."),
        c!("WHERE", Keyword, "Filter rows before grouping."),
        c!("GROUP BY", Keyword, "Bucket rows by column(s)."),
        c!("ORDER BY", Keyword, "Sort the result set."),
        c!("HAVING", Keyword, "Filter rows after grouping."),
        c!("LIMIT", Keyword, "Cap the row count."),
        c!("OFFSET", Keyword, "Skip the first N rows."),
        c!("DISTINCT", Keyword, "Drop duplicate rows from the output."),
        c!("JOIN", Keyword, "Combine rows from two tables."),
        c!("LEFT JOIN", Keyword, "Preserve all rows from the left table."),
        c!("INNER JOIN", Keyword, "Keep only rows matched by both tables."),
        c!("ON", Keyword, "Join predicate."),
        c!("USING", Keyword, "Join on same-named columns."),
        c!("AS", Keyword, "Alias a column or table."),
        c!("WITH", Keyword, "Define a CTE for reuse inside the query."),
        c!("UNION", Keyword, "Concatenate two SELECTs, dropping duplicates."),
        c!("UNION ALL", Keyword, "Concatenate two SELECTs, keeping duplicates."),
        c!("CASE", Keyword, "Conditional expression; pair with WHEN/THEN/ELSE/END."),
        c!("WHEN", Keyword, "CASE branch predicate."),
        c!("THEN", Keyword, "CASE branch value."),
        c!("ELSE", Keyword, "CASE fallback value."),
        c!("END", Keyword, "Close a CASE expression."),
        c!("IN", Keyword, "Set-membership predicate."),
        c!("BETWEEN", Keyword, "Inclusive range predicate."),
        c!("LIKE", Keyword, "Pattern match with % and _ wildcards."),
        c!("GLOB", Keyword, "Shell-style pattern match with * and ?."),
        c!("IS NULL", Keyword, "Test for NULL."),
        c!("IS NOT NULL", Keyword, "Test for non-NULL."),
        c!("AND", Keyword, "Logical conjunction."),
        c!("OR", Keyword, "Logical disjunction."),
        c!("NOT", Keyword, "Logical negation."),
        c!("CAST", Keyword, "Convert a value to a target type."),
        c!("INCLUDE PERFETTO MODULE", Keyword, "Pull in a PerfettoSQL stdlib module (e.g. android.startup)."),

        // Aggregates
        c!("COUNT", Function, "Aggregate: row count. COUNT(*) or COUNT(expr)."),
        c!("SUM", Function, "Aggregate: numeric sum of a column."),
        c!("AVG", Function, "Aggregate: arithmetic mean of a column."),
        c!("MIN", Function, "Aggregate: smallest value in a column."),
        c!("MAX", Function, "Aggregate: largest value in a column."),
        c!("GROUP_CONCAT", Function, "Aggregate: string-join column values."),
        c!("PERCENTILE", Function, "PerfettoSQL aggregate: nth percentile of a column."),

        // Core PerfettoSQL tables — always available, no module include needed.
        c!("slice", Table, "Trace slice events. Columns: ts, dur, name, category, track_id…"),
        c!("thread_slice", Table, "slice rows joined to their thread."),
        c!("process", Table, "One row per process. Columns: upid, pid, name, start_ts…"),
        c!("thread", Table, "One row per thread. Columns: utid, tid, upid, name, start_ts…"),
        c!("thread_state", Table, "Per-thread scheduling state intervals (Running/Runnable/Sleep…)."),
        c!("counter", Table, "Counter samples. Columns: ts, value, track_id."),
        c!("counter_track", Table, "Counter track metadata. Columns: id, name, unit…"),
        c!("process_counter_track", Table, "Counter tracks scoped to a process."),
        c!("thread_counter_track", Table, "Counter tracks scoped to a thread."),
        c!("sched_slice", Table, "CPU scheduling slices. Columns: ts, dur, cpu, utid, end_state."),
        c!("cpu", Table, "Per-CPU metadata."),
        c!("actual_frame_timeline_slice", Table, "Per-frame actual timeline (upid, ts, dur, on_time_finish, jank_type…)."),
        c!("expected_frame_timeline_slice", Table, "Per-frame expected timeline (upid, ts, dur)."),
        c!("android_logs", Table, "logcat entries (ts, prio, tag, msg)."),
        c!("ftrace_event", Table, "Raw ftrace events. Wide; filter by name."),
    ]
}

/// Convert a schema snapshot's raw table names into completion entries.
/// Skips names that already appear in `existing` (case-insensitive) so
/// curated entries with their richer docs win over the raw schema name.
pub fn schema_candidates(tables: &[String], existing: &[Candidate]) -> Vec<Candidate> {
    let known: std::collections::HashSet<String> = existing
        .iter()
        .map(|c| c.label.to_ascii_lowercase())
        .collect();
    tables
        .iter()
        .filter(|t| !known.contains(&t.to_ascii_lowercase()))
        .map(|t| Candidate {
            label: Cow::Owned(t.clone()),
            kind: CandidateKind::SchemaTable,
            doc: Cow::Borrowed("PerfettoSQL table discovered in the loaded trace."),
            origin: None,
        })
        .collect()
}

/// Turn a `table -> [columns]` map into completion candidates. Each
/// column carries its owning table name via `origin` so `ScopeHint`
/// filtering can drop columns out of scope. Duplicate column names
/// (e.g. `ts` appears on `slice`, `counter`, `sched_slice`…) are
/// preserved as distinct entries per table — users typing `slice.ts`
/// should only see the one that belongs to `slice`.
pub fn column_candidates(
    by_table: &std::collections::HashMap<String, Vec<String>>,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (table, cols) in by_table {
        for col in cols {
            out.push(Candidate {
                label: Cow::Owned(col.clone()),
                kind: CandidateKind::Column,
                doc: Cow::Owned(format!("Column on `{}`.", table)),
                origin: Some(Cow::Owned(table.clone())),
            });
        }
    }
    out
}

// ── scope parsing ───────────────────────────────────────────────────────

/// Summary of the table context the cursor is in. Drives scope-aware
/// column completion: columns filter to `tables` (or `dotted` when set).
#[derive(Debug, Default, Clone)]
pub struct ScopeHint {
    /// Tables in the nearest enclosing FROM/JOIN clause (case-preserving,
    /// as written by the user). Empty when the cursor isn't in a
    /// recognised FROM-scope — column completion then shows nothing to
    /// avoid drowning the popup in every column in the trace.
    pub tables: Vec<String>,
    /// Alias→table map so a dotted prefix like `s.foo` resolves to
    /// columns on `slice`. If the user skipped the alias, this maps
    /// the table name to itself.
    pub aliases: std::collections::HashMap<String, String>,
    /// If the word-prefix is preceded by `<ident>.`, this records the
    /// identifier. Typically an alias into `aliases`; may be a bare
    /// table name.
    pub dotted: Option<String>,
}

impl ScopeHint {
    /// Resolve a dotted qualifier to the underlying table name using
    /// the alias map, falling back to treating it as a table name
    /// directly. Returns `None` only when the qualifier matches
    /// nothing in-scope.
    pub fn resolve_dotted<'a>(&'a self, dotted: &'a str) -> Option<&'a str> {
        let lower = dotted.to_ascii_lowercase();
        for (alias, table) in &self.aliases {
            if alias.eq_ignore_ascii_case(&lower) {
                return Some(table.as_str());
            }
        }
        // Fall back: accept a bare table name if the scope lists it.
        self.tables
            .iter()
            .find(|t| t.eq_ignore_ascii_case(&lower))
            .map(|t| t.as_str())
    }
}

/// Derive a [`ScopeHint`] from the current editor content. Uses a
/// simple linear scan rather than a SQL parser — we match `FROM` and
/// `JOIN` keywords case-insensitively and collect `table [AS] alias`
/// triples until the next clause-boundary keyword. Sub-queries and
/// CTEs are not decomposed; we take the nearest FROM before the
/// cursor as the effective scope.
pub fn parse_scope(editor: &TextArea<'static>) -> ScopeHint {
    let cursor = editor.cursor();
    let (cur_row, cur_col) = (cursor.0, cursor.1);

    // Flatten buffer up to the cursor, preserving newlines as spaces.
    // We only care about the text BEFORE the cursor for scope — the
    // FROM that follows (if any) hasn't been written yet.
    let mut buf = String::new();
    for (i, line) in editor.lines().iter().enumerate() {
        match i.cmp(&cur_row) {
            std::cmp::Ordering::Less => {
                buf.push_str(line);
                buf.push(' ');
            }
            std::cmp::Ordering::Equal => {
                let chars: Vec<char> = line.chars().collect();
                let take = cur_col.min(chars.len());
                buf.extend(chars.iter().take(take));
            }
            std::cmp::Ordering::Greater => break,
        }
    }

    let dotted = dotted_qualifier(editor);
    let (tables, aliases) = extract_tables_and_aliases(&buf);
    ScopeHint { tables, aliases, dotted }
}

/// Detect `ident.` immediately before the cursor. Returns `Some(ident)`
/// if the character at `cursor - (prefix_chars + 1)` is `.` and there's
/// an identifier before it.
fn dotted_qualifier(editor: &TextArea<'static>) -> Option<String> {
    let (prefix, row, start_col) = word_prefix_at_cursor(editor)?;
    let line = editor.lines().get(row)?;
    let chars: Vec<char> = line.chars().collect();
    if start_col == 0 {
        return None;
    }
    // The char immediately preceding the prefix must be `.`.
    if chars.get(start_col - 1).copied() != Some('.') {
        return None;
    }
    // Walk further back to collect the qualifier identifier.
    let q_end = start_col - 1;
    let mut q_start = q_end;
    while q_start > 0 && is_word_char(chars[q_start - 1]) {
        q_start -= 1;
    }
    if q_start == q_end {
        return None;
    }
    // (prefix kept for symmetry — unused here)
    let _ = prefix;
    Some(chars[q_start..q_end].iter().collect())
}

/// Walk `buf` left-to-right tracking FROM/JOIN clauses. Records the
/// *nearest* completed FROM-scope (or the scope currently open at EOF
/// if the user is mid-clause). Tolerates JOIN modifiers (LEFT/INNER/
/// OUTER/…) and skips over ON-predicate bodies so identifiers inside
/// them aren't mis-parsed as tables.
fn extract_tables_and_aliases(
    buf: &str,
) -> (Vec<String>, std::collections::HashMap<String, String>) {
    let tokens = tokenize(buf);

    let mut last_tables: Vec<String> = Vec::new();
    let mut last_aliases: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut cur_tables: Vec<String> = Vec::new();
    let mut cur_aliases: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut in_from = false;

    let commit = |in_from: &mut bool,
                  cur_tables: &mut Vec<String>,
                  cur_aliases: &mut std::collections::HashMap<String, String>,
                  last_tables: &mut Vec<String>,
                  last_aliases: &mut std::collections::HashMap<String, String>| {
        if *in_from {
            *last_tables = std::mem::take(cur_tables);
            *last_aliases = std::mem::take(cur_aliases);
            *in_from = false;
        }
    };

    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        let upper = t.to_ascii_uppercase();
        match upper.as_str() {
            "FROM" => {
                commit(
                    &mut in_from,
                    &mut cur_tables,
                    &mut cur_aliases,
                    &mut last_tables,
                    &mut last_aliases,
                );
                in_from = true;
                i += 1;
            }
            "JOIN" => {
                // Extends the current scope. If we saw no open FROM,
                // treat JOIN as starting one (e.g. in nested queries
                // we failed to parse).
                if !in_from {
                    in_from = true;
                }
                i += 1;
            }
            "LEFT" | "RIGHT" | "INNER" | "OUTER" | "FULL" | "CROSS" => {
                // Join modifier; ignore.
                i += 1;
            }
            "ON" => {
                // Skip the predicate body until we hit another JOIN, a
                // comma, or a clause boundary. Identifiers inside the
                // predicate are NOT tables.
                i += 1;
                while i < tokens.len() {
                    let tk = tokens[i].to_ascii_uppercase();
                    if tk == "JOIN" || tk == "," || is_clause_boundary(&tokens[i]) {
                        break;
                    }
                    i += 1;
                }
            }
            _ if is_clause_boundary(t) => {
                commit(
                    &mut in_from,
                    &mut cur_tables,
                    &mut cur_aliases,
                    &mut last_tables,
                    &mut last_aliases,
                );
                i += 1;
            }
            "," => {
                i += 1;
            }
            _ if in_from && is_ident(t) => {
                let table = t.clone();
                i += 1;
                let mut alias = table.clone();
                if i < tokens.len() && tokens[i].eq_ignore_ascii_case("AS") {
                    i += 1;
                    if i < tokens.len() && is_ident(&tokens[i]) {
                        alias = tokens[i].clone();
                        i += 1;
                    }
                } else if i < tokens.len() && is_ident(&tokens[i]) {
                    alias = tokens[i].clone();
                    i += 1;
                }
                cur_tables.push(table.clone());
                cur_aliases.insert(alias, table);
            }
            _ => {
                i += 1;
            }
        }
    }

    // If the user is mid-FROM at EOF (no clause-boundary terminator),
    // surface the open scope so completions work while typing.
    if in_from {
        last_tables = cur_tables;
        last_aliases = cur_aliases;
    }

    (last_tables, last_aliases)
}

fn tokenize(buf: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = buf.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == ',' {
            out.push(",".into());
            chars.next();
        } else if c == '\'' || c == '"' {
            // Skip string / quoted identifier literally.
            let quote = c;
            chars.next();
            while let Some(&nc) = chars.peek() {
                chars.next();
                if nc == quote {
                    break;
                }
            }
        } else if is_word_char(c) {
            let mut s = String::new();
            while let Some(&nc) = chars.peek() {
                if is_word_char(nc) {
                    s.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(s);
        } else {
            // Skip any other punctuation (parens, dots, operators).
            chars.next();
        }
    }
    out
}

fn is_clause_boundary(token: &str) -> bool {
    matches!(
        token.to_ascii_uppercase().as_str(),
        "WHERE"
            | "GROUP"
            | "ORDER"
            | "HAVING"
            | "LIMIT"
            | "OFFSET"
            | "UNION"
            | "INTERSECT"
            | "EXCEPT"
            | "SELECT"
            | ";"
    )
}

fn is_ident(token: &str) -> bool {
    token.chars().next().map_or(false, |c| c.is_ascii_alphabetic() || c == '_')
        && token.chars().all(is_word_char)
        && !is_sql_reserved(token)
}

fn is_sql_reserved(token: &str) -> bool {
    let upper = token.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "FROM"
            | "JOIN"
            | "LEFT"
            | "RIGHT"
            | "INNER"
            | "OUTER"
            | "FULL"
            | "CROSS"
            | "ON"
            | "AS"
            | "USING"
            | "WHERE"
            | "GROUP"
            | "ORDER"
            | "HAVING"
            | "LIMIT"
            | "OFFSET"
            | "UNION"
            | "SELECT"
            | "WITH"
    )
}

// ── state ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CompletionState {
    prefix: String,
    /// Data-cursor coordinates where the prefix starts. Used to replace
    /// the prefix on accept (delete N chars back from cursor).
    trigger_row: usize,
    trigger_col: usize,
    /// Indices into the `candidates` slice passed to `open` / `refresh`.
    /// The caller owns the slice and must pass the same slice to
    /// subsequent calls on this state — rebuilding the slice
    /// (e.g. on schema load) invalidates the indices, so
    /// `ReplState::set_schema` closes any open popup.
    filtered: Vec<usize>,
    selected: usize,
    /// Cached copies of the visible entries so `selected()` / `accept()`
    /// don't need the original slice re-passed. Cheap: we keep at most
    /// a few dozen entries at a time, and `Candidate` clones are shallow
    /// (Cow + enum).
    visible: Vec<Candidate>,
}

impl CompletionState {
    /// Open the popup anchored at the current cursor position. An
    /// empty word-prefix is allowed — the popup then lists every
    /// in-scope candidate in curated pool order so users can browse
    /// what's available. Returns `None` only when the cursor isn't at
    /// a word boundary (e.g. out-of-line) or no candidates match at
    /// all.
    ///
    /// `scope` drives column filtering — see [`ScopeHint`]. Pass a
    /// default-constructed hint if the caller doesn't have one; columns
    /// will then be hidden but keywords/tables still appear.
    pub fn open(
        editor: &TextArea<'static>,
        candidates: &[Candidate],
        scope: &ScopeHint,
    ) -> Option<Self> {
        let (prefix, row, col) = word_prefix_at_cursor(editor)?;
        let filtered = filter(&prefix, candidates, scope);
        if filtered.is_empty() {
            return None;
        }
        let visible = filtered.iter().map(|&i| candidates[i].clone()).collect();
        Some(Self {
            prefix,
            trigger_row: row,
            trigger_col: col,
            filtered,
            selected: 0,
            visible,
        })
    }

    /// Re-scan the cursor's word prefix and refilter. Returns `false` if
    /// the popup should close: cursor drifted off the word-start, or
    /// filtering produced no matches. An empty prefix is kept open —
    /// the popup keeps browsing the full set while the user deletes
    /// back.
    pub fn refresh(
        &mut self,
        editor: &TextArea<'static>,
        candidates: &[Candidate],
        scope: &ScopeHint,
    ) -> bool {
        let Some((prefix, row, col)) = word_prefix_at_cursor(editor) else {
            return false;
        };
        if row != self.trigger_row || col != self.trigger_col {
            return false;
        }
        let filtered = filter(&prefix, candidates, scope);
        if filtered.is_empty() {
            return false;
        }
        let visible: Vec<Candidate> = filtered.iter().map(|&i| candidates[i].clone()).collect();
        self.prefix = prefix;
        self.filtered = filtered;
        self.visible = visible;
        if self.selected >= self.visible.len() {
            self.selected = 0;
        }
        true
    }

    pub fn move_up(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.visible.len() - 1
        } else {
            self.selected - 1
        };
    }

    pub fn move_down(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.visible.len();
    }

    pub fn selected(&self) -> Option<&Candidate> {
        self.visible.get(self.selected)
    }

    /// Replace the typed prefix with the selected candidate's label.
    /// Caller is expected to drop `CompletionState` afterwards.
    pub fn accept(&self, editor: &mut TextArea<'static>) {
        let Some(candidate) = self.selected() else {
            return;
        };
        let prefix_chars = self.prefix.chars().count();
        for _ in 0..prefix_chars {
            editor.move_cursor(CursorMove::Back);
        }
        editor.delete_str(prefix_chars);
        editor.insert_str(candidate.label.as_ref());
    }

    #[cfg(test)]
    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    #[cfg(test)]
    pub fn selected_index(&self) -> usize {
        self.selected
    }
}

/// Apply prefix match + scope-aware filtering. Column rules:
///
/// * Dotted prefix (`alias.foo`) — only columns whose `origin` resolves
///   through `scope.dotted` are offered.
/// * Undotted prefix, scope has tables — columns whose `origin` is one
///   of those tables are offered.
/// * Undotted prefix, no scope — columns are suppressed (hundreds of
///   `ts`/`dur`/`name` entries would drown the popup).
///
/// Non-column entries (keywords, tables, functions) are offered purely
/// on prefix match.
fn filter(prefix: &str, candidates: &[Candidate], scope: &ScopeHint) -> Vec<usize> {
    let needle = prefix.to_ascii_lowercase();
    // Resolve dotted qualifier once for the whole filter pass.
    let dotted_target: Option<String> = scope
        .dotted
        .as_deref()
        .and_then(|q| scope.resolve_dotted(q))
        .map(|s| s.to_ascii_lowercase());
    let scope_lower: std::collections::HashSet<String> = scope
        .tables
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect();

    candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if !c.label.to_ascii_lowercase().starts_with(&needle) {
                return None;
            }
            if c.kind == CandidateKind::Column {
                let origin = c.origin.as_deref()?.to_ascii_lowercase();
                match (&dotted_target, scope_lower.is_empty()) {
                    (Some(target), _) => {
                        if &origin != target {
                            return None;
                        }
                    }
                    (None, true) => return None,
                    (None, false) => {
                        if !scope_lower.contains(&origin) {
                            return None;
                        }
                    }
                }
            } else if dotted_target.is_some() {
                // Dotted prefix implies the user wants a column —
                // suppress non-column matches.
                return None;
            }
            Some(i)
        })
        .collect()
}

/// Walk back from the cursor while the preceding char is a word char
/// (`[A-Za-z0-9_]`). Returns `(prefix, row, start_col)` where `start_col`
/// points at the first char of the prefix.
fn word_prefix_at_cursor(editor: &TextArea<'static>) -> Option<(String, usize, usize)> {
    let cursor = editor.cursor();
    let (row, col) = (cursor.0, cursor.1);
    let line = editor.lines().get(row)?;
    let chars: Vec<char> = line.chars().collect();
    if col > chars.len() {
        return None;
    }
    // Scan back from `col - 1` while word-char.
    let mut start = col;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }
    let prefix: String = chars[start..col].iter().collect();
    Some((prefix, row, start))
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

// ── rendering ───────────────────────────────────────────────────────────

/// Max popup height in rows (excluding borders).
const POPUP_MAX_ROWS: u16 = 8;
/// Max popup width in columns (including borders). The doc line sits in
/// the bottom title so the width has to accommodate a short sentence.
const POPUP_MAX_WIDTH: u16 = 72;

/// Render the completion popup, anchored below the editor cursor.
///
/// `editor_area` is the full editor Rect (including borders); we inset
/// by 1 on each side to get the textarea's inner area. `frame_area` is
/// used to clamp the popup within the terminal.
pub fn render_popup(
    frame: &mut Frame,
    editor_area: Rect,
    frame_area: Rect,
    editor: &TextArea<'static>,
    state: &CompletionState,
) {
    let inner = inset(editor_area, 1);
    let cursor = editor.cursor();
    let cursor_row = cursor.0;

    // Assume no vertical scroll; clamp if cursor would render below inner.
    let cursor_screen_y =
        inner.y.saturating_add(u16::try_from(cursor_row).unwrap_or(0))
            .min(inner.y.saturating_add(inner.height).saturating_sub(1));
    let cursor_screen_x =
        inner.x.saturating_add(u16::try_from(state.trigger_col).unwrap_or(0))
            .min(inner.x.saturating_add(inner.width).saturating_sub(1));

    let rows = u16::try_from(state.visible.len())
        .unwrap_or(POPUP_MAX_ROWS)
        .min(POPUP_MAX_ROWS);
    let width = desired_width(state).min(POPUP_MAX_WIDTH);
    let height = rows.saturating_add(2); // borders

    // Prefer rendering below the cursor. Fall back to above if we'd clip
    // the bottom edge of the frame.
    let below_y = cursor_screen_y.saturating_add(1);
    let y = if below_y.saturating_add(height) <= frame_area.y + frame_area.height {
        below_y
    } else {
        cursor_screen_y.saturating_sub(height)
    };

    // Keep the popup fully inside the frame horizontally.
    let max_x = frame_area.x + frame_area.width.saturating_sub(width);
    let x = cursor_screen_x.min(max_x);

    let rect = Rect { x, y, width, height };

    // Top title: count. Bottom title: doc for the currently-selected
    // candidate, truncated to fit. Tab/Enter/Esc conventions are left
    // to muscle memory + the main-screen footer.
    let doc_line = state
        .selected()
        .map(|c| {
            let budget = width.saturating_sub(4) as usize;
            Line::from(Span::styled(
                format!(" {} ", truncate_chars(c.doc.as_ref(), budget)),
                Style::default().fg(theme::dim()),
            ))
        })
        .unwrap_or_else(|| Line::from(""));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::accent_secondary()))
        .title(Span::styled(
            format!(" completions · {} · ", state.visible.len()),
            theme::title(),
        ))
        .title_bottom(doc_line);

    let items: Vec<ListItem> = state
        .visible
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let selected = i == state.selected;
            let marker = if selected { "▶ " } else { "  " };
            let marker_style = if selected {
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::dim())
            };
            let label_style = if selected {
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(c.label.to_string(), label_style),
                Span::raw("  "),
                Span::styled(c.kind.label(), Style::default().fg(theme::dim())),
            ]))
        })
        .collect();

    // Clear the underlying text so the popup background is clean. ratatui's
    // Clear widget blanks the cells first.
    frame.render_widget(ratatui::widgets::Clear, rect);
    frame.render_widget(List::new(items).block(block), rect);
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
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

fn desired_width(state: &CompletionState) -> u16 {
    // The list rows need: marker (2) + label + gap (2) + kind (7) + borders (2).
    // The doc line in the bottom title also wants breathing room — bias
    // wider by default so short keyword lists don't produce a cramped
    // popup where the doc truncates after 3 words.
    let max_label = state
        .visible
        .iter()
        .map(|c| c.label.chars().count())
        .max()
        .unwrap_or(0);
    let content = max_label + 2 + 7 + 4;
    let list_width = u16::try_from(content).unwrap_or(POPUP_MAX_WIDTH);
    list_width.max(POPUP_MAX_WIDTH)
}

fn inset(r: Rect, by: u16) -> Rect {
    Rect {
        x: r.x.saturating_add(by),
        y: r.y.saturating_add(by),
        width: r.width.saturating_sub(by.saturating_mul(2)),
        height: r.height.saturating_sub(by.saturating_mul(2)),
    }
}

// ── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_textarea::TextArea;

    fn editor_with_cursor_after(text: &str) -> TextArea<'static> {
        let lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
        let lines = if lines.is_empty() {
            vec![text.to_string()]
        } else {
            lines
        };
        let mut ta = TextArea::new(lines);
        ta.move_cursor(CursorMove::Bottom);
        ta.move_cursor(CursorMove::End);
        ta
    }

    #[test]
    fn prefix_extraction_returns_word_before_cursor() {
        let ta = editor_with_cursor_after("SELECT * FROM sli");
        let (prefix, row, start_col) = word_prefix_at_cursor(&ta).unwrap();
        assert_eq!(prefix, "sli");
        assert_eq!(row, 0);
        assert_eq!(start_col, "SELECT * FROM ".chars().count());
    }

    #[test]
    fn prefix_extraction_empty_on_whitespace() {
        let ta = editor_with_cursor_after("SELECT * FROM ");
        let (prefix, _, _) = word_prefix_at_cursor(&ta).unwrap();
        assert!(prefix.is_empty());
    }

    fn scope() -> ScopeHint {
        ScopeHint::default()
    }

    #[test]
    fn open_empty_prefix_shows_all_in_pool_order() {
        let ta = editor_with_cursor_after("SELECT * FROM ");
        let cs = static_candidates();
        let s = CompletionState::open(&ta, &cs, &scope()).expect("open on empty prefix");
        // First visible candidate matches the pool's first entry (curated
        // order is preserved — no alphabetical re-sort).
        assert_eq!(s.visible[0].label, cs[0].label);
        // And the full non-column portion of the pool is surfaced.
        assert_eq!(s.visible_len(), cs.len());
    }

    #[test]
    fn open_filters_by_prefix_case_insensitive() {
        let ta = editor_with_cursor_after("sel");
        let cs = static_candidates();
        let s = CompletionState::open(&ta, &cs, &scope()).expect("should open with 'sel'");
        assert!(s.visible_len() >= 1);
        assert!(
            s.visible
                .iter()
                .any(|c| c.label.eq_ignore_ascii_case("SELECT")),
        );
    }

    #[test]
    fn accept_replaces_prefix_with_label() {
        let mut ta = editor_with_cursor_after("SELECT * FROM sli");
        let cs = static_candidates();
        let mut s = CompletionState::open(&ta, &cs, &scope()).expect("open");
        let slice_in_visible = s
            .visible
            .iter()
            .position(|c| c.label == "slice")
            .expect("slice in filter");
        s.selected = slice_in_visible;
        s.accept(&mut ta);
        assert_eq!(ta.lines(), vec!["SELECT * FROM slice"]);
    }

    #[test]
    fn move_up_down_wraps() {
        let ta = editor_with_cursor_after("SELEC");
        let cs = static_candidates();
        let mut s = CompletionState::open(&ta, &cs, &scope()).expect("open");
        let n = s.visible_len();
        assert!(n >= 1);
        s.move_up();
        assert_eq!(s.selected_index(), n - 1);
        s.move_down();
        assert_eq!(s.selected_index(), 0);
        s.move_down();
        assert_eq!(s.selected_index(), 1 % n);
    }

    #[test]
    fn refresh_keeps_popup_open_when_prefix_cleared() {
        // Opening with "sli" then deleting back to empty should keep
        // the popup open (browsing mode), provided the cursor stays at
        // the original word-start position. Closing is the caller's
        // choice via Esc or a refiltered no-match.
        let ta = editor_with_cursor_after("sli");
        let cs = static_candidates();
        let mut s = CompletionState::open(&ta, &cs, &scope()).expect("open");
        // Same cursor column, empty buffer.
        let ta_empty = editor_with_cursor_after("");
        assert!(s.refresh(&ta_empty, &cs, &scope()));
        assert_eq!(s.visible_len(), cs.len());
    }

    #[test]
    fn refresh_closes_when_cursor_drifts_off_word_start() {
        // Typing a word-boundary char (e.g. space) advances the cursor
        // past the trigger position; refresh should then close.
        let ta = editor_with_cursor_after("sli");
        let cs = static_candidates();
        let mut s = CompletionState::open(&ta, &cs, &scope()).expect("open");
        // Simulate the user typing a space after "sli": cursor now at
        // col 4, but the word-start for any new prefix is also col 4
        // (mismatches the trigger at col 0).
        let ta_moved = editor_with_cursor_after("sli ");
        assert!(!s.refresh(&ta_moved, &cs, &scope()));
    }

    #[test]
    fn candidates_list_is_nonempty_and_sorted_deterministically() {
        let cs = static_candidates();
        assert!(!cs.is_empty());
        assert!(cs.iter().any(|c| c.label == "SELECT"));
        assert!(cs.iter().any(|c| c.label == "slice"));
    }

    #[test]
    fn schema_candidates_dedupes_against_static_set_case_insensitive() {
        let cs = static_candidates();
        let discovered = vec![
            "slice".into(),
            "Slice".into(),
            "android_startups".into(),
        ];
        let extra = schema_candidates(&discovered, &cs);
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].label, "android_startups");
        assert_eq!(extra[0].kind, CandidateKind::SchemaTable);
    }

    #[test]
    fn open_uses_schema_tables_when_static_does_not_match() {
        let mut merged = static_candidates();
        merged.extend(schema_candidates(
            &["android_startups".into()],
            &merged.clone(),
        ));
        let ta = editor_with_cursor_after("SELECT * FROM android_s");
        let s = CompletionState::open(&ta, &merged, &scope()).expect("open");
        assert!(
            s.visible.iter().any(|c| c.label == "android_startups"),
            "schema table must be offered when typing matching prefix",
        );
    }

    // ── scope parser ────────────────────────────────────────────────────

    #[test]
    fn parse_scope_empty_outside_from() {
        let ta = editor_with_cursor_after("SELECT ");
        let s = parse_scope(&ta);
        assert!(s.tables.is_empty());
        assert!(s.dotted.is_none());
    }

    #[test]
    fn parse_scope_collects_single_from_table() {
        let ta = editor_with_cursor_after("SELECT * FROM slice WHERE ");
        let s = parse_scope(&ta);
        assert_eq!(s.tables, vec!["slice"]);
        assert_eq!(s.aliases.get("slice"), Some(&"slice".to_string()));
    }

    #[test]
    fn parse_scope_handles_alias_without_as() {
        let ta = editor_with_cursor_after("SELECT * FROM slice s WHERE ");
        let s = parse_scope(&ta);
        assert_eq!(s.tables, vec!["slice"]);
        assert_eq!(s.aliases.get("s"), Some(&"slice".to_string()));
    }

    #[test]
    fn parse_scope_handles_as_alias() {
        let ta = editor_with_cursor_after("SELECT * FROM slice AS s WHERE ");
        let s = parse_scope(&ta);
        assert_eq!(s.aliases.get("s"), Some(&"slice".to_string()));
    }

    #[test]
    fn parse_scope_handles_join() {
        let ta = editor_with_cursor_after(
            "SELECT * FROM slice s LEFT JOIN thread t ON s.utid = t.utid WHERE ",
        );
        let s = parse_scope(&ta);
        assert_eq!(s.tables.len(), 2);
        assert_eq!(s.aliases.get("s"), Some(&"slice".to_string()));
        assert_eq!(s.aliases.get("t"), Some(&"thread".to_string()));
    }

    #[test]
    fn parse_scope_detects_dotted_prefix() {
        let ta = editor_with_cursor_after("SELECT * FROM slice s WHERE s.n");
        let s = parse_scope(&ta);
        assert_eq!(s.dotted.as_deref(), Some("s"));
    }

    // ── scope-aware filter ──────────────────────────────────────────────

    fn build_pool_with_columns() -> Vec<Candidate> {
        let mut pool = static_candidates();
        let mut by_table = std::collections::HashMap::new();
        by_table.insert(
            "slice".to_string(),
            vec!["ts".into(), "dur".into(), "name".into()],
        );
        by_table.insert(
            "thread".to_string(),
            vec!["utid".into(), "name".into()],
        );
        pool.extend(column_candidates(&by_table));
        pool
    }

    #[test]
    fn columns_hidden_when_no_scope_detected() {
        let pool = build_pool_with_columns();
        // Prefix "n" matches the keyword NOT *and* the slice.name column;
        // with no FROM-scope, only NOT should surface.
        let ta = editor_with_cursor_after("SELECT n");
        let s = CompletionState::open(&ta, &pool, &scope()).expect("open");
        assert!(
            !s.visible.iter().any(|c| c.kind == CandidateKind::Column),
            "columns must be suppressed with no FROM-scope; got {:?}",
            s.visible.iter().map(|c| c.label.as_ref()).collect::<Vec<_>>(),
        );
        assert!(
            s.visible.iter().any(|c| c.label.eq_ignore_ascii_case("NOT")),
            "keyword matches must still appear",
        );
    }

    #[test]
    fn columns_visible_when_from_scope_matches() {
        let pool = build_pool_with_columns();
        let ta = editor_with_cursor_after("SELECT * FROM slice WHERE na");
        let sc = parse_scope(&ta);
        let s = CompletionState::open(&ta, &pool, &sc).expect("open");
        assert!(
            s.visible.iter().any(|c| c.kind == CandidateKind::Column
                && c.label == "name"
                && c.origin.as_deref() == Some("slice")),
            "slice.name must be offered under 'na' prefix + FROM slice",
        );
    }

    #[test]
    fn dotted_prefix_restricts_to_alias_table() {
        let pool = build_pool_with_columns();
        let ta = editor_with_cursor_after("SELECT * FROM slice s, thread t WHERE t.u");
        let sc = parse_scope(&ta);
        let s = CompletionState::open(&ta, &pool, &sc).expect("open");
        // Only thread.utid should be offered, not slice columns.
        let kinds: Vec<_> = s.visible.iter().map(|c| c.kind).collect();
        assert!(
            kinds.iter().all(|k| *k == CandidateKind::Column),
            "dotted prefix suppresses non-column matches",
        );
        assert!(
            s.visible
                .iter()
                .all(|c| c.origin.as_deref() == Some("thread")),
            "only columns of thread (alias `t`) should appear — got {:?}",
            s.visible.iter().map(|c| c.label.as_ref()).collect::<Vec<_>>(),
        );
    }
}
