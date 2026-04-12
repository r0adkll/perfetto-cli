use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::perfetto::config::ATRACE_CATEGORIES;
use crate::perfetto::{TraceConfig, textproto};
use crate::tui::chrome;
use crate::tui::text_input;
use crate::tui::theme;

// ---------------------------------------------------------------------------
// Probe groups — match the sections from the perfetto recorder reference
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ProbeGroup {
    Cpu,
    Gpu,
    Power,
    Memory,
    Android,
    Advanced,
}

impl ProbeGroup {
    const ALL: [ProbeGroup; 6] = [
        Self::Cpu,
        Self::Gpu,
        Self::Power,
        Self::Memory,
        Self::Android,
        Self::Advanced,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Gpu => "GPU",
            Self::Power => "Power",
            Self::Memory => "Memory",
            Self::Android => "Android Apps & Svcs",
            Self::Advanced => "Advanced",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Cpu => "Usage, scheduling, frequency, syscalls",
            Self::Gpu => "Frequency, memory, work period",
            Self::Power => "Battery drain, power rails, board voltages",
            Self::Memory => "Meminfo, high-freq events, LMK, process stats",
            Self::Android => "Atrace categories, logcat, frame timeline",
            Self::Advanced => "Ftrace config, kernel symbols",
        }
    }

    /// Sub-option labels displayed when the group is expanded.
    fn sub_options(self) -> &'static [&'static str] {
        match self {
            Self::Cpu => &[
                "Coarse CPU usage",
                "Scheduling details",
                "CPU frequency & idle",
                "Syscalls",
            ],
            Self::Gpu => &["GPU frequency", "GPU memory", "GPU work period"],
            Self::Power => &["Battery drain & power rails", "Board voltages"],
            Self::Memory => &[
                "Kernel meminfo",
                "High-freq memory events",
                "Low memory killer",
                "Per-process stats",
            ],
            Self::Android => &[], // atrace categories + logcat + frame timeline handled specially
            Self::Advanced => &[
                "Resolve kernel symbols",
                "Disable generic events",
            ],
        }
    }

    /// Descriptions for each sub-option, parallel to sub_options.
    fn sub_descriptions(self) -> &'static [&'static str] {
        match self {
            Self::Cpu => &[
                "Poll CPU time and fork stats via linux.sys_stats",
                "Context switches, wakeups, blocked reasons",
                "Frequency changes, C-state transitions, polling",
                "Raw syscall enter/exit — high overhead",
            ],
            Self::Gpu => &[
                "GPU frequency change events",
                "GPU memory tracking + android.gpu.memory",
                "GPU work period events",
            ],
            Self::Power => &[
                "Battery counters + power rail polling via android.power",
                "Regulator voltages, clock enables/disables",
            ],
            Self::Memory => &[
                "Poll kernel meminfo via linux.sys_stats",
                "mm_event, rss_stat, ion/dmabuf ftrace events",
                "LMK kills + OOM score adjustments",
                "Per-process memory stats polling",
            ],
            Self::Android => &[],
            Self::Advanced => &[
                "Resolve kernel symbols in ftrace events",
                "Filter out generic ftrace events (recommended)",
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// Editor items
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum EditorItem {
    SectionHeader(&'static str),
    NumberField { label: &'static str, target: NumberTarget },
    CycleField { label: &'static str },
    Toggle { label: &'static str, desc: &'static str, target: ToggleTarget },
    TextField { label: &'static str, target: TextTarget },
    GroupHeader { group: ProbeGroup },
    SubToggle { group: ProbeGroup, index: usize, label: &'static str, desc: &'static str },
    SubNumberField { label: &'static str, target: NumberTarget },
    SubTextField { label: &'static str, target: TextTarget },
    AtraceCategory { tag: &'static str, description: &'static str },
    // Android-section specific toggles for logcat sub-options
    AndroidToggle { label: &'static str, target: AndroidToggleTarget },
}

impl EditorItem {
    fn is_selectable(&self) -> bool {
        !matches!(self, EditorItem::SectionHeader(_))
    }
}

#[derive(Debug, Clone, Copy)]
enum NumberTarget {
    Duration,
    Buffer,
    CpuCoarsePoll,
    CpuFreqPoll,
    PowerBatteryPoll,
    MeminfoPoll,
    ProcessStatsPoll,
}

#[derive(Debug, Clone, Copy)]
enum ToggleTarget {
    ColdStart,
    AutoOpen,
    ComposeTracing,
}

#[derive(Debug, Clone, Copy)]
enum AndroidToggleTarget {
    Logcat,
    LogCrash,
    LogDefault,
    LogEvents,
    LogKernel,
    LogSystem,
    FrameTimeline,
}

#[derive(Debug, Clone, Copy)]
enum TextTarget {
    LaunchActivity,
    AtraceApps,
    ExtraFtraceEvents,
}

// ---------------------------------------------------------------------------
// Editor state
// ---------------------------------------------------------------------------

pub struct ConfigEditorScreen {
    session_id: Option<i64>,
    session_name: String,
    config: TraceConfig,
    expanded: HashSet<ProbeGroup>,
    items: Vec<EditorItem>,
    cursor: usize,
    scroll_offset: usize,
    preview_scroll: u16,
    error: Option<String>,
    editing: Option<String>,
}

pub enum EditorAction {
    None,
    Cancel,
    Save(TraceConfig),
}

impl ConfigEditorScreen {
    pub fn new(session_id: Option<i64>, session_name: String, config: &TraceConfig) -> Self {
        let mut screen = Self {
            session_id,
            session_name,
            config: config.clone(),
            expanded: HashSet::new(),
            items: Vec::new(),
            cursor: 0,
            scroll_offset: 0,
            preview_scroll: 0,
            error: None,
            editing: None,
        };
        screen.rebuild_items();
        screen
    }

    pub fn session_id(&self) -> Option<i64> {
        self.session_id
    }

    // -----------------------------------------------------------------------
    // Item list
    // -----------------------------------------------------------------------

    fn rebuild_items(&mut self) {
        self.items.clear();

        self.items.push(EditorItem::SectionHeader("── Recording ──"));
        self.items.push(EditorItem::NumberField {
            label: "Duration (ms)",
            target: NumberTarget::Duration,
        });
        self.items.push(EditorItem::NumberField {
            label: "Buffer (KB)",
            target: NumberTarget::Buffer,
        });
        self.items.push(EditorItem::CycleField { label: "Fill policy" });
        self.items.push(EditorItem::Toggle {
            label: "Cold start",
            desc: "force-stop + restart the app for a clean startup trace",
            target: ToggleTarget::ColdStart,
        });
        self.items.push(EditorItem::Toggle {
            label: "Auto-open",
            desc: "open in ui.perfetto.dev when capture finishes",
            target: ToggleTarget::AutoOpen,
        });
        self.items.push(EditorItem::Toggle {
            label: "Compose tracing",
            desc: "enable Jetpack Compose recomposition events",
            target: ToggleTarget::ComposeTracing,
        });
        self.items.push(EditorItem::TextField {
            label: "Launch activity",
            target: TextTarget::LaunchActivity,
        });

        self.items.push(EditorItem::SectionHeader("── Probes ──"));
        for group in ProbeGroup::ALL {
            self.items.push(EditorItem::GroupHeader { group });
            if !self.expanded.contains(&group) {
                continue;
            }

            match group {
                ProbeGroup::Android => {
                    // Atrace categories
                    for &(tag, description) in ATRACE_CATEGORIES {
                        self.items.push(EditorItem::AtraceCategory { tag, description });
                    }
                    self.items.push(EditorItem::SubTextField {
                        label: "Atrace apps",
                        target: TextTarget::AtraceApps,
                    });
                    // Logcat
                    self.items.push(EditorItem::AndroidToggle {
                        label: "Logcat",
                        target: AndroidToggleTarget::Logcat,
                    });
                    if self.config.android.logcat {
                        for (label, target) in [
                            ("  Crash", AndroidToggleTarget::LogCrash),
                            ("  Default", AndroidToggleTarget::LogDefault),
                            ("  Events", AndroidToggleTarget::LogEvents),
                            ("  Kernel", AndroidToggleTarget::LogKernel),
                            ("  System", AndroidToggleTarget::LogSystem),
                        ] {
                            self.items.push(EditorItem::AndroidToggle { label, target });
                        }
                    }
                    // Frame timeline
                    self.items.push(EditorItem::AndroidToggle {
                        label: "Frame timeline",
                        target: AndroidToggleTarget::FrameTimeline,
                    });
                }
                _ => {
                    let subs = group.sub_options();
                    let descs = group.sub_descriptions();
                    for (i, &label) in subs.iter().enumerate() {
                        let desc = descs.get(i).copied().unwrap_or("");
                        self.items.push(EditorItem::SubToggle {
                            group,
                            index: i,
                            label,
                            desc,
                        });
                        // Poll interval fields after the relevant toggle
                        match (group, i) {
                            (ProbeGroup::Cpu, 0) if self.config.cpu.coarse_usage => {
                                self.items.push(EditorItem::SubNumberField {
                                    label: "Poll interval (ms)",
                                    target: NumberTarget::CpuCoarsePoll,
                                });
                            }
                            (ProbeGroup::Cpu, 2) if self.config.cpu.freq_idle => {
                                self.items.push(EditorItem::SubNumberField {
                                    label: "Poll interval (ms)",
                                    target: NumberTarget::CpuFreqPoll,
                                });
                            }
                            (ProbeGroup::Power, 0) if self.config.power.battery_drain => {
                                self.items.push(EditorItem::SubNumberField {
                                    label: "Poll interval (ms)",
                                    target: NumberTarget::PowerBatteryPoll,
                                });
                            }
                            (ProbeGroup::Memory, 0) if self.config.memory.kernel_meminfo => {
                                self.items.push(EditorItem::SubNumberField {
                                    label: "Poll interval (ms)",
                                    target: NumberTarget::MeminfoPoll,
                                });
                            }
                            (ProbeGroup::Memory, 3) if self.config.memory.per_process_stats => {
                                self.items.push(EditorItem::SubNumberField {
                                    label: "Poll interval (ms)",
                                    target: NumberTarget::ProcessStatsPoll,
                                });
                            }
                            _ => {}
                        }
                    }
                    // Advanced extra ftrace events
                    if group == ProbeGroup::Advanced {
                        self.items.push(EditorItem::SubTextField {
                            label: "Extra ftrace events",
                            target: TextTarget::ExtraFtraceEvents,
                        });
                    }
                }
            }
        }

        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        if self.items.is_empty() {
            self.cursor = 0;
            return;
        }
        if self.cursor >= self.items.len() {
            self.cursor = self.items.len() - 1;
        }
        if !self.items[self.cursor].is_selectable() {
            self.move_cursor(1);
        }
    }

    // -----------------------------------------------------------------------
    // Key handling
    // -----------------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) -> EditorAction {
        if key.kind != KeyEventKind::Press {
            return EditorAction::None;
        }

        if self.editing.is_some() {
            return self.handle_edit_key(key);
        }

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => return EditorAction::Cancel,
            (KeyCode::Char('s'), m) if m.contains(KeyModifiers::CONTROL) => {
                return EditorAction::Save(self.config.clone());
            }
            (KeyCode::Down | KeyCode::Char('j'), _) | (KeyCode::Tab, _) => {
                self.move_cursor(1);
                return EditorAction::None;
            }
            (KeyCode::Up | KeyCode::Char('k'), _) | (KeyCode::BackTab, _) => {
                self.move_cursor(-1);
                return EditorAction::None;
            }
            (KeyCode::Char('['), _) => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
                return EditorAction::None;
            }
            (KeyCode::Char(']'), _) => {
                self.preview_scroll = self.preview_scroll.saturating_add(1);
                return EditorAction::None;
            }
            _ => {}
        }

        let item = self.items[self.cursor].clone();
        match item {
            EditorItem::CycleField { .. } => self.handle_cycle(key),
            EditorItem::Toggle { target, .. } => self.handle_toggle(target),
            EditorItem::GroupHeader { group } => self.handle_group_key(key, group),
            EditorItem::SubToggle { group, index, .. } => self.handle_sub_toggle(key, group, index),
            EditorItem::NumberField { target, .. } => {
                self.start_editing_number(target);
            }
            EditorItem::SubNumberField { target, .. } => match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    if let Some(group) = self.enclosing_group() {
                        self.collapse_group(group);
                    }
                }
                _ => self.start_editing_number(target),
            },
            EditorItem::TextField { target, .. } => {
                self.start_editing_text(target);
            }
            EditorItem::SubTextField { target, .. } => match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    if let Some(group) = self.enclosing_group() {
                        self.collapse_group(group);
                    }
                }
                _ => self.start_editing_text(target),
            },
            EditorItem::AtraceCategory { tag, .. } => match key.code {
                KeyCode::Char(' ') | KeyCode::Enter => {
                    let s = tag.to_string();
                    if !self.config.atrace_categories.remove(&s) {
                        self.config.atrace_categories.insert(s);
                    }
                }
                KeyCode::Left | KeyCode::Char('h') => self.collapse_group(ProbeGroup::Android),
                _ => {}
            },
            EditorItem::AndroidToggle { target, .. } => match key.code {
                KeyCode::Char(' ') | KeyCode::Enter => self.handle_android_toggle(target),
                KeyCode::Left | KeyCode::Char('h') => self.collapse_group(ProbeGroup::Android),
                _ => {}
            },
            _ => {}
        }

        EditorAction::None
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> EditorAction {
        let Some(ref mut buffer) = self.editing else {
            return EditorAction::None;
        };
        match text_input::apply(buffer, &key) {
            text_input::TextAction::Submit | text_input::TextAction::Cancel => {
                self.commit_edit();
            }
            _ => {}
        }
        EditorAction::None
    }

    fn start_editing_number(&mut self, target: NumberTarget) {
        self.editing = Some(self.number_value(target).to_string());
    }

    fn start_editing_text(&mut self, target: TextTarget) {
        self.editing = Some(match target {
            TextTarget::LaunchActivity => self.config.launch_activity.clone().unwrap_or_default(),
            TextTarget::AtraceApps => self.config.atrace_apps.join(", "),
            TextTarget::ExtraFtraceEvents => self.config.advanced.extra_ftrace_events.join(", "),
        });
    }

    fn commit_edit(&mut self) {
        let Some(buffer) = self.editing.take() else { return };
        let item = self.items[self.cursor].clone();
        match item {
            EditorItem::NumberField { target, .. } | EditorItem::SubNumberField { target, .. } => {
                if let Ok(v) = buffer.trim().parse::<u32>() {
                    self.set_number_value(target, v);
                }
            }
            EditorItem::TextField { target, .. } | EditorItem::SubTextField { target, .. } => {
                let trimmed = buffer.trim();
                match target {
                    TextTarget::LaunchActivity => {
                        self.config.launch_activity =
                            if trimmed.is_empty() { None } else { Some(trimmed.into()) };
                    }
                    TextTarget::AtraceApps => {
                        self.config.atrace_apps = split_csv(trimmed);
                    }
                    TextTarget::ExtraFtraceEvents => {
                        self.config.advanced.extra_ftrace_events = split_csv(trimmed);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_cycle(&mut self, key: KeyEvent) {
        if matches!(
            key.code,
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter
                | KeyCode::Char('h') | KeyCode::Char('l')
        ) {
            self.config.fill_policy = self.config.fill_policy.cycle();
        }
    }

    fn handle_toggle(&mut self, target: ToggleTarget) {
        let slot = match target {
            ToggleTarget::ColdStart => &mut self.config.cold_start,
            ToggleTarget::AutoOpen => &mut self.config.auto_open,
            ToggleTarget::ComposeTracing => &mut self.config.compose_tracing,
        };
        *slot = !*slot;
    }

    fn handle_android_toggle(&mut self, target: AndroidToggleTarget) {
        let slot = match target {
            AndroidToggleTarget::Logcat => &mut self.config.android.logcat,
            AndroidToggleTarget::LogCrash => &mut self.config.android.log_crash,
            AndroidToggleTarget::LogDefault => &mut self.config.android.log_default,
            AndroidToggleTarget::LogEvents => &mut self.config.android.log_events,
            AndroidToggleTarget::LogKernel => &mut self.config.android.log_kernel,
            AndroidToggleTarget::LogSystem => &mut self.config.android.log_system,
            AndroidToggleTarget::FrameTimeline => &mut self.config.android.frame_timeline,
        };
        *slot = !*slot;
        // Expand/collapse logcat sub-options
        if matches!(target, AndroidToggleTarget::Logcat) {
            self.rebuild_items();
        }
    }

    fn handle_group_key(&mut self, key: KeyEvent, group: ProbeGroup) {
        match key.code {
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                if self.expanded.contains(&group) {
                    self.expanded.remove(&group);
                } else {
                    self.expanded.insert(group);
                }
                self.rebuild_items();
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if self.expanded.contains(&group) {
                    self.expanded.remove(&group);
                    self.rebuild_items();
                }
            }
            _ => {}
        }
    }

    fn handle_sub_toggle(&mut self, key: KeyEvent, group: ProbeGroup, index: usize) {
        match key.code {
            KeyCode::Char(' ') | KeyCode::Enter => {
                if let Some(slot) = self.sub_toggle_mut(group, index) {
                    *slot = !*slot;
                    self.rebuild_items();
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.collapse_group(group);
            }
            _ => {}
        }
    }

    /// Find the nearest enclosing group by scanning upward from the cursor.
    fn enclosing_group(&self) -> Option<ProbeGroup> {
        for i in (0..=self.cursor).rev() {
            if let EditorItem::GroupHeader { group } = &self.items[i] {
                return Some(*group);
            }
        }
        None
    }

    /// Collapse a group and move the cursor to its header row.
    fn collapse_group(&mut self, group: ProbeGroup) {
        if self.expanded.remove(&group) {
            // Find the group header so the cursor lands there after collapse
            let header_pos = self
                .items
                .iter()
                .position(|item| matches!(item, EditorItem::GroupHeader { group: g } if *g == group));
            self.rebuild_items();
            if let Some(pos) = header_pos {
                self.cursor = pos;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Config field access
    // -----------------------------------------------------------------------

    fn sub_toggle_mut(&mut self, group: ProbeGroup, index: usize) -> Option<&mut bool> {
        Some(match (group, index) {
            (ProbeGroup::Cpu, 0) => &mut self.config.cpu.coarse_usage,
            (ProbeGroup::Cpu, 1) => &mut self.config.cpu.scheduling,
            (ProbeGroup::Cpu, 2) => &mut self.config.cpu.freq_idle,
            (ProbeGroup::Cpu, 3) => &mut self.config.cpu.syscalls,
            (ProbeGroup::Gpu, 0) => &mut self.config.gpu.frequency,
            (ProbeGroup::Gpu, 1) => &mut self.config.gpu.memory,
            (ProbeGroup::Gpu, 2) => &mut self.config.gpu.work_period,
            (ProbeGroup::Power, 0) => &mut self.config.power.battery_drain,
            (ProbeGroup::Power, 1) => &mut self.config.power.board_voltages,
            (ProbeGroup::Memory, 0) => &mut self.config.memory.kernel_meminfo,
            (ProbeGroup::Memory, 1) => &mut self.config.memory.high_freq_events,
            (ProbeGroup::Memory, 2) => &mut self.config.memory.low_memory_killer,
            (ProbeGroup::Memory, 3) => &mut self.config.memory.per_process_stats,
            (ProbeGroup::Advanced, 0) => &mut self.config.advanced.symbolize_ksyms,
            (ProbeGroup::Advanced, 1) => &mut self.config.advanced.disable_generic_events,
            _ => return None,
        })
    }

    fn sub_toggle_value(&self, group: ProbeGroup, index: usize) -> bool {
        match (group, index) {
            (ProbeGroup::Cpu, 0) => self.config.cpu.coarse_usage,
            (ProbeGroup::Cpu, 1) => self.config.cpu.scheduling,
            (ProbeGroup::Cpu, 2) => self.config.cpu.freq_idle,
            (ProbeGroup::Cpu, 3) => self.config.cpu.syscalls,
            (ProbeGroup::Gpu, 0) => self.config.gpu.frequency,
            (ProbeGroup::Gpu, 1) => self.config.gpu.memory,
            (ProbeGroup::Gpu, 2) => self.config.gpu.work_period,
            (ProbeGroup::Power, 0) => self.config.power.battery_drain,
            (ProbeGroup::Power, 1) => self.config.power.board_voltages,
            (ProbeGroup::Memory, 0) => self.config.memory.kernel_meminfo,
            (ProbeGroup::Memory, 1) => self.config.memory.high_freq_events,
            (ProbeGroup::Memory, 2) => self.config.memory.low_memory_killer,
            (ProbeGroup::Memory, 3) => self.config.memory.per_process_stats,
            (ProbeGroup::Advanced, 0) => self.config.advanced.symbolize_ksyms,
            (ProbeGroup::Advanced, 1) => self.config.advanced.disable_generic_events,
            _ => false,
        }
    }

    fn number_value(&self, target: NumberTarget) -> u32 {
        match target {
            NumberTarget::Duration => self.config.duration_ms,
            NumberTarget::Buffer => self.config.buffer_size_kb,
            NumberTarget::CpuCoarsePoll => self.config.cpu.coarse_poll_ms,
            NumberTarget::CpuFreqPoll => self.config.cpu.freq_poll_ms,
            NumberTarget::PowerBatteryPoll => self.config.power.battery_poll_ms,
            NumberTarget::MeminfoPoll => self.config.memory.meminfo_poll_ms,
            NumberTarget::ProcessStatsPoll => self.config.memory.process_poll_ms,
        }
    }

    fn set_number_value(&mut self, target: NumberTarget, v: u32) {
        match target {
            NumberTarget::Duration => self.config.duration_ms = v,
            NumberTarget::Buffer => self.config.buffer_size_kb = v,
            NumberTarget::CpuCoarsePoll => self.config.cpu.coarse_poll_ms = v,
            NumberTarget::CpuFreqPoll => self.config.cpu.freq_poll_ms = v,
            NumberTarget::PowerBatteryPoll => self.config.power.battery_poll_ms = v,
            NumberTarget::MeminfoPoll => self.config.memory.meminfo_poll_ms = v,
            NumberTarget::ProcessStatsPoll => self.config.memory.process_poll_ms = v,
        }
    }

    fn group_has_any_enabled(&self, group: ProbeGroup) -> bool {
        match group {
            ProbeGroup::Cpu => {
                self.config.cpu.coarse_usage
                    || self.config.cpu.scheduling
                    || self.config.cpu.freq_idle
                    || self.config.cpu.syscalls
            }
            ProbeGroup::Gpu => {
                self.config.gpu.frequency
                    || self.config.gpu.memory
                    || self.config.gpu.work_period
            }
            ProbeGroup::Power => {
                self.config.power.battery_drain || self.config.power.board_voltages
            }
            ProbeGroup::Memory => {
                self.config.memory.kernel_meminfo
                    || self.config.memory.high_freq_events
                    || self.config.memory.low_memory_killer
                    || self.config.memory.per_process_stats
            }
            ProbeGroup::Android => {
                !self.config.atrace_categories.is_empty()
                    || self.config.android.logcat
                    || self.config.android.frame_timeline
            }
            ProbeGroup::Advanced => {
                !self.config.advanced.extra_ftrace_events.is_empty()
            }
        }
    }

    // -----------------------------------------------------------------------
    // Cursor
    // -----------------------------------------------------------------------

    fn move_cursor(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as i32;
        let mut pos = self.cursor as i32;
        loop {
            pos = (pos + delta).rem_euclid(len);
            if self.items[pos as usize].is_selectable() {
                break;
            }
        }
        self.cursor = pos as usize;
    }

    // -----------------------------------------------------------------------
    // Render
    // -----------------------------------------------------------------------

    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(chrome::HEADER_HEIGHT),
                Constraint::Min(5),
                Constraint::Length(1),
            ])
            .split(area);

        let header = chrome::app_header(Line::from(vec![
            Span::styled("  ⚙  Config ", theme::title()),
            Span::styled(format!("— {}", self.session_name), theme::hint()),
        ]));
        frame.render_widget(header, rows[0]);

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(rows[1]);

        self.render_form(frame, cols[0]);
        self.render_preview(frame, cols[1]);

        let footer = match &self.error {
            Some(msg) => Line::from(Span::styled(
                format!(" ✗ {msg}"),
                Style::default().fg(theme::ERR),
            )),
            None => {
                if self.editing.is_some() {
                    Line::from(vec![
                        Span::styled(" [Enter]", theme::title()),
                        Span::raw(" save  "),
                        Span::styled("[Esc]", theme::title()),
                        Span::raw(" cancel  "),
                        Span::styled("[Alt-⌫]", theme::title()),
                        Span::raw(" word  "),
                        Span::styled("[Ctrl-U]", theme::title()),
                        Span::raw(" clear"),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled(" [↑/↓]", theme::title()),
                        Span::raw(" move  "),
                        Span::styled("[Space]", theme::title()),
                        Span::raw(" toggle  "),
                        Span::styled("[Enter]", theme::title()),
                        Span::raw(" expand/edit  "),
                        Span::styled("[Ctrl-S]", theme::title()),
                        Span::raw(" save  "),
                        Span::styled("[Esc]", theme::title()),
                        Span::raw(" cancel"),
                    ])
                }
            }
        };
        frame.render_widget(Paragraph::new(footer), rows[2]);
    }

    fn render_form(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let inner_height = area.height.saturating_sub(2) as usize;
        let scroll = {
            let mut s = self.scroll_offset;
            if self.cursor < s { s = self.cursor; }
            else if self.cursor >= s + inner_height { s = self.cursor - inner_height + 1; }
            s
        };
        let end = self.items.len().min(scroll + inner_height);
        let mut lines: Vec<Line> = Vec::with_capacity(end - scroll);
        for idx in scroll..end {
            lines.push(self.render_item(&self.items[idx], idx == self.cursor));
        }
        let form = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Config "));
        frame.render_widget(form, area);
    }

    fn render_item(&self, item: &EditorItem, focused: bool) -> Line<'static> {
        let arrow = if focused && item.is_selectable() {
            Span::styled(" ▶ ", Style::default().fg(theme::ACCENT))
        } else {
            Span::raw("   ")
        };
        let fs = if focused {
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        match item {
            EditorItem::SectionHeader(label) => Line::from(vec![
                Span::raw(" "),
                Span::styled(label.to_string(), Style::default().fg(theme::DIM).add_modifier(Modifier::BOLD)),
            ]),

            EditorItem::NumberField { label, target } => {
                let val = if focused && self.editing.is_some() {
                    format!("{}█", self.editing.as_deref().unwrap_or(""))
                } else {
                    self.number_value(*target).to_string()
                };
                Line::from(vec![arrow, Span::styled(format!("{label:<20}"), theme::hint()), Span::styled(val, fs)])
            }

            EditorItem::CycleField { label } => Line::from(vec![
                arrow,
                Span::styled(format!("{label:<20}"), theme::hint()),
                Span::styled(format!("‹ {} ›", self.config.fill_policy.label()), fs),
            ]),

            EditorItem::Toggle { label, desc, target } => {
                let val = match target {
                    ToggleTarget::ColdStart => self.config.cold_start,
                    ToggleTarget::AutoOpen => self.config.auto_open,
                    ToggleTarget::ComposeTracing => self.config.compose_tracing,
                };
                let icon = if val { "☑" } else { "☐" };
                Line::from(vec![
                    arrow,
                    Span::styled(format!("{icon} {label:<18}"), fs),
                    Span::styled(*desc, theme::hint()),
                ])
            }

            EditorItem::TextField { label, target } => {
                let val = if focused && self.editing.is_some() {
                    format!("{}█", self.editing.as_deref().unwrap_or(""))
                } else {
                    match target {
                        TextTarget::LaunchActivity => self.config.launch_activity.as_deref().unwrap_or("(auto)").into(),
                        _ => String::new(),
                    }
                };
                Line::from(vec![arrow, Span::styled(format!("{label:<20}"), theme::hint()), Span::styled(val, fs)])
            }

            EditorItem::GroupHeader { group } => {
                let expanded = self.expanded.contains(group);
                let has_active = self.group_has_any_enabled(*group);
                let chevron = if expanded { "▼" } else { "▶" };
                let status = if has_active { "active" } else { "—" };
                let ss = if has_active { Style::default().fg(theme::OK) } else { Style::default().fg(theme::DIM) };
                Line::from(vec![
                    arrow,
                    Span::styled(format!("{chevron} {:<20}", group.label()), fs),
                    Span::styled(status, ss),
                    Span::raw("  "),
                    Span::styled(group.description(), theme::hint()),
                ])
            }

            EditorItem::SubToggle { group, index, label, desc } => {
                let val = self.sub_toggle_value(*group, *index);
                let icon = if val { "☑" } else { "☐" };
                let mut spans = vec![
                    Span::raw("      "),
                    Span::styled(format!("{icon} {label:<28}"), fs),
                ];
                if !desc.is_empty() {
                    spans.push(Span::styled(*desc, theme::hint()));
                }
                Line::from(spans)
            }

            EditorItem::SubNumberField { label, target } => {
                let val = if focused && self.editing.is_some() {
                    format!("{}█", self.editing.as_deref().unwrap_or(""))
                } else {
                    self.number_value(*target).to_string()
                };
                Line::from(vec![
                    Span::raw("        "),
                    Span::styled(format!("{label:<26}"), theme::hint()),
                    Span::styled(val, fs),
                ])
            }

            EditorItem::AtraceCategory { tag, description } => {
                let on = self.config.atrace_categories.contains(*tag);
                let icon = if on { "☑" } else { "☐" };
                Line::from(vec![
                    Span::raw("      "),
                    Span::styled(format!("{icon} {tag:<16}"), fs),
                    Span::styled(format!("— {description}"), theme::hint()),
                ])
            }

            EditorItem::AndroidToggle { label, target } => {
                let val = match target {
                    AndroidToggleTarget::Logcat => self.config.android.logcat,
                    AndroidToggleTarget::LogCrash => self.config.android.log_crash,
                    AndroidToggleTarget::LogDefault => self.config.android.log_default,
                    AndroidToggleTarget::LogEvents => self.config.android.log_events,
                    AndroidToggleTarget::LogKernel => self.config.android.log_kernel,
                    AndroidToggleTarget::LogSystem => self.config.android.log_system,
                    AndroidToggleTarget::FrameTimeline => self.config.android.frame_timeline,
                };
                let icon = if val { "☑" } else { "☐" };
                Line::from(vec![
                    Span::raw("      "),
                    Span::styled(format!("{icon} {label}"), fs),
                ])
            }

            EditorItem::SubTextField { label, target, .. } => {
                let val = if focused && self.editing.is_some() {
                    format!("{}█", self.editing.as_deref().unwrap_or(""))
                } else {
                    let csv = match target {
                        TextTarget::AtraceApps => self.config.atrace_apps.join(", "),
                        TextTarget::ExtraFtraceEvents => self.config.advanced.extra_ftrace_events.join(", "),
                        _ => String::new(),
                    };
                    if csv.is_empty() { "(none)".into() } else { csv }
                };
                Line::from(vec![
                    Span::raw("      "),
                    Span::styled(format!("{label:<24}"), theme::hint()),
                    Span::styled(val, fs),
                ])
            }
        }
    }

    fn render_preview(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let txt = textproto::build(&self.config);
        let preview = Paragraph::new(txt)
            .block(Block::default().borders(Borders::ALL).title(" Textproto preview "))
            .scroll((self.preview_scroll, 0))
            .wrap(Wrap { trim: false });
        frame.render_widget(preview, area);
    }
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}
