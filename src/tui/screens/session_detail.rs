use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::cloud::{self, UploadProgress};
use crate::db::Database;
use crate::db::traces::TraceRecord;
use crate::import::Benchmark;
use crate::import::benchmark_json;
use crate::perfetto::capture::Cancel;
use crate::perfetto::textproto;
use crate::session::Session;
use crate::tui::chrome;
use crate::tui::text_input::{self, TextAction};
use crate::tui::theme;

/// Minimum total terminal width at which we split the session detail into a
/// two-pane layout (session + traces on the left, textproto preview on the
/// right). Below this we fall back to the original single-column layout so
/// narrow terminals still look sensible.
const TWO_PANE_WIDTH: u16 = 120;

const TRACE_EXT: &str = ".pftrace";

pub struct SessionDetailScreen {
    session: Session,
    traces: Vec<TraceRecord>,
    /// Indices into `traces` that survive the current tag filter. Selection
    /// indexes into this, not `traces`, so filtering never points at a hidden
    /// row.
    visible: Vec<usize>,
    list_state: ListState,
    error: Option<String>,
    status: theme::Status,
    mode: Mode,
    tag_filter: Option<String>,
    preview_scroll: u16,
    /// Cancel handle for an in-flight upload (set by App).
    upload_cancel: Option<Arc<Cancel>>,
    /// ID of the default cloud provider.
    cloud_provider_id: String,
    /// Name of the active cloud provider (e.g. "Google Drive", "Amazon S3").
    cloud_provider_name: String,
    /// Parsed macrobenchmark metrics for imported sessions. Populated once in
    /// `new` from `session.benchmark_json_path`; `None` for non-imported
    /// sessions or when parsing failed (benchmark_load_error carries the
    /// reason).
    benchmarks: Option<Vec<Benchmark>>,
    benchmark_load_error: Option<String>,
}

enum Mode {
    Browse,
    Rename { buffer: String },
    EditTags { buffer: String },
    /// Prompt for a trace filename before launching the capture. Submit
    /// dispatches `DetailAction::Capture(Some(name))`; an empty name on
    /// submit falls back to the default timestamp filename.
    PromptCaptureName { buffer: String },
    ConfirmDelete,
    UploadPickProvider {
        scope: UploadScope,
        providers: Vec<(String, String)>, // (id, name)
        selected: usize,
    },
    UploadConfirm {
        scope: UploadScope,
        provider_id: String,
        provider_name: String,
    },
    Uploading { progress: Option<UploadProgress> },
    SharePickProvider {
        entries: Vec<(String, String)>, // (provider_name, url)
        selected: usize,
    },
}

/// What to upload — a single trace or all traces in the session.
#[derive(Debug, Clone)]
pub enum UploadScope {
    SingleTrace(i64),
    AllTraces,
}

pub enum DetailAction {
    None,
    Back,
    EditConfig,
    /// Start a capture. `Some(name)` uses the user-supplied filename stem
    /// (no extension); `None` falls back to the default timestamped filename.
    Capture(Option<String>),
    OpenTrace(PathBuf),
    OpenFolder(PathBuf),
    Upload(UploadScope, String), // (scope, provider_id)
    Analyze(PathBuf),            // opens Analysis screen for the given trace
}

impl SessionDetailScreen {
    pub fn new(session: Session, db: &Database, cloud_provider_id: &str, cloud_provider_name: &str) -> Self {
        let (benchmarks, benchmark_load_error) = load_benchmarks(&session);
        let mut screen = Self {
            session,
            traces: Vec::new(),
            visible: Vec::new(),
            list_state: ListState::default(),
            error: None,
            status: theme::Status::default(),
            mode: Mode::Browse,
            tag_filter: None,
            preview_scroll: 0,
            upload_cancel: None,
            cloud_provider_id: cloud_provider_id.to_string(),
            cloud_provider_name: cloud_provider_name.to_string(),
            benchmarks,
            benchmark_load_error,
        };
        screen.reload(db);
        screen
    }

    pub fn reload(&mut self, db: &Database) {
        let Some(id) = self.session.id else {
            return;
        };
        match db.list_traces(id) {
            Ok(traces) => {
                self.traces = traces;
                self.error = None;
                self.apply_filter();
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn apply_filter(&mut self) {
        self.visible = self
            .traces
            .iter()
            .enumerate()
            .filter(|(_, t)| match &self.tag_filter {
                Some(tag) => t.tags.iter().any(|x| x == tag),
                None => true,
            })
            .map(|(i, _)| i)
            .collect();

        if self.visible.is_empty() {
            self.list_state.select(None);
            return;
        }
        let clamped = self
            .list_state
            .selected()
            .map(|i| i.min(self.visible.len() - 1))
            .unwrap_or(0);
        self.list_state.select(Some(clamped));
    }

    fn all_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self
            .traces
            .iter()
            .flat_map(|t| t.tags.iter().cloned())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }

    fn cycle_filter(&mut self) {
        let tags = self.all_tags();
        self.tag_filter = match &self.tag_filter {
            None => tags.first().cloned(),
            Some(current) => {
                let idx = tags.iter().position(|t| t == current);
                match idx {
                    Some(i) if i + 1 < tags.len() => Some(tags[i + 1].clone()),
                    _ => None,
                }
            }
        };
        self.apply_filter();
    }

    pub fn on_key(&mut self, db: &Database, key: KeyEvent) -> DetailAction {
        if key.kind != KeyEventKind::Press {
            return DetailAction::None;
        }
        match &mut self.mode {
            Mode::Rename { .. } => {
                return self.handle_rename_key(db, key);
            }
            Mode::EditTags { .. } => {
                return self.handle_tags_key(db, key);
            }
            Mode::PromptCaptureName { .. } => {
                return self.handle_capture_name_key(key);
            }
            Mode::ConfirmDelete => {
                return self.handle_confirm_delete(db, key);
            }
            Mode::UploadPickProvider { .. } => {
                return self.handle_upload_pick_provider(key);
            }
            Mode::UploadConfirm { .. } => {
                return self.handle_upload_confirm(key);
            }
            Mode::Uploading { .. } => {
                return self.handle_uploading_key(key);
            }
            Mode::SharePickProvider { .. } => {
                return self.handle_share_pick_provider(key);
            }
            Mode::Browse => {}
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => DetailAction::Back,
            KeyCode::Char('e') => DetailAction::EditConfig,
            KeyCode::Char('c') => DetailAction::Capture(None),
            KeyCode::Char('C') => {
                self.mode = Mode::PromptCaptureName {
                    buffer: String::new(),
                };
                DetailAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                DetailAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                DetailAction::None
            }
            KeyCode::Char('r') => {
                if let Some(t) = self.selected_trace() {
                    let seed = t
                        .label
                        .clone()
                        .unwrap_or_else(|| file_name(&t.file_path));
                    let buffer = strip_trace_ext(&seed).to_string();
                    self.mode = Mode::Rename { buffer };
                }
                DetailAction::None
            }
            KeyCode::Char('t') => {
                if let Some(t) = self.selected_trace() {
                    let buffer = t.tags.join(", ");
                    self.mode = Mode::EditTags { buffer };
                }
                DetailAction::None
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if self.selected_trace().is_some() {
                    self.mode = Mode::ConfirmDelete;
                }
                DetailAction::None
            }
            KeyCode::Char('f') => {
                self.cycle_filter();
                DetailAction::None
            }
            KeyCode::Char('o') | KeyCode::Enter => {
                if let Some(t) = self.selected_trace() {
                    DetailAction::OpenTrace(t.file_path.clone())
                } else {
                    DetailAction::None
                }
            }
            KeyCode::Char('s') => {
                if let Some(t) = self.selected_trace() {
                    if t.uploads.len() == 1 {
                        let text = t.uploads.values().next().unwrap().clone();
                        match cli_clipboard::set_contents(text) {
                            Ok(_) => self.set_status("link copied to clipboard".into()),
                            Err(e) => self.set_error(format!("clipboard: {e}")),
                        }
                    } else if t.uploads.len() > 1 {
                        let entries: Vec<(String, String)> = t
                            .uploads
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                        self.mode = Mode::SharePickProvider {
                            entries,
                            selected: 0,
                        };
                    }
                }
                DetailAction::None
            }
            KeyCode::Char('u') => {
                if let Some(t) = self.selected_trace() {
                    let scope = UploadScope::SingleTrace(t.id);
                    self.enter_upload_or_pick(scope);
                }
                DetailAction::None
            }
            KeyCode::Char('U') => {
                if !self.traces.is_empty() {
                    self.enter_upload_or_pick(UploadScope::AllTraces);
                }
                DetailAction::None
            }
            KeyCode::Char('a') => {
                if let Some(t) = self.selected_trace() {
                    DetailAction::Analyze(t.file_path.clone())
                } else {
                    DetailAction::None
                }
            }
            KeyCode::Char('d') => DetailAction::OpenFolder(self.session.folder_path.clone()),
            KeyCode::Char('[') => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
                DetailAction::None
            }
            KeyCode::Char(']') => {
                self.preview_scroll = self.preview_scroll.saturating_add(1);
                DetailAction::None
            }
            _ => DetailAction::None,
        }
    }

    pub fn set_status(&mut self, message: String) {
        self.status.set(message);
        self.error = None;
    }

    pub fn set_error(&mut self, message: String) {
        self.error = Some(message);
        self.status.clear();
    }

    fn handle_rename_key(&mut self, db: &Database, key: KeyEvent) -> DetailAction {
        let Mode::Rename { mut buffer } = std::mem::replace(&mut self.mode, Mode::Browse) else {
            return DetailAction::None;
        };
        // Filenames should stay shell-friendly — translate literal spaces to
        // dashes as the user types. Tag editing still allows spaces.
        let mut key = key;
        if matches!(key.code, KeyCode::Char(' ')) {
            key.code = KeyCode::Char('-');
        }
        match text_input::apply(&mut buffer, &key) {
            TextAction::Cancel => {}
            TextAction::Submit => {
                if let Some(t) = self.selected_trace().cloned() {
                    let stem = strip_trace_ext(buffer.trim()).trim();
                    let (label_opt, new_file_path) = if stem.is_empty() {
                        // Empty name — treat as no-op, keep existing.
                        self.mode = Mode::Browse;
                        return DetailAction::None;
                    } else {
                        let new_name = format!("{stem}{TRACE_EXT}");
                        let new_path = t.file_path.with_file_name(&new_name);
                        // Rename the physical file on disk.
                        if t.file_path != new_path {
                            if let Err(e) = std::fs::rename(&t.file_path, &new_path) {
                                self.error = Some(format!("file rename failed: {e}"));
                                self.mode = Mode::Browse;
                                return DetailAction::None;
                            }
                        }
                        (Some(new_name), Some(new_path))
                    };
                    if let Err(e) = db.rename_trace(
                        t.id,
                        label_opt.as_deref(),
                        new_file_path.as_deref(),
                    ) {
                        self.error = Some(format!("rename failed: {e}"));
                    }
                    self.reload(db);
                }
            }
            TextAction::Edited | TextAction::Ignored => {
                self.mode = Mode::Rename { buffer };
            }
        }
        DetailAction::None
    }

    fn handle_tags_key(&mut self, db: &Database, key: KeyEvent) -> DetailAction {
        let Mode::EditTags { mut buffer } = std::mem::replace(&mut self.mode, Mode::Browse) else {
            return DetailAction::None;
        };
        match text_input::apply(&mut buffer, &key) {
            TextAction::Cancel => {}
            TextAction::Submit => {
                if let Some(t) = self.selected_trace().cloned() {
                    let tags: Vec<String> = buffer
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if let Err(e) = db.set_trace_tags(t.id, &tags) {
                        self.error = Some(format!("tag save failed: {e}"));
                    }
                    self.reload(db);
                }
            }
            TextAction::Edited | TextAction::Ignored => {
                self.mode = Mode::EditTags { buffer };
            }
        }
        DetailAction::None
    }

    fn handle_capture_name_key(&mut self, key: KeyEvent) -> DetailAction {
        let Mode::PromptCaptureName { mut buffer } =
            std::mem::replace(&mut self.mode, Mode::Browse)
        else {
            return DetailAction::None;
        };
        // Translate spaces to dashes so the on-disk filename stays shell-
        // friendly — same convention as the rename flow.
        let mut key = key;
        if matches!(key.code, KeyCode::Char(' ')) {
            key.code = KeyCode::Char('-');
        }
        match text_input::apply(&mut buffer, &key) {
            TextAction::Cancel => DetailAction::None,
            TextAction::Submit => {
                let stem = strip_trace_ext(buffer.trim()).trim().to_string();
                if stem.is_empty() {
                    // Empty submit → fall back to the timestamped default
                    // rather than capturing into a `.pftrace` with no stem.
                    DetailAction::Capture(None)
                } else {
                    DetailAction::Capture(Some(stem))
                }
            }
            TextAction::Edited | TextAction::Ignored => {
                self.mode = Mode::PromptCaptureName { buffer };
                DetailAction::None
            }
        }
    }

    fn handle_confirm_delete(&mut self, db: &Database, key: KeyEvent) -> DetailAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(t) = self.selected_trace().cloned() {
                    if let Err(e) = db.delete_trace(t.id) {
                        self.error = Some(format!("db delete failed: {e}"));
                    }
                    if t.file_path.exists() {
                        if let Err(e) = std::fs::remove_file(&t.file_path) {
                            self.error = Some(format!("file delete failed: {e}"));
                        }
                    }
                    self.reload(db);
                }
                self.mode = Mode::Browse;
            }
            _ => self.mode = Mode::Browse,
        }
        DetailAction::None
    }

    /// Enter provider picker if multiple providers exist, otherwise go straight
    /// to upload confirm with the single/default provider.
    fn enter_upload_or_pick(&mut self, scope: UploadScope) {
        let all = cloud::all_providers();
        if all.len() <= 1 {
            let p = all.first().expect("at least one provider");
            self.mode = Mode::UploadConfirm {
                scope,
                provider_id: p.id().to_string(),
                provider_name: p.name().to_string(),
            };
        } else {
            // Build list with default first.
            let mut providers: Vec<(String, String)> = Vec::with_capacity(all.len());
            // Default first.
            if let Some(def) = all.iter().find(|p| p.id() == self.cloud_provider_id) {
                providers.push((def.id().to_string(), def.name().to_string()));
            }
            for p in &all {
                if p.id() != self.cloud_provider_id {
                    providers.push((p.id().to_string(), p.name().to_string()));
                }
            }
            self.mode = Mode::UploadPickProvider {
                scope,
                providers,
                selected: 0,
            };
        }
    }

    fn handle_upload_pick_provider(&mut self, key: KeyEvent) -> DetailAction {
        let Mode::UploadPickProvider {
            scope,
            providers,
            mut selected,
        } = std::mem::replace(&mut self.mode, Mode::Browse)
        else {
            return DetailAction::None;
        };
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                selected = if selected == 0 {
                    providers.len() - 1
                } else {
                    selected - 1
                };
                self.mode = Mode::UploadPickProvider {
                    scope,
                    providers,
                    selected,
                };
            }
            KeyCode::Right | KeyCode::Char('l') => {
                selected = (selected + 1) % providers.len();
                self.mode = Mode::UploadPickProvider {
                    scope,
                    providers,
                    selected,
                };
            }
            KeyCode::Enter => {
                let (id, name) = providers[selected].clone();
                self.mode = Mode::UploadConfirm {
                    scope,
                    provider_id: id,
                    provider_name: name,
                };
            }
            KeyCode::Esc => {} // already replaced with Browse
            _ => {
                self.mode = Mode::UploadPickProvider {
                    scope,
                    providers,
                    selected,
                };
            }
        }
        DetailAction::None
    }

    fn handle_upload_confirm(&mut self, key: KeyEvent) -> DetailAction {
        let Mode::UploadConfirm {
            scope,
            provider_id,
            provider_name,
        } = std::mem::replace(&mut self.mode, Mode::Browse)
        else {
            return DetailAction::None;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                self.cloud_provider_name = provider_name;
                DetailAction::Upload(scope, provider_id)
            }
            _ => DetailAction::None, // any other key cancels
        }
    }

    fn handle_share_pick_provider(&mut self, key: KeyEvent) -> DetailAction {
        let Mode::SharePickProvider {
            entries,
            mut selected,
        } = std::mem::replace(&mut self.mode, Mode::Browse)
        else {
            return DetailAction::None;
        };
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                selected = if selected == 0 {
                    entries.len() - 1
                } else {
                    selected - 1
                };
                self.mode = Mode::SharePickProvider { entries, selected };
            }
            KeyCode::Right | KeyCode::Char('l') => {
                selected = (selected + 1) % entries.len();
                self.mode = Mode::SharePickProvider { entries, selected };
            }
            KeyCode::Enter => {
                let (_, url) = &entries[selected];
                match cli_clipboard::set_contents(url.clone()) {
                    Ok(_) => self.set_status("link copied to clipboard".into()),
                    Err(e) => self.set_error(format!("clipboard: {e}")),
                }
            }
            KeyCode::Esc => {} // already replaced with Browse
            _ => {
                self.mode = Mode::SharePickProvider { entries, selected };
            }
        }
        DetailAction::None
    }

    fn handle_uploading_key(&mut self, key: KeyEvent) -> DetailAction {
        // Allow Esc or Ctrl-C to cancel the upload.
        let ctrl_c = matches!(
            (key.code, key.modifiers),
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL)
        );
        if key.code == KeyCode::Esc || ctrl_c {
            if let Some(cancel) = &self.upload_cancel {
                cancel.cancel();
            }
        }
        DetailAction::None
    }

    /// Called by App to set the cancel handle for the current upload.
    pub fn set_upload_cancel(&mut self, cancel: Arc<Cancel>) {
        self.upload_cancel = Some(cancel);
    }

    /// Transition to the Uploading mode.
    pub fn enter_uploading(&mut self) {
        self.mode = Mode::Uploading { progress: None };
    }

    /// Update progress during an upload.
    pub fn on_upload_progress(&mut self, progress: UploadProgress) {
        if let Mode::Uploading { progress: p } = &mut self.mode {
            *p = Some(progress);
        }
    }

    /// Handle upload completion: reload traces, copy link to clipboard.
    pub fn on_upload_done(
        &mut self,
        db: &Database,
        result: Result<crate::cloud::UploadResult, String>,
    ) {
        self.upload_cancel = None;
        self.mode = Mode::Browse;
        match result {
            Ok(upload) => {
                // Reload traces so the UI picks up the new remote_url values.
                self.reload(db);

                let count = upload.traces.len();

                // Copy the most useful link to clipboard:
                // - Single trace: copy the trace URL
                // - Multiple traces: copy the folder URL
                let link_to_copy = if count == 1 {
                    upload.traces.first().and_then(|(_, url)| url.clone())
                } else {
                    upload.folder_url.clone()
                };

                let provider = &self.cloud_provider_name;
                let mut msg = if count == 1 {
                    format!("uploaded 1 trace to {provider}")
                } else {
                    format!("uploaded {count} traces to {provider}")
                };

                if let Some(link) = link_to_copy {
                    match cli_clipboard::set_contents(link) {
                        Ok(_) => msg.push_str(" — link copied"),
                        Err(e) => tracing::warn!(?e, "clipboard copy failed"),
                    }
                }

                self.set_status(msg);
            }
            Err(msg) => self.set_error(format!("upload failed: {msg}")),
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as i32;
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.list_state.select(Some(next as usize));
    }

    fn selected_trace(&self) -> Option<&TraceRecord> {
        let slot = self.list_state.selected()?;
        let idx = *self.visible.get(slot)?;
        self.traces.get(idx)
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        let header = chrome::app_header(Line::from(vec![
            Span::styled("  📁 ", theme::title()),
            Span::styled(
                self.session.name.clone(),
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  — {}", self.session.package_name),
                theme::hint(),
            ),
        ]));
        frame.render_widget(header, outer[0]);

        // Split the middle row into left (session + traces) and right
        // (textproto preview) when there's enough horizontal room. On narrow
        // terminals we drop the right pane entirely.
        let (left_area, right_area) = if area.width >= TWO_PANE_WIDTH {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(outer[1]);
            (cols[0], Some(cols[1]))
        } else {
            (outer[1], None)
        };

        let left_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(8), Constraint::Min(1)])
            .split(left_area);
        let meta_area = left_rows[0];
        let traces_area = left_rows[1];

        if let Some(right_area) = right_area {
            // Imported sessions replace the textproto preview with a
            // macrobenchmark metrics summary — the textproto here is a
            // placeholder stub, and the JSON the user actually cares about
            // is next to the traces.
            let show_benchmark_summary =
                self.session.is_imported && self.benchmarks.is_some();
            // Startup commands don't apply to imported sessions, but the
            // check on `startup_commands` already covers that (imports leave
            // it empty).
            let has_commands = !self.session.config.startup_commands.is_empty();
            let (proto_area, cmd_area) = if has_commands {
                let rows = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(5), Constraint::Length(
                        // 2 for border + 1 per command, capped at 8
                        (self.session.config.startup_commands.len() as u16 + 2).min(8),
                    )])
                    .split(right_area);
                (rows[0], Some(rows[1]))
            } else {
                (right_area, None)
            };

            if show_benchmark_summary {
                let lines = benchmark_summary_lines(self.benchmarks.as_deref().unwrap_or(&[]));
                let preview = Paragraph::new(lines)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(Span::styled(" Benchmark results ", theme::title())),
                    )
                    .scroll((self.preview_scroll, 0))
                    .wrap(Wrap { trim: false });
                frame.render_widget(preview, proto_area);
            } else if self.session.is_imported {
                // Imported session, but no benchmarks parsed — show the
                // load error or a helpful stub.
                let msg = self
                    .benchmark_load_error
                    .clone()
                    .unwrap_or_else(|| "No benchmark JSON found for this session.".into());
                let preview = Paragraph::new(vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("  {msg}"),
                        Style::default().fg(theme::err()),
                    )),
                ])
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(Span::styled(" Benchmark results ", theme::title())),
                )
                .wrap(Wrap { trim: false });
                frame.render_widget(preview, proto_area);
            } else {
                let textproto = textproto::build(&self.session.config);
                let preview = Paragraph::new(textproto)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(Span::styled(" Config (textproto) ", theme::title())),
                    )
                    .scroll((self.preview_scroll, 0))
                    .wrap(Wrap { trim: false });
                frame.render_widget(preview, proto_area);
            }

            if let Some(cmd_area) = cmd_area {
                let lines: Vec<Line> = self
                    .session
                    .config
                    .startup_commands
                    .iter()
                    .map(|cmd| {
                        let short = cmd
                            .id
                            .strip_prefix("dev.perfetto.")
                            .unwrap_or(&cmd.id);
                        let args = if cmd.args.is_empty() || cmd.args.iter().all(|a| a.is_empty()) {
                            String::new()
                        } else {
                            format!(
                                "  ({})",
                                cmd.args
                                    .iter()
                                    .filter(|a| !a.is_empty())
                                    .map(|a| if a.len() > 25 {
                                        format!("{}…", &a[..25])
                                    } else {
                                        a.clone()
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        };
                        Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                short.to_string(),
                                Style::default().add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(args, theme::hint()),
                        ])
                    })
                    .collect();
                let cmd_block = Paragraph::new(lines).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(Span::styled(
                            format!(
                                " Startup Commands ({}) ",
                                self.session.config.startup_commands.len()
                            ),
                            theme::title(),
                        )),
                );
                frame.render_widget(cmd_block, cmd_area);
            }
        }

        let metadata_lines = vec![
            Line::from(vec![
                Span::styled("  Device:     ", theme::hint()),
                Span::raw(
                    self.session
                        .device_serial
                        .clone()
                        .unwrap_or_else(|| "(none)".into()),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Created:    ", theme::hint()),
                Span::raw(self.session.created_at.format("%Y-%m-%d %H:%M:%S").to_string()),
            ]),
            Line::from(vec![
                Span::styled("  Folder:     ", theme::hint()),
                Span::raw(self.session.folder_path.to_string_lossy().into_owned()),
            ]),
            Line::from(vec![
                Span::styled("  Duration:   ", theme::hint()),
                Span::raw(format!("{} ms", self.session.config.duration_ms)),
            ]),
            Line::from(vec![
                Span::styled("  Buffer:     ", theme::hint()),
                Span::raw(format!("{} KB", self.session.config.buffer_size_kb)),
            ]),
            Line::from(vec![
                Span::styled("  Cold start: ", theme::hint()),
                Span::raw(if self.session.config.cold_start {
                    "yes"
                } else {
                    "no"
                }),
                Span::styled("    Auto-open: ", theme::hint()),
                Span::raw(if self.session.config.auto_open {
                    "yes"
                } else {
                    "no"
                }),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(metadata_lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(Span::styled(" Session ", theme::title())),
                ),
            meta_area,
        );

        let title = match &self.tag_filter {
            Some(tag) => format!(" Traces — filter: {tag} "),
            None => " Traces ".into(),
        };
        let traces_block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, theme::title()));

        if let Some(err) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("  ✗ {err}"),
                    Style::default().fg(theme::err()),
                )))
                .block(traces_block),
                traces_area,
            );
        } else if self.visible.is_empty() {
            let msg = if self.traces.is_empty() {
                "  No traces captured yet."
            } else {
                "  No traces match the current filter."
            };
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(msg),
                    Line::from(""),
                    Line::from(Span::styled(
                        if self.traces.is_empty() {
                            "  Press [c] to run a capture."
                        } else {
                            "  Press [f] to cycle the filter."
                        },
                        theme::hint(),
                    )),
                ])
                .block(traces_block),
                traces_area,
            );
        } else {
            let items: Vec<ListItem> = self
                .visible
                .iter()
                .filter_map(|i| self.traces.get(*i))
                .map(|t| {
                    let label = t
                        .label
                        .clone()
                        .unwrap_or_else(|| file_name(&t.file_path));
                    let when = t.captured_at.format("%Y-%m-%d %H:%M:%S").to_string();
                    let size = t
                        .size_bytes
                        .map(|b| format!("{:.1} KB", b as f64 / 1024.0))
                        .unwrap_or_else(|| "—".into());
                    let duration = t
                        .duration_ms
                        .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
                        .unwrap_or_else(|| "—".into());
                    let mut spans = vec![
                        Span::raw("  "),
                        Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
                        Span::raw("  "),
                        Span::styled(when, theme::hint()),
                        Span::raw("  "),
                        Span::styled(format!("{size}  {duration}"), theme::hint()),
                    ];
                    if !t.uploads.is_empty() {
                        let names: Vec<&str> = t.uploads.keys().map(|s| s.as_str()).collect();
                        spans.push(Span::styled(
                            format!("  [{}]", names.join(", ")),
                            Style::default()
                                .fg(theme::accent_secondary())
                                .add_modifier(Modifier::DIM),
                        ));
                    }
                    if !t.tags.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(
                            format!("#{}", t.tags.join(" #")),
                            Style::default().fg(theme::accent()),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let list = List::new(items)
                .block(traces_block)
                .highlight_style(Style::default().bg(theme::accent()).fg(Color::Black))
                .highlight_symbol("▶ ");
            frame.render_stateful_widget(list, traces_area, &mut self.list_state);
        }

        let footer = match &self.mode {
            Mode::Rename { buffer } => Line::from(vec![
                Span::styled(" rename › ", theme::title()),
                Span::raw(buffer.clone()),
                Span::styled("█", Style::default().fg(theme::accent())),
                Span::styled(TRACE_EXT, theme::hint()),
                Span::styled(
                    "   [Enter] save  [Esc] cancel  [Alt-⌫] word  [Ctrl-U] clear",
                    theme::hint(),
                ),
            ]),
            Mode::EditTags { buffer } => Line::from(vec![
                Span::styled(" tags › ", theme::title()),
                Span::raw(buffer.clone()),
                Span::styled("█", Style::default().fg(theme::accent())),
                Span::styled(
                    "   comma-separated  [Enter] save  [Esc] cancel",
                    theme::hint(),
                ),
            ]),
            Mode::PromptCaptureName { buffer } => Line::from(vec![
                Span::styled(" capture name › ", theme::title()),
                Span::raw(buffer.clone()),
                Span::styled("█", Style::default().fg(theme::accent())),
                Span::styled(TRACE_EXT, theme::hint()),
                Span::styled(
                    "   [Enter] start  [Esc] cancel  (empty = timestamp)",
                    theme::hint(),
                ),
            ]),
            Mode::ConfirmDelete => {
                let name = self
                    .selected_trace()
                    .map(|t| t.label.clone().unwrap_or_else(|| file_name(&t.file_path)))
                    .unwrap_or_else(|| "?".into());
                Line::from(vec![
                    Span::styled(
                        format!(" ⚠ delete \"{name}\" and its file? "),
                        Style::default().fg(theme::warn()),
                    ),
                    Span::styled("[y]", theme::title()),
                    Span::raw(" yes  "),
                    Span::styled("[n]", theme::title()),
                    Span::raw(" cancel"),
                ])
            }
            Mode::UploadPickProvider {
                providers,
                selected,
                ..
            } => {
                let mut spans = vec![Span::styled(" Upload to: ", theme::title())];
                for (i, (id, name)) in providers.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::styled("  ▸  ", theme::hint()));
                    }
                    let is_default = *id == self.cloud_provider_id;
                    let label = if is_default {
                        format!("★ {name}")
                    } else {
                        name.clone()
                    };
                    if i == *selected {
                        spans.push(Span::styled(
                            format!(" {label} "),
                            Style::default().bg(theme::accent()).fg(Color::Black),
                        ));
                    } else {
                        spans.push(Span::styled(label, theme::hint()));
                    }
                }
                spans.extend([
                    Span::styled("   [Enter]", theme::title()),
                    Span::raw(" select  "),
                    Span::styled("[Esc]", theme::title()),
                    Span::raw(" cancel"),
                ]);
                Line::from(spans)
            }
            Mode::UploadConfirm {
                scope,
                provider_name,
                ..
            } => {
                let label = match scope {
                    UploadScope::SingleTrace(_) => {
                        self.selected_trace()
                            .map(|t| t.label.clone().unwrap_or_else(|| file_name(&t.file_path)))
                            .unwrap_or_else(|| "trace".into())
                    }
                    UploadScope::AllTraces => format!("all {} traces", self.traces.len()),
                };
                Line::from(vec![
                    Span::styled(
                        format!(" Upload \"{label}\" to {provider_name}? "),
                        Style::default().fg(theme::accent()),
                    ),
                    Span::styled("[y]", theme::title()),
                    Span::raw(" yes  "),
                    Span::styled("[n]", theme::title()),
                    Span::raw(" cancel"),
                ])
            }
            Mode::Uploading { progress } => {
                if let Some(p) = progress {
                    let pct = if p.total_bytes > 0 {
                        (p.bytes_sent as f64 / p.total_bytes as f64 * 100.0) as u8
                    } else {
                        0
                    };
                    let file_label = if p.total_files > 1 {
                        format!("{} ({}/{})", p.file_name, p.file_index + 1, p.total_files)
                    } else {
                        p.file_name.clone()
                    };
                    Line::from(vec![
                        Span::styled(" Uploading ", theme::title()),
                        Span::raw(file_label),
                        Span::raw(format!("  {pct}%  ")),
                        Span::styled("[Esc]", theme::title()),
                        Span::raw(" cancel"),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled(format!(" Authenticating with {}… ", self.cloud_provider_name), theme::hint()),
                        Span::styled("[Esc]", theme::title()),
                        Span::raw(" cancel"),
                    ])
                }
            }
            Mode::SharePickProvider { entries, selected } => {
                let mut spans = vec![Span::styled(" Copy link: ", theme::title())];
                for (i, (name, _)) in entries.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::styled("  ▸  ", theme::hint()));
                    }
                    if i == *selected {
                        spans.push(Span::styled(
                            format!(" {name} "),
                            Style::default().bg(theme::accent()).fg(Color::Black),
                        ));
                    } else {
                        spans.push(Span::styled(name.clone(), theme::hint()));
                    }
                }
                spans.extend([
                    Span::styled("   [Enter]", theme::title()),
                    Span::raw(" copy  "),
                    Span::styled("[Esc]", theme::title()),
                    Span::raw(" cancel"),
                ]);
                Line::from(spans)
            }
            Mode::Browse => {
                if let Some(msg) = self.status.get() {
                    Line::from(Span::styled(
                        format!(" ✓ {msg}"),
                        Style::default().fg(theme::ok()),
                    ))
                } else {
                    let has_link = self
                        .selected_trace()
                        .is_some_and(|t| !t.uploads.is_empty());
                    let mut spans = vec![
                        Span::styled(" [c]", theme::title()),
                        Span::raw(" capture  "),
                        Span::styled("[C]", theme::title()),
                        Span::raw(" capture as…  "),
                        Span::styled("[o]", theme::title()),
                        Span::raw(" open  "),
                        Span::styled("[a]", theme::title()),
                        Span::raw(" analyze  "),
                        Span::styled("[u]", theme::title()),
                        Span::raw(" upload  "),
                    ];
                    if has_link {
                        spans.push(Span::styled("[s]", theme::title()));
                        spans.push(Span::raw(" share  "));
                    }
                    spans.extend([
                        Span::styled("[r]", theme::title()),
                        Span::raw(" rename  "),
                        Span::styled("[t]", theme::title()),
                        Span::raw(" tag  "),
                        Span::styled("[x]", theme::title()),
                        Span::raw(" delete  "),
                        Span::styled("[f]", theme::title()),
                        Span::raw(" filter  "),
                        Span::styled("[d]", theme::title()),
                        Span::raw(" folder  "),
                        Span::styled("[e]", theme::title()),
                        Span::raw(" config  "),
                        Span::styled("[Esc]", theme::title()),
                        Span::raw(" back"),
                    ]);
                    Line::from(spans)
                }
            }
        };
        frame.render_widget(Paragraph::new(footer), outer[2]);
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Case-insensitively strip the trailing `.pftrace` extension, if present.
fn strip_trace_ext(s: &str) -> &str {
    if s.len() >= TRACE_EXT.len()
        && s[s.len() - TRACE_EXT.len()..].eq_ignore_ascii_case(TRACE_EXT)
    {
        &s[..s.len() - TRACE_EXT.len()]
    } else {
        s
    }
}

/// Parse the macrobenchmark JSON for imported sessions once, at screen
/// construction. Returns (benchmarks, error) — either the Vec or a
/// human-readable error string, never both.
fn load_benchmarks(session: &Session) -> (Option<Vec<Benchmark>>, Option<String>) {
    if !session.is_imported {
        return (None, None);
    }
    let Some(path) = session.benchmark_json_path.as_ref() else {
        return (None, None);
    };
    match benchmark_json::parse(path) {
        Ok(bms) => (Some(bms), None),
        Err(e) => (None, Some(e.to_string())),
    }
}

/// Build the lines that render the macrobenchmark summary on the right pane
/// of an imported session. Each benchmark contributes a heading plus one line
/// per metric with min/median/max.
fn benchmark_summary_lines(benchmarks: &[Benchmark]) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if benchmarks.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no benchmarks in JSON)",
            theme::hint(),
        )));
        return lines;
    }
    for (i, b) in benchmarks.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{}.{}", short_class(&b.class_name), b.method_name),
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        if b.metrics.is_empty() {
            lines.push(Line::from(Span::styled(
                "    (no metrics)",
                theme::hint(),
            )));
            continue;
        }
        for m in &b.metrics {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    m.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]));
            let body = if m.runs.is_empty()
                && m.minimum == 0.0
                && m.median == 0.0
                && m.maximum == 0.0
            {
                "      (unparsed — see JSON)".to_string()
            } else {
                format!(
                    "      min {}  ·  median {}  ·  max {}  ({} run{})",
                    fmt_num(m.minimum),
                    fmt_num(m.median),
                    fmt_num(m.maximum),
                    m.runs.len(),
                    if m.runs.len() == 1 { "" } else { "s" },
                )
            };
            lines.push(Line::from(Span::styled(body, theme::hint())));
        }
    }
    lines
}

fn short_class(fqcn: &str) -> &str {
    fqcn.rsplit('.').next().unwrap_or(fqcn)
}

/// Format a macrobenchmark numeric value: integer when it's whole, otherwise
/// 3 significant decimals.
fn fmt_num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e12 {
        format!("{:.0}", v)
    } else {
        format!("{:.3}", v)
    }
}
