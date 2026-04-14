use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use crossterm::execute;
use crossterm::event::{KeyEvent, KeyEventKind};
use crossterm::terminal::SetTitle;
use tokio::sync::mpsc::UnboundedSender;

use crate::cloud::{self, CloudProvider};
use crate::config::Paths;
use crate::db::Database;
use crate::perfetto::capture::Cancel;
use crate::session::Session;
use crate::tui::Tui;
use crate::tui::event::{self, AppEvent};
use crate::tui::screens::analysis::{AnalysisAction, AnalysisScreen};
use crate::tui::screens::capture::{CaptureAction, CaptureScreen};
use crate::tui::screens::diff::{DiffAction, DiffScreen};
use crate::tui::screens::cloud_providers::{CloudProvidersScreen, ProviderAction};
use crate::tui::screens::command_set_editor::{CmdEditorAction, CommandSetEditorScreen};
use crate::tui::screens::command_set_list::{CommandSetListAction, CommandSetListScreen};
use crate::tui::screens::config_editor::{ConfigEditorScreen, EditorAction};
use crate::tui::screens::config_import::{ConfigImportScreen, ImportAction};
use crate::tui::screens::config_list::{ConfigListAction, ConfigListScreen};
use crate::tui::screens::new_session::{NewSessionScreen, WizardAction};
use crate::tui::screens::session_detail::{DetailAction, SessionDetailScreen, UploadScope};
use crate::tui::screens::sessions_list::{SessionsAction, SessionsListScreen};
use crate::tui::screens::theme_picker::{ThemePickerAction, ThemePickerScreen};
use crate::ui_server::UiServer;

/// Tracks what opened the config editor so we know where to route the save.
#[derive(Debug, Clone)]
enum EditorContext {
    /// Editing a session's config — save goes to `update_session_config`.
    Session(i64),
    /// Editing a standalone saved config — save goes to `update_config`.
    SavedConfig(i64),
    /// Creating a new standalone config — save goes to `create_config`.
    NewConfig(String),
}

/// Tracks what opened the command set editor.
#[derive(Debug, Clone)]
enum CmdSetEditorContext {
    Existing(i64),
    New(String),
}

enum Screen {
    SessionsList,
    NewSession(NewSessionScreen),
    SessionDetail(SessionDetailScreen),
    ConfigEditor(ConfigEditorScreen),
    ConfigList(ConfigListScreen),
    ConfigImport(ConfigImportScreen<'static>),
    CommandSetList(CommandSetListScreen),
    CommandSetEditor(CommandSetEditorScreen),
    Capture(CaptureScreen),
    ThemePicker(ThemePickerScreen),
    CloudProviders(CloudProvidersScreen),
    Analysis(AnalysisScreen),
    Diff(DiffScreen),
}

pub struct App {
    db: Database,
    paths: Paths,
    should_quit: bool,
    screen: Screen,
    sessions_list: SessionsListScreen,
    event_tx: Option<UnboundedSender<AppEvent>>,
    ui_server: Option<UiServer>,
    /// What opened the config editor — controls where save routes to.
    editor_context: Option<EditorContext>,
    /// What opened the command set editor.
    cmd_editor_context: Option<CmdSetEditorContext>,
    /// Cloud provider used for uploads.
    cloud_provider: Arc<dyn CloudProvider>,
    /// Cancel handle for an in-flight upload.
    upload_cancel: Option<Arc<Cancel>>,
    /// Pending upload intent while OAuth is in progress.
    pending_upload: Option<(Session, UploadScope, Arc<dyn CloudProvider>)>,
}

impl App {
    pub fn new(db: Database, paths: Paths) -> Self {
        let sessions_list = SessionsListScreen::new(&db);
        let cloud_provider = cloud::default_provider(&db);

        Self {
            db,
            paths,
            should_quit: false,
            screen: Screen::SessionsList,
            sessions_list,
            event_tx: None,
            ui_server: None,
            editor_context: None,
            cmd_editor_context: None,
            cloud_provider,
            upload_cancel: None,
            pending_upload: None,
        }
    }

    fn open_trace(&mut self, trace: &Path, commands: &[crate::perfetto::commands::StartupCommand]) -> Result<String> {
        // Reap a previous server whose thread has already exited (trace
        // delivered + loop broken). Joining guarantees the :9001 listener is
        // fully released before we try to rebind it.
        if let Some(server) = &self.ui_server {
            if !server.is_alive() {
                self.ui_server.take().unwrap().join();
            }
        }

        if self.ui_server.is_none() {
            self.ui_server = Some(UiServer::start()?);
        }
        let server = self.ui_server.as_ref().expect("ui_server just initialized");
        server.serve(trace, commands)
    }

    pub async fn run(mut self, terminal: &mut Tui) -> Result<()> {
        let mut bus = event::start();
        self.event_tx = Some(bus.tx.clone());

        while !self.should_quit {
            terminal.draw(|frame| self.render(frame))?;

            let Some(ev) = bus.rx.recv().await else {
                break;
            };
            self.handle_event(ev);
        }
        Ok(())
    }

    fn render(&mut self, frame: &mut ratatui::Frame) {
        let title = match &self.screen {
            Screen::SessionsList => "Perfetto CLI".to_string(),
            Screen::NewSession(_) => "Perfetto CLI — New Session".to_string(),
            Screen::SessionDetail(d) => format!("Perfetto CLI — {}", d.session().name),
            Screen::ConfigEditor(_) => "Perfetto CLI — Config".to_string(),
            Screen::ConfigList(_) => "Perfetto CLI — Configurations".to_string(),
            Screen::ConfigImport(_) => "Perfetto CLI — Import Config".to_string(),
            Screen::CommandSetList(_) => "Perfetto CLI — Startup Commands".to_string(),
            Screen::CommandSetEditor(_) => "Perfetto CLI — Edit Commands".to_string(),
            Screen::Capture(_) => "Perfetto CLI — Capturing".to_string(),
            Screen::ThemePicker(_) => "Perfetto CLI — Theme".to_string(),
            Screen::CloudProviders(_) => "Perfetto CLI — Cloud Providers".to_string(),
            Screen::Analysis(_) => "Perfetto CLI — Analyze".to_string(),
            Screen::Diff(_) => "Perfetto CLI — Diff".to_string(),
        };
        let _ = execute!(std::io::stdout(), SetTitle(title));

        match &mut self.screen {
            Screen::SessionsList => self.sessions_list.render(frame),
            Screen::NewSession(w) => w.render(frame),
            Screen::SessionDetail(d) => d.render(frame),
            Screen::ConfigEditor(e) => e.render(frame),
            Screen::ConfigList(cl) => cl.render(frame),
            Screen::ConfigImport(ci) => ci.render(frame),
            Screen::CommandSetList(cl) => cl.render(frame),
            Screen::CommandSetEditor(ce) => ce.render(frame),
            Screen::Capture(c) => c.render(frame),
            Screen::ThemePicker(tp) => tp.render(frame),
            Screen::CloudProviders(cp) => cp.render(frame),
            Screen::Analysis(a) => a.render(frame),
            Screen::Diff(d) => d.render(frame),
        }
    }

    fn handle_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            AppEvent::DevicesLoaded(result) => {
                if let Screen::NewSession(w) = &mut self.screen {
                    w.on_devices_loaded(result);
                }
            }
            AppEvent::DeviceInfoLoaded(result) => {
                if let Screen::NewSession(w) = &mut self.screen {
                    w.on_device_info_loaded(result);
                }
            }
            AppEvent::PackagesLoaded(result) => {
                if let Screen::NewSession(w) = &mut self.screen {
                    w.on_packages_loaded(result);
                }
            }
            AppEvent::Capture(ev) => {
                if let Screen::Capture(c) = &mut self.screen {
                    c.on_capture_event(ev);
                }
            }
            AppEvent::CaptureDone(result) => {
                // Pull out everything we need up front so we can release the
                // borrow on `self.screen` before touching other state.
                let (session_id, should_auto_open, trace_for_auto_open, startup_cmds) = {
                    let Screen::Capture(c) = &self.screen else {
                        return;
                    };
                    let id = c.session_id();
                    let session = self
                        .db
                        .list_sessions()
                        .ok()
                        .and_then(|mut list| list.drain(..).find(|s| s.id == Some(id)));
                    let (auto, path) = match (&result, &session) {
                        (Ok(captured), Some(s)) if !captured.cancelled => {
                            (s.config.auto_open, Some(captured.trace_path.clone()))
                        }
                        _ => (false, None),
                    };
                    let cmds = session
                        .map(|s| s.config.startup_commands)
                        .unwrap_or_default();
                    (id, auto, path, cmds)
                };

                if let Ok(captured) = &result {
                    if let Err(e) = self.db.create_trace(
                        session_id,
                        &captured.trace_path,
                        None,
                        Some(captured.duration_ms),
                        Some(captured.size_bytes),
                    ) {
                        tracing::error!(?e, "failed to record trace in db");
                    }
                }

                let auto_open_note = if should_auto_open {
                    match trace_for_auto_open.as_deref() {
                        Some(path) => match self.open_trace(path, &startup_cmds) {
                            Ok(_) => {
                                Some((crate::perfetto::capture::LogLevel::Ok, "opened in browser".to_string()))
                            }
                            Err(e) => Some((
                                crate::perfetto::capture::LogLevel::Warn,
                                format!("auto-open failed: {e}"),
                            )),
                        },
                        None => None,
                    }
                } else {
                    None
                };

                if let Screen::Capture(c) = &mut self.screen {
                    if let Some((level, msg)) = auto_open_note {
                        c.push_log(level, msg);
                    }
                    c.on_done(result);
                }
            }
            AppEvent::CloudAuthResult(result) => {
                // Route to the cloud providers screen if it's active,
                // otherwise handle as upload auth.
                if let Screen::CloudProviders(cp) = &mut self.screen {
                    cp.on_auth_result(result);
                } else {
                    match result {
                        Ok(provider_name) => {
                            tracing::info!(provider_name, "cloud auth succeeded");
                            if let Some((session, scope, provider)) = self.pending_upload.take() {
                                self.start_upload(session, scope, provider);
                            }
                        }
                        Err(msg) => {
                            tracing::error!(msg, "cloud auth failed");
                            self.pending_upload = None;
                            if let Screen::SessionDetail(d) = &mut self.screen {
                                d.set_error(format!("auth failed: {msg}"));
                            }
                        }
                    }
                }
            }
            AppEvent::CloudProviderStatus { provider_id, authenticated } => {
                if let Screen::CloudProviders(cp) = &mut self.screen {
                    cp.on_provider_status(&provider_id, authenticated);
                }
            }
            AppEvent::CloudUploadProgress(progress) => {
                if let Screen::SessionDetail(d) = &mut self.screen {
                    d.on_upload_progress(progress);
                }
            }
            AppEvent::CloudUploadDone(result) => {
                self.upload_cancel = None;
                if let Screen::SessionDetail(d) = &mut self.screen {
                    d.on_upload_done(&self.db, result);
                }
            }
            AppEvent::Analysis(ev) => {
                if let Screen::Analysis(a) = &mut self.screen {
                    a.on_event(ev);
                }
            }
            AppEvent::Diff { side, event } => {
                if let Screen::Diff(d) = &mut self.screen {
                    d.on_event(side, event);
                }
            }
            AppEvent::Paste(text) => self.handle_paste(text),
            _ => {}
        }
    }

    /// Route a bracketed-paste payload to whichever screen wants it. Screens
    /// without text input ignore paste; screens with single-line inputs
    /// collapse newlines so a multi-line paste doesn't break the form.
    fn handle_paste(&mut self, text: String) {
        match &mut self.screen {
            Screen::Analysis(a) => a.on_paste(&text),
            Screen::ConfigImport(ci) => ci.on_paste(&text),
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match &mut self.screen {
            Screen::SessionsList => match self.sessions_list.on_key(&self.db, key) {
                SessionsAction::Quit => self.should_quit = true,
                SessionsAction::OpenConfigList => {
                    self.screen = Screen::ConfigList(ConfigListScreen::new(self.db.clone()));
                }
                SessionsAction::OpenCommandSets => {
                    self.screen =
                        Screen::CommandSetList(CommandSetListScreen::new(self.db.clone()));
                }
                SessionsAction::NewSession => {
                    let tx = self.require_tx();
                    self.screen = Screen::NewSession(NewSessionScreen::new(
                        self.db.clone(),
                        self.paths.clone(),
                        tx,
                    ));
                }
                SessionsAction::OpenSession(id) => {
                    if let Some(session) =
                        self.db.list_sessions().ok().and_then(|mut list| {
                            list.drain(..).find(|s| s.id == Some(id))
                        })
                    {
                        self.screen = Screen::SessionDetail(SessionDetailScreen::new(
                            session, &self.db, self.cloud_provider.id(), self.cloud_provider.name(),
                        ));
                    }
                }
                SessionsAction::OpenThemePicker => {
                    self.screen =
                        Screen::ThemePicker(ThemePickerScreen::new(self.paths.themes_dir()));
                }
                SessionsAction::OpenCloudProviders => {
                    let tx = self.require_tx();
                    self.screen =
                        Screen::CloudProviders(CloudProvidersScreen::new(self.db.clone(), tx));
                }
                SessionsAction::None => {}
            },
            Screen::NewSession(w) => match w.on_key(key) {
                WizardAction::Cancel => self.return_to_sessions_list(),
                WizardAction::Created(id) => {
                    self.sessions_list.reload(&self.db);
                    if let Some(session) = self
                        .db
                        .list_sessions()
                        .ok()
                        .and_then(|mut list| list.drain(..).find(|s| s.id == Some(id)))
                    {
                        self.screen = Screen::SessionDetail(SessionDetailScreen::new(
                            session, &self.db, self.cloud_provider.id(), self.cloud_provider.name(),
                        ));
                    } else {
                        self.return_to_sessions_list();
                    }
                }
                WizardAction::None => {}
            },
            Screen::SessionDetail(d) => match d.on_key(&self.db, key) {
                DetailAction::Back => self.return_to_sessions_list(),
                DetailAction::EditConfig => {
                    let session = d.session().clone();
                    self.editor_context = Some(EditorContext::Session(session.id.unwrap_or(0)));
                    let editor = ConfigEditorScreen::new(
                        session.id,
                        session.name.clone(),
                        &session.config,
                    );
                    self.screen = Screen::ConfigEditor(editor);
                }
                DetailAction::Capture => {
                    let session = d.session().clone();
                    let tx = self.require_tx();
                    self.screen = Screen::Capture(CaptureScreen::new(&session, tx));
                }
                DetailAction::OpenTrace(path) => {
                    let cmds = d.session().config.startup_commands.clone();
                    let outcome = self.open_trace(&path, &cmds);
                    if let Screen::SessionDetail(d) = &mut self.screen {
                        match outcome {
                            Ok(_) => d.set_status("opened in browser".into()),
                            Err(e) => d.set_error(format!("open failed: {e}")),
                        }
                    }
                }
                DetailAction::OpenFolder(path) => {
                    let outcome = open::that(&path);
                    if let Screen::SessionDetail(d) = &mut self.screen {
                        match outcome {
                            Ok(_) => d.set_status("opened folder".into()),
                            Err(e) => d.set_error(format!("open folder failed: {e}")),
                        }
                    }
                }
                DetailAction::Upload(scope, provider_id) => {
                    let session = d.session().clone();
                    let provider = cloud::provider_by_id(&provider_id)
                        .unwrap_or_else(|| self.cloud_provider.clone());
                    self.initiate_upload(session, scope, provider);
                }
                DetailAction::Analyze(path) => {
                    let session = d.session();
                    let session_id = session.id.unwrap_or(0);
                    let package_name = session.package_name.clone();
                    let tx = self.require_tx();
                    self.screen = Screen::Analysis(AnalysisScreen::new(
                        self.db.clone(),
                        self.paths.clone(),
                        path,
                        session_id,
                        tx,
                        package_name,
                    ));
                }
                DetailAction::Diff { left, right } => {
                    let session = d.session();
                    let session_id = session.id.unwrap_or(0);
                    let package_name = session.package_name.clone();
                    let tx = self.require_tx();
                    self.screen = Screen::Diff(DiffScreen::new(
                        self.db.clone(),
                        self.paths.clone(),
                        session_id,
                        package_name,
                        left.0,
                        left.1,
                        right.0,
                        right.1,
                        tx,
                    ));
                }
                DetailAction::None => {}
            },
            Screen::Capture(c) => match c.on_key(key) {
                CaptureAction::Back => {
                    let session_id = c.session_id();
                    self.return_to_detail(Some(session_id));
                }
                CaptureAction::None => {}
            },
            Screen::Diff(d) => match d.on_key(key) {
                DiffAction::Back => {
                    let session_id = d.session_id();
                    self.return_to_detail(Some(session_id));
                }
                DiffAction::None => {}
            },
            Screen::Analysis(a) => match a.on_key(key) {
                AnalysisAction::Back => {
                    let session_id = a.session_id();
                    self.return_to_detail(Some(session_id));
                }
                AnalysisAction::OpenInBrowser(path) => {
                    // Reuse the session's startup commands if we can still
                    // find the owning session in the DB; otherwise pass
                    // empty.
                    let cmds = self
                        .db
                        .list_sessions()
                        .ok()
                        .and_then(|list| {
                            list.into_iter()
                                .find(|s| s.id == Some(a.session_id()))
                                .map(|s| s.config.startup_commands)
                        })
                        .unwrap_or_default();
                    let outcome = self.open_trace(&path, &cmds);
                    if let Screen::Analysis(a) = &mut self.screen {
                        match outcome {
                            Ok(_) => a.set_status("opened in browser".into()),
                            Err(e) => a.set_error(format!("open failed: {e}")),
                        }
                    }
                }
                AnalysisAction::None => {}
            },
            Screen::ConfigEditor(e) => {
                match e.on_key(key) {
                    EditorAction::Cancel => {
                        self.return_from_editor();
                    }
                    EditorAction::Save(new_config) => {
                        self.save_editor_config(&new_config);
                        self.return_from_editor();
                    }
                    EditorAction::None => {}
                }
            }
            Screen::ConfigList(cl) => match cl.on_key(key) {
                ConfigListAction::Back => self.return_to_sessions_list(),
                ConfigListAction::Edit(id, name, config) => {
                    self.editor_context = Some(EditorContext::SavedConfig(id));
                    let editor = ConfigEditorScreen::new(Some(id), name, &config);
                    self.screen = Screen::ConfigEditor(editor);
                }
                ConfigListAction::CreateNew(name) => {
                    self.editor_context = Some(EditorContext::NewConfig(name.clone()));
                    let editor = ConfigEditorScreen::new(
                        None,
                        name,
                        &crate::perfetto::TraceConfig::default(),
                    );
                    self.screen = Screen::ConfigEditor(editor);
                }
                ConfigListAction::Import => {
                    self.screen =
                        Screen::ConfigImport(ConfigImportScreen::new());
                }
                ConfigListAction::None => {}
            },
            Screen::ConfigImport(ci) => match ci.on_key(key) {
                ImportAction::Cancel => {
                    self.screen =
                        Screen::ConfigList(ConfigListScreen::new(self.db.clone()));
                }
                ImportAction::Save { name, textproto } => {
                    let mut config = crate::perfetto::TraceConfig::default();
                    config.custom_textproto = Some(textproto);
                    match self.db.create_config(&name, &config) {
                        Ok(_) => tracing::info!(name, "imported config"),
                        Err(e) => tracing::error!(?e, "failed to save imported config"),
                    }
                    self.screen =
                        Screen::ConfigList(ConfigListScreen::new(self.db.clone()));
                }
                ImportAction::None => {}
            },
            Screen::CommandSetList(cl) => match cl.on_key(key) {
                CommandSetListAction::Back => self.return_to_sessions_list(),
                CommandSetListAction::Edit(id, name, cmds) => {
                    self.cmd_editor_context = Some(CmdSetEditorContext::Existing(id));
                    self.screen = Screen::CommandSetEditor(
                        CommandSetEditorScreen::new(name, cmds),
                    );
                }
                CommandSetListAction::CreateNew(name) => {
                    self.cmd_editor_context = Some(CmdSetEditorContext::New(name.clone()));
                    self.screen = Screen::CommandSetEditor(
                        CommandSetEditorScreen::new(name, Vec::new()),
                    );
                }
                CommandSetListAction::None => {}
            },
            Screen::CommandSetEditor(ce) => match ce.on_key(key) {
                CmdEditorAction::Cancel => {
                    self.cmd_editor_context = None;
                    self.screen =
                        Screen::CommandSetList(CommandSetListScreen::new(self.db.clone()));
                }
                CmdEditorAction::Save(cmds) => {
                    match self.cmd_editor_context.take() {
                        Some(CmdSetEditorContext::Existing(id)) => {
                            if let Err(e) = self.db.update_command_set(id, &cmds) {
                                tracing::error!(?e, "save command set failed");
                            }
                        }
                        Some(CmdSetEditorContext::New(name)) => {
                            if let Err(e) = self.db.create_command_set(&name, &cmds) {
                                tracing::error!(?e, "create command set failed");
                            }
                        }
                        None => {}
                    }
                    self.screen =
                        Screen::CommandSetList(CommandSetListScreen::new(self.db.clone()));
                }
                CmdEditorAction::None => {}
            },
            Screen::ThemePicker(tp) => match tp.on_key(key) {
                ThemePickerAction::Back => self.return_to_sessions_list(),
                ThemePickerAction::Selected(name) => {
                    let _ = self.db.set_setting("theme", &name);
                    self.return_to_sessions_list();
                }
                ThemePickerAction::None => {}
            },
            Screen::CloudProviders(cp) => match cp.on_key(key) {
                ProviderAction::Back => {
                    // Refresh the cloud provider in case the default changed.
                    self.cloud_provider = cloud::default_provider(&self.db);
                    self.return_to_sessions_list();
                }
                ProviderAction::None => {}
            },
        }
    }

    fn save_editor_config(&self, config: &crate::perfetto::TraceConfig) {
        match &self.editor_context {
            Some(EditorContext::Session(id)) => {
                if let Err(e) = self.save_session_config(*id, config) {
                    tracing::error!(?e, "failed to save session config");
                }
            }
            Some(EditorContext::SavedConfig(id)) => {
                if let Err(e) = self.db.update_config(*id, config) {
                    tracing::error!(?e, "failed to update saved config");
                }
            }
            Some(EditorContext::NewConfig(name)) => {
                match self.db.create_config(name, config) {
                    Ok(_) => tracing::info!(name, "saved new config"),
                    Err(e) => tracing::error!(?e, "failed to create config"),
                }
            }
            None => {
                tracing::warn!("no editor context — save discarded");
            }
        }
    }

    fn return_from_editor(&mut self) {
        let ctx = self.editor_context.take();
        match ctx {
            Some(EditorContext::Session(id)) => self.return_to_detail(Some(id)),
            Some(EditorContext::SavedConfig(_)) | Some(EditorContext::NewConfig(_)) => {
                self.screen = Screen::ConfigList(ConfigListScreen::new(self.db.clone()));
            }
            None => self.return_to_sessions_list(),
        }
    }

    fn save_session_config(&self, id: i64, config: &crate::perfetto::TraceConfig) -> Result<()> {
        self.db.update_session_config(id, config)?;
        if let Some(session) = self
            .db
            .list_sessions()?
            .into_iter()
            .find(|s| s.id == Some(id))
        {
            session.ensure_filesystem()?;
        }
        Ok(())
    }

    fn return_to_detail(&mut self, session_id: Option<i64>) {
        let session = session_id.and_then(|id| {
            self.db
                .list_sessions()
                .ok()
                .and_then(|mut list| list.drain(..).find(|s| s.id == Some(id)))
        });
        match session {
            Some(s) => {
                self.screen = Screen::SessionDetail(SessionDetailScreen::new(s, &self.db, self.cloud_provider.id(), self.cloud_provider.name()))
            }
            None => self.return_to_sessions_list(),
        }
    }

    fn return_to_sessions_list(&mut self) {
        self.sessions_list.reload(&self.db);
        self.screen = Screen::SessionsList;
    }

    /// Check auth and either start the upload or kick off OAuth first.
    fn initiate_upload(&mut self, session: Session, scope: UploadScope, provider: Arc<dyn CloudProvider>) {
        let db = self.db.clone();
        let tx = self.require_tx();

        // Check auth synchronously via a spawned task; if not authenticated,
        // run the OAuth flow, then the pending_upload will be retried.
        self.pending_upload = Some((session.clone(), scope.clone(), provider.clone()));

        tokio::spawn(async move {
            if provider.is_authenticated(&db).await {
                // Already authed — signal ready (the pending_upload will be consumed).
                let _ = tx.send(AppEvent::CloudAuthResult(Ok(provider.id().to_string())));
            } else {
                // Need to authenticate first.
                match provider.authenticate(&db).await {
                    Ok(()) => {
                        let _ = tx.send(AppEvent::CloudAuthResult(Ok(provider.id().to_string())));
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::CloudAuthResult(Err(format!("{e:#}"))));
                    }
                }
            }
        });

        // Transition UI to uploading state.
        if let Screen::SessionDetail(d) = &mut self.screen {
            d.enter_uploading();
        }
    }

    /// Actually spawn the upload task (called after auth succeeds).
    fn start_upload(&mut self, session: Session, scope: UploadScope, provider: Arc<dyn CloudProvider>) {
        let db = self.db.clone();
        let tx = self.require_tx();
        let cancel = Cancel::new();
        self.upload_cancel = Some(cancel.clone());

        // Transition UI.
        if let Screen::SessionDetail(d) = &mut self.screen {
            d.set_upload_cancel(cancel.clone());
            d.enter_uploading();
        }

        let traces = match &scope {
            UploadScope::SingleTrace(trace_id) => {
                let tid = *trace_id;
                session.id.and_then(|sid| {
                    self.db.list_traces(sid).ok().map(|list| {
                        list.into_iter().filter(|t| t.id == tid).collect::<Vec<_>>()
                    })
                }).unwrap_or_default()
            }
            UploadScope::AllTraces => {
                session.id.and_then(|sid| {
                    self.db.list_traces(sid).ok()
                }).unwrap_or_default()
            }
        };

        tokio::spawn(async move {
            let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();

            // Forward progress events to the app event bus.
            let tx_progress = tx.clone();
            tokio::spawn(async move {
                while let Some(p) = progress_rx.recv().await {
                    if tx_progress.send(AppEvent::CloudUploadProgress(p)).is_err() {
                        break;
                    }
                }
            });

            let result = cloud::upload::upload_traces(
                provider.as_ref(),
                &db,
                &session,
                &traces,
                &progress_tx,
                &cancel,
            )
            .await;

            drop(progress_tx);
            let _ = tx.send(AppEvent::CloudUploadDone(result.map_err(|e| format!("{e:#}"))));
        });
    }

    fn require_tx(&self) -> UnboundedSender<AppEvent> {
        self.event_tx
            .clone()
            .expect("event_tx initialized in run()")
    }
}
