use std::path::Path;

use anyhow::Result;
use crossterm::execute;
use crossterm::event::{KeyEvent, KeyEventKind};
use crossterm::terminal::SetTitle;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Paths;
use crate::db::Database;
use crate::tui::theme;
use crate::tui::Tui;
use crate::tui::event::{self, AppEvent};
use crate::tui::screens::capture::{CaptureAction, CaptureScreen};
use crate::tui::screens::command_set_editor::{CmdEditorAction, CommandSetEditorScreen};
use crate::tui::screens::command_set_list::{CommandSetListAction, CommandSetListScreen};
use crate::tui::screens::config_editor::{ConfigEditorScreen, EditorAction};
use crate::tui::screens::config_import::{ConfigImportScreen, ImportAction};
use crate::tui::screens::config_list::{ConfigListAction, ConfigListScreen};
use crate::tui::screens::device_picker::{DevicePickerScreen, PickerAction};
use crate::tui::screens::new_session::{NewSessionScreen, WizardAction};
use crate::tui::screens::session_detail::{DetailAction, SessionDetailScreen};
use crate::tui::screens::sessions_list::{SessionsAction, SessionsListScreen};
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
    DevicePicker(DevicePickerScreen),
    NewSession(NewSessionScreen),
    SessionDetail(SessionDetailScreen),
    ConfigEditor(ConfigEditorScreen),
    ConfigList(ConfigListScreen),
    ConfigImport(ConfigImportScreen<'static>),
    CommandSetList(CommandSetListScreen),
    CommandSetEditor(CommandSetEditorScreen),
    Capture(CaptureScreen),
}

pub struct App {
    db: Database,
    paths: Paths,
    should_quit: bool,
    screen: Screen,
    sessions_list: SessionsListScreen,
    event_tx: Option<UnboundedSender<AppEvent>>,
    ui_server: Option<UiServer>,
    /// (serial, display label) for the device shown in the header.
    active_device: Option<(String, String)>,
    /// What opened the config editor — controls where save routes to.
    editor_context: Option<EditorContext>,
    /// What opened the command set editor.
    cmd_editor_context: Option<CmdSetEditorContext>,
}

impl App {
    pub fn new(db: Database, paths: Paths) -> Self {
        let sessions_list = SessionsListScreen::new(&db);

        // Restore persisted active device, falling back to the most-recently-seen.
        let active_device = {
            let known = db.list_known_devices().unwrap_or_default();
            let saved = db.get_setting("active_device").ok().flatten();
            let rec = saved
                .as_deref()
                .and_then(|s| known.iter().find(|d| d.serial == s))
                .or_else(|| known.first());
            rec.map(|d| {
                let label = d
                    .nickname
                    .clone()
                    .unwrap_or_else(|| d.model.clone().unwrap_or_else(|| d.serial.clone()));
                (d.serial.clone(), label)
            })
        };

        Self {
            db,
            paths,
            should_quit: false,
            screen: Screen::SessionsList,
            sessions_list,
            event_tx: None,
            ui_server: None,
            active_device,
            editor_context: None,
            cmd_editor_context: None,
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
            Screen::DevicePicker(_) => "Perfetto CLI — Devices".to_string(),
            Screen::NewSession(_) => "Perfetto CLI — New Session".to_string(),
            Screen::SessionDetail(d) => format!("Perfetto CLI — {}", d.session().name),
            Screen::ConfigEditor(_) => "Perfetto CLI — Config".to_string(),
            Screen::ConfigList(_) => "Perfetto CLI — Configurations".to_string(),
            Screen::ConfigImport(_) => "Perfetto CLI — Import Config".to_string(),
            Screen::CommandSetList(_) => "Perfetto CLI — Startup Commands".to_string(),
            Screen::CommandSetEditor(_) => "Perfetto CLI — Edit Commands".to_string(),
            Screen::Capture(_) => "Perfetto CLI — Capturing".to_string(),
        };
        let _ = execute!(std::io::stdout(), SetTitle(title));

        match &mut self.screen {
            Screen::SessionsList => self.sessions_list.render(frame),
            Screen::DevicePicker(p) => p.render(frame),
            Screen::NewSession(w) => w.render(frame),
            Screen::SessionDetail(d) => d.render(frame),
            Screen::ConfigEditor(e) => e.render(frame),
            Screen::ConfigList(cl) => cl.render(frame),
            Screen::ConfigImport(ci) => ci.render(frame),
            Screen::CommandSetList(cl) => cl.render(frame),
            Screen::CommandSetEditor(ce) => ce.render(frame),
            Screen::Capture(c) => c.render(frame),
        }

        // Render active device label inside the header box, right-aligned on
        // the subtitle row (row 2 of the 5-row header).
        if let Some((_, label)) = &self.active_device {
            let area = frame.area();
            let device_line = Line::from(vec![
                Span::styled("📱 ", theme::hint()),
                Span::styled(
                    label.as_str(),
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]);
            let w = device_line.width() as u16;
            // Only render if there's room alongside the subtitle text.
            if w + 30 < area.width {
                let x = area.width - w - 1; // inside right border
                frame.render_widget(device_line, Rect::new(x, 2, w, 1));
            }
        }
    }

    fn handle_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            AppEvent::DevicesLoaded(result) => match &mut self.screen {
                Screen::DevicePicker(p) => p.on_devices_loaded(result),
                Screen::NewSession(w) => w.on_devices_loaded(result),
                _ => {}
            },
            AppEvent::DeviceInfoLoaded(result) => match &mut self.screen {
                Screen::DevicePicker(p) => p.on_device_info_loaded(result),
                Screen::NewSession(w) => w.on_device_info_loaded(result),
                _ => {}
            },
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
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match &mut self.screen {
            Screen::SessionsList => match self.sessions_list.on_key(&self.db, key) {
                SessionsAction::Quit => self.should_quit = true,
                SessionsAction::OpenDevicePicker => {
                    let tx = self.require_tx();
                    self.screen =
                        Screen::DevicePicker(DevicePickerScreen::new(self.db.clone(), tx));
                }
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
                            session, &self.db,
                        ));
                    }
                }
                SessionsAction::None => {}
            },
            Screen::DevicePicker(p) => match p.on_key(key) {
                PickerAction::Back => self.return_to_sessions_list(),
                PickerAction::Selected { serial, label } => {
                    tracing::info!(serial, "device selected");
                    let _ = self.db.set_setting("active_device", &serial);
                    self.active_device = Some((serial, label));
                    self.return_to_sessions_list();
                }
                PickerAction::None => {}
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
                            session, &self.db,
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
                DetailAction::None => {}
            },
            Screen::Capture(c) => match c.on_key(key) {
                CaptureAction::Back => {
                    let session_id = c.session_id();
                    self.return_to_detail(Some(session_id));
                }
                CaptureAction::None => {}
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
        }
    }

    fn save_editor_config(&mut self, config: &crate::perfetto::TraceConfig) {
        let ctx = self.editor_context.take();
        match ctx {
            Some(EditorContext::Session(id)) => {
                if let Err(e) = self.save_session_config(id, config) {
                    tracing::error!(?e, "failed to save session config");
                }
            }
            Some(EditorContext::SavedConfig(id)) => {
                if let Err(e) = self.db.update_config(id, config) {
                    tracing::error!(?e, "failed to update saved config");
                }
            }
            Some(EditorContext::NewConfig(name)) => {
                match self.db.create_config(&name, config) {
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
                self.screen = Screen::SessionDetail(SessionDetailScreen::new(s, &self.db))
            }
            None => self.return_to_sessions_list(),
        }
    }

    fn return_to_sessions_list(&mut self) {
        self.sessions_list.reload(&self.db);
        self.screen = Screen::SessionsList;
    }

    fn require_tx(&self) -> UnboundedSender<AppEvent> {
        self.event_tx
            .clone()
            .expect("event_tx initialized in run()")
    }
}
