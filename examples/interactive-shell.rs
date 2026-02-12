use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tape_tui::core::cursor::CURSOR_MARKER;
use tape_tui::runtime::tui::{Command as RuntimeCommand, RuntimeHandle};
use tape_tui::{
    truncate_to_width, visible_width, Component, Focusable, InputEvent, KeyEventType,
    OverlayAnchor, OverlayMargin, OverlayOptions, ProcessTerminal, SizeValue, SurfaceHandle,
    SurfaceInputPolicy, SurfaceKind, SurfaceOptions, TUI,
};

const MAX_OUTPUT_LINES: usize = 240;
const MAX_HANDOFFS: usize = 5;
const OVERLAY_WIDTH_PERCENT: f32 = 92.0;
const OVERLAY_HEIGHT_PERCENT: f32 = 70.0;
const TOTAL_SIMULATION_TICKS: usize = 14;

#[derive(Clone, Copy)]
struct SimulationStep {
    phase: AgentPhase,
    ticks: usize,
    announce: &'static str,
}

const SIMULATION_STEPS: [SimulationStep; 3] = [
    SimulationStep {
        phase: AgentPhase::Planning,
        ticks: 4,
        announce: "planning deterministic changes",
    },
    SimulationStep {
        phase: AgentPhase::Editing,
        ticks: 6,
        announce: "editing files",
    },
    SimulationStep {
        phase: AgentPhase::Testing,
        ticks: 4,
        announce: "running verification checks",
    },
];

fn ansi_wrap(text: &str, prefix: &str, suffix: &str) -> String {
    format!("{prefix}{text}{suffix}")
}

fn dim(text: &str) -> String {
    ansi_wrap(text, "\x1b[2m", "\x1b[22m")
}

fn bold(text: &str) -> String {
    ansi_wrap(text, "\x1b[1m", "\x1b[22m")
}

fn italic(text: &str) -> String {
    ansi_wrap(text, "\x1b[3m", "\x1b[23m")
}

fn green(text: &str) -> String {
    ansi_wrap(text, "\x1b[32m", "\x1b[39m")
}

fn yellow(text: &str) -> String {
    ansi_wrap(text, "\x1b[33m", "\x1b[39m")
}

fn red(text: &str) -> String {
    ansi_wrap(text, "\x1b[31m", "\x1b[39m")
}

fn cyan(text: &str) -> String {
    ansi_wrap(text, "\x1b[36m", "\x1b[39m")
}

fn line_rule(width: usize, ch: char) -> String {
    if width == 0 {
        return String::new();
    }
    ch.to_string().repeat(width)
}

fn shell_border_top(width: usize) -> String {
    match width {
        0 => String::new(),
        1 => dim("│"),
        2 => dim("┌┐"),
        _ => dim(&format!("┌{}┐", "─".repeat(width.saturating_sub(2)))),
    }
}

fn shell_border_bottom(width: usize) -> String {
    match width {
        0 => String::new(),
        1 => dim("│"),
        2 => dim("└┘"),
        _ => dim(&format!("└{}┘", "─".repeat(width.saturating_sub(2)))),
    }
}

fn shell_border_line(content: &str, width: usize) -> String {
    match width {
        0 => String::new(),
        1 => dim("│"),
        2 => dim("││"),
        _ => {
            let inner = pad_to_width(content, width.saturating_sub(2));
            format!("{}{}{}", dim("│"), inner, dim("│"))
        }
    }
}

fn box_shell_lines(lines: Vec<String>, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }

    let mut boxed = Vec::with_capacity(lines.len().saturating_add(2));
    boxed.push(shell_border_top(width));
    for line in lines {
        boxed.push(shell_border_line(&line, width));
    }
    boxed.push(shell_border_bottom(width));
    boxed
}

fn pad_to_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = truncate_to_width(text, width, "", true);
    let current = visible_width(&out);
    if current < width {
        out.push_str(&" ".repeat(width - current));
    }
    out
}

fn format_duration(dur: Duration) -> String {
    if dur.as_secs() >= 60 {
        format!("{}m{:02}s", dur.as_secs() / 60, dur.as_secs() % 60)
    } else {
        format!("{}.{:01}s", dur.as_secs(), dur.subsec_millis() / 100)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionMode {
    Interactive,
    HandsFree,
    Background,
}

impl SessionMode {
    fn label(self) -> &'static str {
        match self {
            SessionMode::Interactive => "interactive",
            SessionMode::HandsFree => "hands-free",
            SessionMode::Background => "background",
        }
    }

    fn colorize(self, text: &str) -> String {
        match self {
            SessionMode::Interactive => green(text),
            SessionMode::HandsFree => yellow(text),
            SessionMode::Background => dim(text),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionState {
    Running,
    Exited { code: i32 },
}

impl SessionState {
    fn chip(self) -> String {
        match self {
            SessionState::Running => green("[RUNNING]"),
            SessionState::Exited { code } => dim(&format!("[EXIT {code}]")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentPhase {
    Planning,
    Editing,
    Testing,
    Done,
    Error,
}

impl AgentPhase {
    fn label(self) -> &'static str {
        match self {
            AgentPhase::Planning => "planning",
            AgentPhase::Editing => "editing",
            AgentPhase::Testing => "testing",
            AgentPhase::Done => "done",
            AgentPhase::Error => "error",
        }
    }

    fn chip(self) -> String {
        match self {
            AgentPhase::Planning => yellow("planning"),
            AgentPhase::Editing => cyan("editing"),
            AgentPhase::Testing => yellow("testing"),
            AgentPhase::Done => green("done"),
            AgentPhase::Error => red("error"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskState {
    Todo,
    Running,
    Blocked,
    Done,
}

impl TaskState {
    fn chip(self) -> String {
        match self {
            TaskState::Todo => dim("todo"),
            TaskState::Running => yellow("running"),
            TaskState::Blocked => red("blocked"),
            TaskState::Done => green("done"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandoffStatus {
    Done,
    Blocked,
    Killed,
}

impl HandoffStatus {
    fn chip(self) -> String {
        match self {
            HandoffStatus::Done => green("done"),
            HandoffStatus::Blocked => red("blocked"),
            HandoffStatus::Killed => yellow("killed"),
        }
    }
}

#[derive(Default)]
struct SimulationAdvance {
    lines: Vec<String>,
    task_state: Option<TaskState>,
    finished: bool,
}

#[derive(Clone, Debug)]
struct SessionSimulation {
    phase: AgentPhase,
    phase_index: usize,
    phase_tick: usize,
    total_tick: usize,
    progress: u8,
}

impl SessionSimulation {
    fn new() -> Self {
        Self {
            phase: AgentPhase::Planning,
            phase_index: 0,
            phase_tick: 0,
            total_tick: 0,
            progress: 0,
        }
    }

    fn advance(&mut self) -> SimulationAdvance {
        if matches!(self.phase, AgentPhase::Done | AgentPhase::Error) {
            return SimulationAdvance::default();
        }

        let mut update = SimulationAdvance::default();

        if self.total_tick == 0 {
            update.task_state = Some(TaskState::Running);
            update.lines.push(format!(
                "[agent] {}",
                SIMULATION_STEPS[self.phase_index].announce
            ));
        }

        let step = SIMULATION_STEPS[self.phase_index];
        self.total_tick = self.total_tick.saturating_add(1);
        self.phase_tick = self.phase_tick.saturating_add(1);
        self.progress = (((self.total_tick * 100) / TOTAL_SIMULATION_TICKS) as u8).min(99);

        if self.phase_tick % 2 == 0 || self.phase_tick == step.ticks {
            update.lines.push(format!(
                "[agent] {}... {}%",
                self.phase.label(),
                self.progress
            ));
        }

        if self.phase_tick >= step.ticks {
            if self.phase_index + 1 < SIMULATION_STEPS.len() {
                self.phase_index += 1;
                self.phase = SIMULATION_STEPS[self.phase_index].phase;
                self.phase_tick = 0;
                update.lines.push(format!(
                    "[agent] {}",
                    SIMULATION_STEPS[self.phase_index].announce
                ));
            } else {
                self.phase = AgentPhase::Done;
                self.progress = 100;
                update.lines.push("[agent] workflow complete".to_string());
                update.task_state = Some(TaskState::Done);
                update.finished = true;
            }
        }

        update
    }

    fn force_error(&mut self, reason: &str) -> Vec<String> {
        self.phase = AgentPhase::Error;
        vec![format!("[agent] error: {reason}")]
    }

    fn plan_overview(&self) -> String {
        match self.phase {
            AgentPhase::Planning => "planning -> editing -> testing -> done".to_string(),
            AgentPhase::Editing => "editing -> testing -> done".to_string(),
            AgentPhase::Testing => "testing -> done".to_string(),
            AgentPhase::Done => "done".to_string(),
            AgentPhase::Error => "error".to_string(),
        }
    }

    fn summary_line(&self) -> String {
        format!(
            "phase={} progress={}%; deterministic timeline active",
            self.phase.label(),
            self.progress
        )
    }
}

struct Session {
    id: usize,
    title: String,
    command: String,
    task_id: usize,
    mode: SessionMode,
    state: SessionState,
    output: VecDeque<String>,
    pending_inputs: VecDeque<String>,
    interrupted: bool,
    last_activity: Instant,
    simulation: SessionSimulation,
}

impl Session {
    fn new(id: usize, task_id: usize, title: String, command: String, mode: SessionMode) -> Self {
        let now = Instant::now();
        let mut output = VecDeque::new();
        output.push_back(format!("[boot] starting {command}"));
        output.push_back(format!("[boot] attached to task #{task_id}"));
        output.push_back("[boot] tip: type help for commands".to_string());
        Self {
            id,
            title,
            command,
            task_id,
            mode,
            state: SessionState::Running,
            output,
            pending_inputs: VecDeque::new(),
            interrupted: false,
            last_activity: now,
            simulation: SessionSimulation::new(),
        }
    }

    fn push_output(&mut self, line: String) {
        self.output.push_back(line);
        while self.output.len() > MAX_OUTPUT_LINES {
            self.output.pop_front();
        }
        self.last_activity = Instant::now();
    }
}

#[derive(Clone)]
struct Task {
    id: usize,
    title: String,
    state: TaskState,
    session_id: Option<usize>,
    last_activity: Instant,
}

impl Task {
    fn new(id: usize, title: String) -> Self {
        Self {
            id,
            title,
            state: TaskState::Todo,
            session_id: None,
            last_activity: Instant::now(),
        }
    }
}

#[derive(Clone, Debug)]
struct HandoffRecord {
    session_title: String,
    task_title: String,
    summary: String,
    status: HandoffStatus,
    changed_files: Vec<String>,
    when: Instant,
    tail: Vec<String>,
}

struct SessionStore {
    sessions: Vec<Session>,
    tasks: Vec<Task>,
    next_session_id: usize,
    next_task_id: usize,
    handoffs: VecDeque<HandoffRecord>,
}

impl SessionStore {
    fn new() -> Self {
        Self {
            sessions: Vec::new(),
            tasks: Vec::new(),
            next_session_id: 1,
            next_task_id: 1,
            handoffs: VecDeque::new(),
        }
    }

    fn create_task_internal(&mut self) -> usize {
        let id = self.next_task_id;
        self.next_task_id += 1;
        let task = Task::new(id, sample_task_title(id));
        self.tasks.push(task);
        id
    }

    fn create_task(&mut self) -> usize {
        self.create_task_internal()
    }

    fn create_session(&mut self, mode: SessionMode) -> usize {
        let session_id = self.next_session_id;
        self.next_session_id += 1;

        let task_id = if let Some(task) = self
            .tasks
            .iter_mut()
            .find(|task| task.state == TaskState::Todo && task.session_id.is_none())
        {
            task.session_id = Some(session_id);
            task.last_activity = Instant::now();
            task.id
        } else {
            let task_id = self.create_task_internal();
            if let Some(task) = self.task_mut(task_id) {
                task.session_id = Some(session_id);
                task.last_activity = Instant::now();
            }
            task_id
        };

        let command = sample_command(session_id);
        let title = format!("Session {session_id}");
        let session = Session::new(session_id, task_id, title, command, mode);
        self.sessions.push(session);
        session_id
    }

    fn session_mut(&mut self, id: usize) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|session| session.id == id)
    }

    fn session(&self, id: usize) -> Option<&Session> {
        self.sessions.iter().find(|session| session.id == id)
    }

    fn task_mut(&mut self, id: usize) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|task| task.id == id)
    }

    fn task(&self, id: usize) -> Option<&Task> {
        self.tasks.iter().find(|task| task.id == id)
    }

    fn enqueue_input(&mut self, id: usize, input: String) {
        if let Some(session) = self.session_mut(id) {
            session.pending_inputs.push_back(input);
        }
    }

    fn set_task_state(&mut self, id: usize, state: TaskState) {
        if let Some(task) = self.task_mut(id) {
            task.state = state;
            task.last_activity = Instant::now();
        }
    }

    fn mark_background(&mut self, id: usize) {
        if let Some(session) = self.session_mut(id) {
            session.mode = SessionMode::Background;
            session.push_output("[system] session moved to background".to_string());
        }
    }

    fn mark_interactive(&mut self, id: usize) {
        if let Some(session) = self.session_mut(id) {
            session.mode = SessionMode::Interactive;
            session.push_output("[system] user took control".to_string());
        }
    }

    fn resume_session(&mut self, id: usize) {
        let mut task_id = None;
        if let Some(session) = self.session_mut(id) {
            if session.state == SessionState::Running {
                session.interrupted = false;
                session.push_output("[system] resumed deterministic worker".to_string());
                task_id = Some(session.task_id);
            }
        }
        if let Some(task_id) = task_id {
            self.set_task_state(task_id, TaskState::Running);
        }
    }

    fn interrupt_session(&mut self, id: usize) {
        if let Some(session) = self.session_mut(id) {
            if session.state == SessionState::Running {
                if session.mode == SessionMode::HandsFree {
                    session.mode = SessionMode::Interactive;
                    session.push_output("[system] user took control".to_string());
                }
                session.interrupted = true;
                session.pending_inputs.clear();
                session.push_output("^C".to_string());
                session.push_output("[system] interrupt signal sent".to_string());
            }
        }
    }

    fn kill_session(&mut self, id: usize, code: i32) {
        let mut task_id = None;
        if let Some(session) = self.session_mut(id) {
            if session.state == SessionState::Running {
                session.state = SessionState::Exited { code };
                session.mode = SessionMode::Background;
                session.push_output(format!("[system] terminated (code {code})"));
                task_id = Some(session.task_id);
            }
        }

        if let Some(task_id) = task_id {
            self.set_task_state(task_id, TaskState::Blocked);
        }
    }

    fn record_handoff(&mut self, id: usize) {
        let Some(session) = self.session(id) else {
            return;
        };
        let Some(task) = self.task(session.task_id) else {
            return;
        };

        let tail = session
            .output
            .iter()
            .rev()
            .take(8)
            .cloned()
            .collect::<Vec<_>>();
        let mut tail = tail;
        tail.reverse();

        let status = match (task.state, session.state) {
            (TaskState::Done, _) => HandoffStatus::Done,
            (_, SessionState::Exited { code: 130 | 137 }) => HandoffStatus::Killed,
            _ => HandoffStatus::Blocked,
        };

        let summary = match status {
            HandoffStatus::Done => format!(
                "completed {} at {}%",
                session.simulation.phase.label(),
                session.simulation.progress
            ),
            HandoffStatus::Killed => "run terminated by presenter control".to_string(),
            HandoffStatus::Blocked => format!(
                "blocked in {} at {}%",
                session.simulation.phase.label(),
                session.simulation.progress
            ),
        };

        let record = HandoffRecord {
            session_title: session.title.clone(),
            task_title: task.title.clone(),
            summary,
            status,
            changed_files: sample_changed_files(task.id),
            when: Instant::now(),
            tail,
        };

        self.handoffs.push_front(record);
        while self.handoffs.len() > MAX_HANDOFFS {
            self.handoffs.pop_back();
        }
    }

    fn snapshot(&self) -> SessionSnapshot {
        let tasks = self
            .tasks
            .iter()
            .map(|task| {
                let (phase, progress) = task
                    .session_id
                    .and_then(|session_id| self.session(session_id))
                    .map(|session| {
                        (
                            Some(session.simulation.phase),
                            Some(session.simulation.progress),
                        )
                    })
                    .unwrap_or((None, None));

                TaskRow {
                    id: task.id,
                    title: task.title.clone(),
                    state: task.state,
                    session_id: task.session_id,
                    phase,
                    progress,
                    last_activity: task.last_activity,
                }
            })
            .collect();

        let sessions = self
            .sessions
            .iter()
            .map(|session| {
                let (task_title, task_state) = self
                    .task(session.task_id)
                    .map(|task| (task.title.clone(), task.state))
                    .unwrap_or_else(|| ("(missing task)".to_string(), TaskState::Blocked));

                SessionRow {
                    id: session.id,
                    title: session.title.clone(),
                    task_title,
                    task_state,
                    mode: session.mode,
                    state: session.state,
                    phase: session.simulation.phase,
                    progress: session.simulation.progress,
                    last_line: session
                        .output
                        .back()
                        .cloned()
                        .unwrap_or_else(|| "(no output yet)".to_string()),
                    last_activity: session.last_activity,
                }
            })
            .collect();

        let handoffs = self
            .handoffs
            .iter()
            .map(|handoff| HandoffRow {
                session_title: handoff.session_title.clone(),
                task_title: handoff.task_title.clone(),
                summary: handoff.summary.clone(),
                status: handoff.status,
                changed_files: handoff.changed_files.clone(),
                when: handoff.when,
                tail: handoff.tail.clone(),
            })
            .collect();

        SessionSnapshot {
            tasks,
            sessions,
            handoffs,
        }
    }
}

fn sample_command(id: usize) -> String {
    const COMMANDS: [&str; 4] = [
        "codex agent --worktree main",
        "codex run --project shell",
        "claude code --session",
        "pi console --interactive",
    ];
    COMMANDS[(id - 1) % COMMANDS.len()].to_string()
}

fn sample_task_title(id: usize) -> String {
    const TITLES: [&str; 6] = [
        "stabilize shell overlay",
        "refactor task dashboard",
        "validate deterministic tick engine",
        "prepare handoff summary",
        "tighten control hints",
        "verify acceptance matrix",
    ];

    format!("Task {id}: {}", TITLES[(id - 1) % TITLES.len()])
}

fn sample_changed_files(task_id: usize) -> Vec<String> {
    const FILES: [&str; 6] = [
        "examples/interactive-shell.rs",
        "README.md",
        "docs/ARCHITECTURE.md",
        "docs/EXTENSION_CAPABILITY_MATRIX.md",
        "src/runtime/tui.rs",
        "tests/renderer_golden.rs",
    ];

    (0..3)
        .map(|index| FILES[(task_id + index) % FILES.len()].to_string())
        .collect()
}

#[derive(Clone)]
struct TaskRow {
    id: usize,
    title: String,
    state: TaskState,
    session_id: Option<usize>,
    phase: Option<AgentPhase>,
    progress: Option<u8>,
    last_activity: Instant,
}

#[derive(Clone)]
struct SessionRow {
    id: usize,
    title: String,
    task_title: String,
    task_state: TaskState,
    mode: SessionMode,
    state: SessionState,
    phase: AgentPhase,
    progress: u8,
    last_line: String,
    last_activity: Instant,
}

#[derive(Clone)]
struct HandoffRow {
    session_title: String,
    task_title: String,
    summary: String,
    status: HandoffStatus,
    changed_files: Vec<String>,
    when: Instant,
    tail: Vec<String>,
}

struct SessionSnapshot {
    tasks: Vec<TaskRow>,
    sessions: Vec<SessionRow>,
    handoffs: Vec<HandoffRow>,
}

#[derive(Default)]
struct DashboardState {
    action: Option<DashboardAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DashboardAction {
    NewTask,
    NewSession(SessionMode),
    OpenSession(usize),
    KillSession(usize),
}

struct SessionDashboard {
    store: Arc<Mutex<SessionStore>>,
    state: Rc<RefCell<DashboardState>>,
    exit_flag: Rc<RefCell<bool>>,
    selected_task_index: usize,
}

impl SessionDashboard {
    fn new(
        store: Arc<Mutex<SessionStore>>,
        state: Rc<RefCell<DashboardState>>,
        exit_flag: Rc<RefCell<bool>>,
    ) -> Self {
        Self {
            store,
            state,
            exit_flag,
            selected_task_index: 0,
        }
    }

    fn set_action(&mut self, action: DashboardAction) {
        self.state.borrow_mut().action = Some(action);
    }

    fn handle_press(&mut self, key_id: &str, snapshot: &SessionSnapshot) {
        match key_id {
            "up" => {
                if self.selected_task_index > 0 {
                    self.selected_task_index -= 1;
                }
            }
            "down" => {
                if self.selected_task_index + 1 < snapshot.tasks.len() {
                    self.selected_task_index += 1;
                }
            }
            "enter" | "return" => {
                if let Some(task) = snapshot.tasks.get(self.selected_task_index) {
                    if let Some(session_id) = task.session_id {
                        self.set_action(DashboardAction::OpenSession(session_id));
                    }
                }
            }
            "ctrl+c" => {
                *self.exit_flag.borrow_mut() = true;
            }
            "t" => {
                self.set_action(DashboardAction::NewTask);
            }
            "n" => {
                self.set_action(DashboardAction::NewSession(SessionMode::Interactive));
            }
            "h" => {
                self.set_action(DashboardAction::NewSession(SessionMode::HandsFree));
            }
            "b" => {
                self.set_action(DashboardAction::NewSession(SessionMode::Background));
            }
            "x" => {
                if let Some(task) = snapshot.tasks.get(self.selected_task_index) {
                    if let Some(session_id) = task.session_id {
                        self.set_action(DashboardAction::KillSession(session_id));
                    }
                }
            }
            _ => {}
        }
    }
}

impl Component for SessionDashboard {
    fn render(&mut self, width: usize) -> Vec<String> {
        let snapshot = {
            let store = self.store.lock().expect("session store lock poisoned");
            store.snapshot()
        };

        if self.selected_task_index >= snapshot.tasks.len() && !snapshot.tasks.is_empty() {
            self.selected_task_index = snapshot.tasks.len() - 1;
        }

        let mut lines = Vec::new();
        let total_tasks = snapshot.tasks.len();
        let todo = snapshot
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Todo)
            .count();
        let running = snapshot
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Running)
            .count();
        let blocked = snapshot
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Blocked)
            .count();
        let done = snapshot
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Done)
            .count();

        let title = bold(&cyan("Interactive Shell Lab"));
        let stats = format!(
            "{}  {}  {}  {}",
            dim(&format!("tasks: {total_tasks}")),
            dim(&format!("todo: {todo}")),
            dim(&format!("running: {running}")),
            dim(&format!("done: {done}"))
        );
        let header = format!("{title}  {stats}");
        lines.push(pad_to_width(&header, width));
        lines.push(dim(&line_rule(width, '─')));
        lines.push(pad_to_width(
            &dim(
                "T new task  N/H/B new session  Enter attach  X kill selected session  Ctrl+C quit",
            ),
            width,
        ));

        lines.push(String::new());
        lines.push(pad_to_width(&bold("Task lanes"), width));
        let lane_line = format!(
            "{} {}   {} {}   {} {}   {} {}",
            TaskState::Todo.chip(),
            todo,
            TaskState::Running.chip(),
            running,
            TaskState::Blocked.chip(),
            blocked,
            TaskState::Done.chip(),
            done
        );
        lines.push(pad_to_width(&lane_line, width));

        lines.push(String::new());
        lines.push(pad_to_width(&bold("Tasks"), width));
        if snapshot.tasks.is_empty() {
            lines.push(pad_to_width(
                &dim("  No tasks. Press T to create one, then N/H/B to start a session."),
                width,
            ));
        }

        for (idx, task) in snapshot.tasks.iter().enumerate() {
            let marker = if idx == self.selected_task_index {
                "▸"
            } else {
                " "
            };
            let session = task
                .session_id
                .map(|id| format!("session #{id}"))
                .unwrap_or_else(|| dim("unassigned"));
            let phase = task
                .phase
                .map(|phase| phase.chip())
                .unwrap_or_else(|| dim("-"));
            let progress = task
                .progress
                .map(|value| format!("{value:>3}%"))
                .unwrap_or_else(|| dim(" --%"));
            let idle = format_duration(Instant::now().duration_since(task.last_activity));
            let line = format!(
                "{marker} #{} {}  {}  {}  {}  {}  {}",
                task.id,
                task.title,
                task.state.chip(),
                phase,
                progress,
                dim(&idle),
                session
            );
            lines.push(pad_to_width(&line, width));
        }

        lines.push(String::new());
        lines.push(pad_to_width(&bold("Sessions"), width));
        if snapshot.sessions.is_empty() {
            lines.push(pad_to_width(
                &dim("  No sessions yet. Start one with N/H/B."),
                width,
            ));
        }
        for session in snapshot.sessions.iter().take(4) {
            let age = format_duration(Instant::now().duration_since(session.last_activity));
            let line = format!(
                "  #{} {} {} {} {} {} {}",
                session.id,
                session.title,
                session.mode.colorize(session.mode.label()),
                session.state.chip(),
                session.phase.chip(),
                dim(&format!("{}%", session.progress)),
                dim(&age),
            );
            lines.push(pad_to_width(&line, width));
            lines.push(pad_to_width(
                &format!(
                    "    task: {} ({})",
                    session.task_title,
                    session.task_state.chip()
                ),
                width,
            ));
            lines.push(pad_to_width(
                &format!("    {}", dim(&session.last_line)),
                width,
            ));
        }

        lines.push(String::new());
        lines.push(pad_to_width(&bold("Review (latest handoff)"), width));
        if let Some(handoff) = snapshot.handoffs.first() {
            let elapsed = format_duration(Instant::now().duration_since(handoff.when));
            lines.push(pad_to_width(
                &format!(
                    "  {} {} from {} ({elapsed} ago)",
                    handoff.status.chip(),
                    handoff.task_title,
                    handoff.session_title
                ),
                width,
            ));
            lines.push(pad_to_width(
                &format!("    summary: {}", handoff.summary),
                width,
            ));
            lines.push(pad_to_width(
                &format!("    files: {}", handoff.changed_files.join(", ")),
                width,
            ));
            for tail_line in handoff.tail.iter().rev().take(2).rev() {
                lines.push(pad_to_width(&format!("    {}", dim(tail_line)), width));
            }
        } else {
            lines.push(pad_to_width(
                &dim("  No handoffs yet. Open a session and press Ctrl+T."),
                width,
            ));
        }

        lines.push(String::new());
        lines.push(dim(&line_rule(width, '─')));
        let footer = dim("Tip: block with `block`, resume with `resume`, and handoff with Ctrl+T.");
        lines.push(pad_to_width(&footer, width));

        lines
    }

    fn handle_event(&mut self, event: &InputEvent) {
        let snapshot = {
            let store = self.store.lock().expect("session store lock poisoned");
            store.snapshot()
        };

        match event {
            InputEvent::Key {
                key_id,
                event_type: KeyEventType::Press,
                ..
            } => {
                self.handle_press(key_id, &snapshot);
            }
            InputEvent::Text {
                text,
                event_type: KeyEventType::Press,
                ..
            } => {
                let trimmed = text.trim().to_lowercase();
                if trimmed.len() == 1 {
                    self.handle_press(trimmed.as_str(), &snapshot);
                }
            }
            _ => {}
        }
    }

    fn invalidate(&mut self) {}

    fn set_terminal_rows(&mut self, _rows: usize) {}

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for SessionDashboard {
    fn set_focused(&mut self, _focused: bool) {}

    fn is_focused(&self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayCloseReason {
    Background,
    Handoff,
    Killed,
    Exit,
}

#[derive(Default)]
struct OverlaySignals {
    close_reason: Option<OverlayCloseReason>,
}

impl OverlaySignals {
    fn request_close(&mut self, reason: OverlayCloseReason) {
        self.close_reason = Some(reason);
    }

    fn take_reason(&mut self) -> Option<OverlayCloseReason> {
        self.close_reason.take()
    }
}

struct OverlaySnapshot {
    title: String,
    command: String,
    mode: SessionMode,
    state: SessionState,
    task_title: String,
    task_state: TaskState,
    phase: AgentPhase,
    progress: u8,
    output: Vec<String>,
    last_activity: Instant,
}

struct InteractiveOverlay {
    store: Arc<Mutex<SessionStore>>,
    session_id: usize,
    signals: Rc<RefCell<OverlaySignals>>,
    input_buffer: String,
    scroll_offset: usize,
    autoscroll: bool,
    last_output_len: usize,
    focused: bool,
    viewport_rows: usize,
    show_menu: bool,
}

impl InteractiveOverlay {
    fn new(
        store: Arc<Mutex<SessionStore>>,
        session_id: usize,
        signals: Rc<RefCell<OverlaySignals>>,
    ) -> Self {
        Self {
            store,
            session_id,
            signals,
            input_buffer: String::new(),
            scroll_offset: 0,
            autoscroll: true,
            last_output_len: 0,
            focused: false,
            viewport_rows: 0,
            show_menu: false,
        }
    }

    fn snapshot(&self) -> Option<OverlaySnapshot> {
        let store = self.store.lock().expect("session store lock poisoned");
        let session = store.session(self.session_id)?;
        let task = store.task(session.task_id)?;

        Some(OverlaySnapshot {
            title: session.title.clone(),
            command: session.command.clone(),
            mode: session.mode,
            state: session.state,
            task_title: task.title.clone(),
            task_state: task.state,
            phase: session.simulation.phase,
            progress: session.simulation.progress,
            output: session.output.iter().cloned().collect(),
            last_activity: session.last_activity,
        })
    }

    fn handle_control(&mut self, key_id: &str) -> bool {
        match key_id {
            "ctrl+c" => {
                {
                    let mut store = self.store.lock().expect("session store lock poisoned");
                    store.interrupt_session(self.session_id);
                }
                self.input_buffer.clear();
                self.scroll_offset = 0;
                self.autoscroll = true;
                true
            }
            "ctrl+r" => {
                {
                    let mut store = self.store.lock().expect("session store lock poisoned");
                    store.resume_session(self.session_id);
                }
                true
            }
            "ctrl+b" => {
                {
                    let mut store = self.store.lock().expect("session store lock poisoned");
                    store.mark_background(self.session_id);
                }
                self.signals
                    .borrow_mut()
                    .request_close(OverlayCloseReason::Background);
                true
            }
            "ctrl+t" => {
                {
                    let mut store = self.store.lock().expect("session store lock poisoned");
                    store.record_handoff(self.session_id);
                    store.mark_background(self.session_id);
                }
                self.signals
                    .borrow_mut()
                    .request_close(OverlayCloseReason::Handoff);
                true
            }
            "ctrl+k" => {
                {
                    let mut store = self.store.lock().expect("session store lock poisoned");
                    store.kill_session(self.session_id, 130);
                }
                self.signals
                    .borrow_mut()
                    .request_close(OverlayCloseReason::Killed);
                true
            }
            "ctrl+q" => {
                self.show_menu = !self.show_menu;
                true
            }
            "escape" => {
                self.signals
                    .borrow_mut()
                    .request_close(OverlayCloseReason::Exit);
                true
            }
            "shift+up" | "pageup" => {
                self.autoscroll = false;
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                true
            }
            "shift+down" | "pagedown" => {
                if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                }
                if self.scroll_offset == 0 {
                    self.autoscroll = true;
                }
                true
            }
            "home" => {
                self.autoscroll = false;
                self.scroll_offset = usize::MAX;
                true
            }
            "end" => {
                self.scroll_offset = 0;
                self.autoscroll = true;
                true
            }
            _ => false,
        }
    }

    fn maybe_take_control(&mut self, snapshot: &OverlaySnapshot) {
        if snapshot.mode == SessionMode::HandsFree {
            let mut store = self.store.lock().expect("session store lock poisoned");
            store.mark_interactive(self.session_id);
        }
    }

    fn submit_input(&mut self) {
        let trimmed = self.input_buffer.trim().to_string();
        if trimmed.is_empty() {
            self.input_buffer.clear();
            return;
        }

        {
            let mut store = self.store.lock().expect("session store lock poisoned");
            if let Some(session) = store.session(self.session_id) {
                if session.state != SessionState::Running {
                    if let Some(session) = store.session_mut(self.session_id) {
                        session.push_output(
                            "[system] session exited; input ignored (Esc to close)".to_string(),
                        );
                    }
                    self.input_buffer.clear();
                    return;
                }
            }
            store.enqueue_input(self.session_id, trimmed);
        }
        self.input_buffer.clear();
    }
}

impl Component for InteractiveOverlay {
    fn render(&mut self, width: usize) -> Vec<String> {
        let content_width = width.saturating_sub(2);

        let Some(snapshot) = self.snapshot() else {
            return box_shell_lines(
                vec![pad_to_width(&red("Session missing"), content_width)],
                width,
            );
        };

        if snapshot.output.len() != self.last_output_len {
            self.last_output_len = snapshot.output.len();
            if self.autoscroll {
                self.scroll_offset = 0;
            }
        }

        let overlay_rows = self.viewport_rows.max(8);
        let border_lines = 2usize;
        let header_lines = 4usize;
        let footer_lines = if self.show_menu { 4 } else { 3 };
        let chrome = border_lines + header_lines + footer_lines + 1;
        let body_height = overlay_rows.saturating_sub(chrome).max(4);

        let mut lines = Vec::new();

        let title = bold(&cyan(&format!("{} ({})", snapshot.title, snapshot.command)));
        lines.push(pad_to_width(&title, content_width));

        let mode = snapshot.mode.colorize(snapshot.mode.label());
        let state = snapshot.state.chip();
        let task = format!("{} ({})", snapshot.task_title, snapshot.task_state.chip());
        lines.push(pad_to_width(
            &format!("mode: {mode}  state: {state}"),
            content_width,
        ));
        lines.push(pad_to_width(
            &format!(
                "task: {task}  phase: {}  progress: {}%",
                snapshot.phase.chip(),
                snapshot.progress
            ),
            content_width,
        ));

        if snapshot.mode == SessionMode::HandsFree {
            let note = italic("hands-free: press any key to take control");
            lines.push(pad_to_width(&note, content_width));
        } else {
            let idle = format_duration(Instant::now().duration_since(snapshot.last_activity));
            lines.push(pad_to_width(&dim(&format!("idle: {idle}")), content_width));
        }

        lines.push(dim(&line_rule(content_width, '─')));
        let body_start = lines.len();

        let max_offset = snapshot.output.len().saturating_sub(body_height);
        if self.scroll_offset == usize::MAX {
            self.scroll_offset = max_offset;
        }
        if self.scroll_offset > max_offset {
            self.scroll_offset = max_offset;
        }

        let end = snapshot.output.len().saturating_sub(self.scroll_offset);
        let start = end.saturating_sub(body_height);
        let visible = snapshot.output.get(start..end).unwrap_or(&[]);

        for line in visible.iter() {
            lines.push(pad_to_width(line, content_width));
        }
        while lines.len() < body_start + body_height {
            lines.push(String::new());
        }

        let scroll = format!("scroll: {}/{}", self.scroll_offset, max_offset);
        let footer_left = dim(
            "Ctrl+C interrupt  Ctrl+R resume  Ctrl+B background  Ctrl+T handoff  Ctrl+K kill  Esc close",
        );
        lines.push(pad_to_width(&footer_left, content_width));

        if self.show_menu {
            lines.push(pad_to_width(
                &dim("Menu: B background  T handoff  K kill  Esc close menu"),
                content_width,
            ));
            lines.push(pad_to_width(
                &dim("Menu captures input: unrelated keys/text are ignored."),
                content_width,
            ));
        } else {
            let footer = format!(
                "{}  {}",
                dim("Enter send, Shift+Up/Down scroll"),
                dim(&scroll)
            );
            lines.push(pad_to_width(&footer, content_width));
        }

        let mut prompt = format!("input> {}", self.input_buffer);
        if self.focused {
            prompt.push_str(CURSOR_MARKER);
        }
        lines.push(pad_to_width(&prompt, content_width));

        box_shell_lines(lines, width)
    }

    fn handle_event(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Key {
                key_id,
                event_type: KeyEventType::Press,
                ..
            } => {
                if self.show_menu {
                    match key_id.as_str() {
                        "escape" => {
                            self.show_menu = false;
                        }
                        "b" => {
                            self.handle_control("ctrl+b");
                        }
                        "t" => {
                            self.handle_control("ctrl+t");
                        }
                        "k" => {
                            self.handle_control("ctrl+k");
                        }
                        "ctrl+b" | "ctrl+t" | "ctrl+k" | "ctrl+q" => {
                            self.handle_control(key_id);
                        }
                        _ => {}
                    }
                    return;
                }

                if self.handle_control(key_id) {
                    return;
                }

                let snapshot = match self.snapshot() {
                    Some(snapshot) => snapshot,
                    None => return,
                };

                if snapshot.mode == SessionMode::HandsFree {
                    self.maybe_take_control(&snapshot);
                }

                match key_id.as_str() {
                    "enter" | "return" => {
                        self.submit_input();
                    }
                    "backspace" => {
                        self.input_buffer.pop();
                    }
                    "up" => {
                        self.autoscroll = false;
                        self.scroll_offset = self.scroll_offset.saturating_add(1);
                    }
                    "down" => {
                        if self.scroll_offset > 0 {
                            self.scroll_offset -= 1;
                        }
                        if self.scroll_offset == 0 {
                            self.autoscroll = true;
                        }
                    }
                    _ => {}
                }
            }
            InputEvent::Text {
                text,
                event_type: KeyEventType::Press,
                ..
            } => {
                if self.show_menu {
                    let normalized = text.trim().to_lowercase();
                    match normalized.as_str() {
                        "b" => {
                            self.handle_control("ctrl+b");
                        }
                        "t" => {
                            self.handle_control("ctrl+t");
                        }
                        "k" => {
                            self.handle_control("ctrl+k");
                        }
                        _ => {}
                    }
                    return;
                }

                let snapshot = match self.snapshot() {
                    Some(snapshot) => snapshot,
                    None => return,
                };
                if snapshot.mode == SessionMode::HandsFree {
                    self.maybe_take_control(&snapshot);
                }
                self.input_buffer.push_str(text);
            }
            InputEvent::Paste { text, .. } => {
                if self.show_menu {
                    return;
                }
                let snapshot = match self.snapshot() {
                    Some(snapshot) => snapshot,
                    None => return,
                };
                if snapshot.mode == SessionMode::HandsFree {
                    self.maybe_take_control(&snapshot);
                }
                self.input_buffer.push_str(text);
            }
            _ => {}
        }
    }

    fn cursor_pos(&self) -> Option<tape_tui::core::cursor::CursorPos> {
        None
    }

    fn set_viewport_size(&mut self, _cols: usize, rows: usize) {
        self.viewport_rows = rows;
    }

    fn set_terminal_rows(&mut self, _rows: usize) {}

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for InteractiveOverlay {
    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }
}

fn overlay_surface_options() -> SurfaceOptions {
    SurfaceOptions {
        kind: SurfaceKind::Modal,
        input_policy: SurfaceInputPolicy::Capture,
        overlay: OverlayOptions {
            anchor: Some(OverlayAnchor::Center),
            margin: Some(OverlayMargin::uniform(1)),
            width: Some(SizeValue::percent(OVERLAY_WIDTH_PERCENT)),
            max_height: Some(SizeValue::percent(OVERLAY_HEIGHT_PERCENT)),
            ..Default::default()
        },
    }
}

#[derive(Default)]
struct TickDecision {
    exit_now: bool,
}

fn drive_session_tick(store: &mut SessionStore, id: usize, tick: usize) -> TickDecision {
    let mut decision = TickDecision::default();
    let mut task_update: Option<(usize, TaskState)> = None;

    {
        let Some(session) = store.session_mut(id) else {
            decision.exit_now = true;
            return decision;
        };

        if session.state != SessionState::Running {
            decision.exit_now = true;
            return decision;
        }

        while let Some(input) = session.pending_inputs.pop_front() {
            let trimmed = input.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }

            session.push_output(format!("> {trimmed}"));
            let lowered = trimmed.to_ascii_lowercase();

            match lowered.as_str() {
                "help" => {
                    session.push_output(
                        "commands: help, status, plan, summarize, resume, block, fail, exit"
                            .to_string(),
                    );
                }
                "status" => {
                    session.push_output(format!(
                        "status: phase={} progress={} interrupted={}",
                        session.simulation.phase.label(),
                        session.simulation.progress,
                        session.interrupted
                    ));
                }
                "plan" => {
                    session.push_output(format!("plan: {}", session.simulation.plan_overview()));
                }
                "summarize" => {
                    session.push_output(format!("summary: {}", session.simulation.summary_line()));
                }
                "resume" => {
                    session.interrupted = false;
                    session.push_output("[system] resumed deterministic worker".to_string());
                    task_update = Some((session.task_id, TaskState::Running));
                }
                "block" => {
                    session.interrupted = true;
                    session.push_output(
                        "[system] task marked blocked; run `resume` to continue".to_string(),
                    );
                    task_update = Some((session.task_id, TaskState::Blocked));
                }
                "fail" => {
                    session.interrupted = false;
                    for line in session
                        .simulation
                        .force_error("simulated verification failure")
                    {
                        session.push_output(line);
                    }
                    session.state = SessionState::Exited { code: 1 };
                    session.mode = SessionMode::Background;
                    session.push_output("[system] process exited (1)".to_string());
                    task_update = Some((session.task_id, TaskState::Blocked));
                    decision.exit_now = true;
                    break;
                }
                "exit" | "quit" => {
                    session.push_output("[system] exiting...".to_string());
                    session.state = SessionState::Exited { code: 0 };
                    session.mode = SessionMode::Background;
                    session.push_output("[system] process exited (0)".to_string());
                    let terminal_task_state = if session.simulation.phase == AgentPhase::Done {
                        TaskState::Done
                    } else {
                        TaskState::Blocked
                    };
                    task_update = Some((session.task_id, terminal_task_state));
                    decision.exit_now = true;
                    break;
                }
                _ => {
                    session.push_output(simulate_response(&trimmed, &session.simulation, tick));
                }
            }
        }

        if !decision.exit_now && session.state == SessionState::Running {
            if session.interrupted {
                if tick % 12 == 0 {
                    session
                        .push_output("[agent] paused (interrupt active; use `resume`)".to_string());
                }
            } else {
                let advance = session.simulation.advance();
                for line in advance.lines {
                    session.push_output(line);
                }
                if let Some(state) = advance.task_state {
                    task_update = Some((session.task_id, state));
                }
                if advance.finished {
                    session.state = SessionState::Exited { code: 0 };
                    session.mode = SessionMode::Background;
                    session.push_output("[system] process exited (0)".to_string());
                    decision.exit_now = true;
                }
            }
        }
    }

    if let Some((task_id, state)) = task_update {
        store.set_task_state(task_id, state);
    }

    decision
}

fn spawn_session_thread(id: usize, store: Arc<Mutex<SessionStore>>, render: RuntimeHandle) {
    thread::spawn(move || {
        let mut tick: usize = 0;
        loop {
            let exit_now = {
                let mut store = store.lock().expect("session store lock poisoned");
                drive_session_tick(&mut store, id, tick).exit_now
            };

            render.dispatch(RuntimeCommand::RequestRender);

            if exit_now {
                break;
            }

            thread::sleep(Duration::from_millis(200));
            tick = tick.saturating_add(1);
        }
    });
}

fn simulate_response(input: &str, simulation: &SessionSimulation, tick: usize) -> String {
    if input.eq_ignore_ascii_case("files") {
        return "mock files: examples/interactive-shell.rs, README.md".to_string();
    }
    if input.eq_ignore_ascii_case("phase") {
        return format!("phase: {}", simulation.phase.label());
    }
    if input.eq_ignore_ascii_case("progress") {
        return format!("progress: {}%", simulation.progress);
    }
    if input.eq_ignore_ascii_case("heartbeat") {
        return format!("heartbeat tick: {tick}");
    }
    format!("echo: {input}")
}

fn main() -> std::io::Result<()> {
    let terminal = ProcessTerminal::new();
    let mut tui = TUI::new(terminal);
    let runtime_handle = tui.runtime_handle();

    let store = Arc::new(Mutex::new(SessionStore::new()));
    let dashboard_state = Rc::new(RefCell::new(DashboardState::default()));
    let exit_flag = Rc::new(RefCell::new(false));

    let dashboard = SessionDashboard::new(
        Arc::clone(&store),
        Rc::clone(&dashboard_state),
        Rc::clone(&exit_flag),
    );
    let dashboard_id = tui.register_component(dashboard);
    tui.set_root(vec![dashboard_id]);
    tui.set_focus(dashboard_id);
    tui.start()?;

    let mut overlay_handle: Option<SurfaceHandle> = None;
    let mut overlay_signals: Option<Rc<RefCell<OverlaySignals>>> = None;

    runtime_handle.dispatch(RuntimeCommand::RequestRender);

    loop {
        tui.run_blocking_once();

        if *exit_flag.borrow() {
            break;
        }

        if let Some(signals) = overlay_signals.as_ref() {
            let reason = signals.borrow_mut().take_reason();
            if reason.is_some() {
                if let Some(handle) = overlay_handle.take() {
                    handle.hide();
                }
                overlay_signals = None;
                tui.request_render();
            }
        }

        let action = dashboard_state.borrow_mut().action.take();
        if let Some(action) = action {
            match action {
                DashboardAction::NewTask => {
                    let mut store = store.lock().expect("session store lock poisoned");
                    store.create_task();
                    tui.request_render();
                }
                DashboardAction::NewSession(mode) => {
                    let id = {
                        let mut store = store.lock().expect("session store lock poisoned");
                        store.create_session(mode)
                    };
                    spawn_session_thread(id, Arc::clone(&store), runtime_handle.clone());
                    if mode != SessionMode::Background {
                        if let Some(handle) = overlay_handle.take() {
                            handle.hide();
                        }

                        let signals = Rc::new(RefCell::new(OverlaySignals::default()));
                        let surface =
                            InteractiveOverlay::new(Arc::clone(&store), id, Rc::clone(&signals));
                        let surface_id = tui.register_component(surface);
                        let handle = tui.show_surface(surface_id, Some(overlay_surface_options()));
                        overlay_handle = Some(handle);
                        overlay_signals = Some(signals);
                    }
                    tui.request_render();
                }
                DashboardAction::OpenSession(id) => {
                    let should_open = {
                        let store = store.lock().expect("session store lock poisoned");
                        matches!(
                            store.session(id).map(|session| session.state),
                            Some(SessionState::Running)
                        )
                    };
                    if should_open {
                        if let Some(handle) = overlay_handle.take() {
                            handle.hide();
                        }

                        let signals = Rc::new(RefCell::new(OverlaySignals::default()));
                        let surface =
                            InteractiveOverlay::new(Arc::clone(&store), id, Rc::clone(&signals));
                        let surface_id = tui.register_component(surface);
                        let handle = tui.show_surface(surface_id, Some(overlay_surface_options()));
                        overlay_handle = Some(handle);
                        overlay_signals = Some(signals);
                        tui.request_render();
                    }
                }
                DashboardAction::KillSession(id) => {
                    let mut store = store.lock().expect("session store lock poisoned");
                    store.kill_session(id, 137);
                    tui.request_render();
                }
            }
        }
    }

    tui.stop()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_simulation_repeats_exact_timeline() {
        let mut a = SessionSimulation::new();
        let mut b = SessionSimulation::new();

        let mut timeline_a = Vec::new();
        let mut timeline_b = Vec::new();

        for _ in 0..TOTAL_SIMULATION_TICKS {
            timeline_a.extend(a.advance().lines);
            timeline_b.extend(b.advance().lines);
        }

        assert_eq!(timeline_a, timeline_b);
        assert_eq!(a.phase, AgentPhase::Done);
        assert_eq!(b.phase, AgentPhase::Done);
        assert_eq!(a.progress, 100);
        assert_eq!(b.progress, 100);
    }

    #[test]
    fn interrupt_pauses_progress_until_resume() {
        let mut store = SessionStore::new();
        let session_id = store.create_session(SessionMode::Interactive);

        store.interrupt_session(session_id);
        let progress_before = store
            .session(session_id)
            .map(|session| session.simulation.progress)
            .expect("session should exist");

        drive_session_tick(&mut store, session_id, 1);

        let progress_after_interrupt = store
            .session(session_id)
            .map(|session| session.simulation.progress)
            .expect("session should exist");
        assert_eq!(progress_before, progress_after_interrupt);

        store.enqueue_input(session_id, "resume".to_string());
        drive_session_tick(&mut store, session_id, 2);

        let progress_after_resume = store
            .session(session_id)
            .map(|session| session.simulation.progress)
            .expect("session should exist");
        assert!(progress_after_resume > progress_after_interrupt);
    }

    #[test]
    fn session_creation_reuses_unassigned_todo_task() {
        let mut store = SessionStore::new();
        let task_id = store.create_task();

        let session_id = store.create_session(SessionMode::Interactive);
        let session_task_id = store
            .session(session_id)
            .map(|session| session.task_id)
            .expect("session should exist");

        assert_eq!(session_task_id, task_id);
        assert_eq!(
            store.task(task_id).map(|task| task.session_id),
            Some(Some(session_id))
        );
    }

    #[test]
    fn handoff_records_are_bounded_and_structured() {
        let mut store = SessionStore::new();
        let session_id = store.create_session(SessionMode::Interactive);

        for index in 0..(MAX_HANDOFFS + 3) {
            if let Some(session) = store.session_mut(session_id) {
                session.push_output(format!("line {index}"));
            }
            store.record_handoff(session_id);
        }

        assert_eq!(store.handoffs.len(), MAX_HANDOFFS);
        let latest = store.handoffs.front().expect("latest handoff should exist");
        assert!(!latest.summary.is_empty());
        assert!(!latest.changed_files.is_empty());
    }

    #[test]
    fn shift_scroll_does_not_close_exited_overlay() {
        let store = Arc::new(Mutex::new(SessionStore::new()));
        let session_id = {
            let mut locked = store.lock().expect("session store lock poisoned");
            let session_id = locked.create_session(SessionMode::Interactive);
            if let Some(session) = locked.session_mut(session_id) {
                session.state = SessionState::Exited { code: 0 };
                session.mode = SessionMode::Background;
            }
            session_id
        };

        let signals = Rc::new(RefCell::new(OverlaySignals::default()));
        let mut overlay =
            InteractiveOverlay::new(Arc::clone(&store), session_id, Rc::clone(&signals));

        assert!(overlay.handle_control("shift+up"));
        assert!(overlay.handle_control("shift+down"));
        assert_eq!(signals.borrow_mut().take_reason(), None);
    }

    #[test]
    fn inline_scroll_offset_clamps_after_session_exit_and_resize() {
        let store = Arc::new(Mutex::new(SessionStore::new()));
        let session_id = {
            let mut locked = store.lock().expect("session store lock poisoned");
            let session_id = locked.create_session(SessionMode::Interactive);
            if let Some(session) = locked.session_mut(session_id) {
                session.state = SessionState::Exited { code: 0 };
                session.mode = SessionMode::Background;
                for index in 0..32 {
                    session.push_output(format!("[tail] line {index}"));
                }
            }
            session_id
        };

        let signals = Rc::new(RefCell::new(OverlaySignals::default()));
        let mut overlay =
            InteractiveOverlay::new(Arc::clone(&store), session_id, Rc::clone(&signals));

        let body_height_for_viewport = |rows: usize| {
            let border_lines = 2usize;
            let header_lines = 4usize;
            let footer_lines = 3usize;
            let chrome = border_lines + header_lines + footer_lines + 1;
            rows.saturating_sub(chrome).max(4)
        };

        overlay.set_viewport_size(80, 24);
        overlay.render(80);
        for _ in 0..8 {
            assert!(overlay.handle_control("shift+up"));
        }
        overlay.render(80);
        assert!(overlay.scroll_offset > 0);

        overlay.set_viewport_size(80, 80);
        overlay.render(80);
        let max_offset_large = {
            let locked = store.lock().expect("session store lock poisoned");
            let output_len = locked
                .session(session_id)
                .expect("session should exist")
                .output
                .len();
            output_len.saturating_sub(body_height_for_viewport(80))
        };
        assert!(overlay.scroll_offset <= max_offset_large);

        overlay.set_viewport_size(80, 8);
        overlay.render(80);
        let max_offset_small = {
            let locked = store.lock().expect("session store lock poisoned");
            let output_len = locked
                .session(session_id)
                .expect("session should exist")
                .output
                .len();
            output_len.saturating_sub(body_height_for_viewport(8))
        };
        assert!(overlay.scroll_offset <= max_offset_small);
        assert_eq!(signals.borrow_mut().take_reason(), None);
    }

    #[test]
    fn dashboard_accepts_enter_and_return_for_attach() {
        let store = Arc::new(Mutex::new(SessionStore::new()));
        let session_id = {
            let mut locked = store.lock().expect("session store lock poisoned");
            locked.create_session(SessionMode::Interactive)
        };

        let state = Rc::new(RefCell::new(DashboardState::default()));
        let exit_flag = Rc::new(RefCell::new(false));
        let mut dashboard = SessionDashboard::new(Arc::clone(&store), Rc::clone(&state), exit_flag);

        dashboard.handle_event(&InputEvent::Key {
            raw: "\r".to_string(),
            key_id: "enter".to_string(),
            event_type: KeyEventType::Press,
        });
        assert_eq!(
            state.borrow().action,
            Some(DashboardAction::OpenSession(session_id))
        );

        state.borrow_mut().action = None;

        dashboard.handle_event(&InputEvent::Key {
            raw: "\r".to_string(),
            key_id: "return".to_string(),
            event_type: KeyEventType::Press,
        });
        assert_eq!(
            state.borrow().action,
            Some(DashboardAction::OpenSession(session_id))
        );
    }

    #[test]
    fn dashboard_shortcuts_work_without_overlay_flag_state() {
        let store = Arc::new(Mutex::new(SessionStore::new()));
        {
            let mut locked = store.lock().expect("session store lock poisoned");
            locked.create_session(SessionMode::Interactive);
        }

        let state = Rc::new(RefCell::new(DashboardState::default()));
        let exit_flag = Rc::new(RefCell::new(false));
        let mut dashboard = SessionDashboard::new(Arc::clone(&store), Rc::clone(&state), exit_flag);

        dashboard.handle_event(&InputEvent::Text {
            raw: "n".to_string(),
            text: "n".to_string(),
            event_type: KeyEventType::Press,
        });
        assert_eq!(
            state.borrow().action,
            Some(DashboardAction::NewSession(SessionMode::Interactive))
        );

        state.borrow_mut().action = None;
        dashboard.handle_event(&InputEvent::Text {
            raw: "t".to_string(),
            text: "t".to_string(),
            event_type: KeyEventType::Press,
        });
        assert_eq!(state.borrow().action, Some(DashboardAction::NewTask));
    }
}
