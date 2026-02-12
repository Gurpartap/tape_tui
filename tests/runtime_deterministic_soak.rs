use std::sync::{Arc, Mutex};

use tape_tui::core::cursor::CursorPos;
use tape_tui::core::terminal::Terminal;
use tape_tui::{
    Component, Focusable, InputEvent, SurfaceInputPolicy, SurfaceKind, SurfaceOptions, TUI,
};

const SOAK_RUNS: usize = 20;

#[derive(Default)]
struct TerminalState {
    writes: String,
    columns: u16,
    rows: u16,
    on_input: Option<Box<dyn FnMut(String) + Send>>,
    on_resize: Option<Box<dyn FnMut() + Send>>,
}

#[derive(Clone)]
struct HarnessTerminal {
    state: Arc<Mutex<TerminalState>>,
}

impl HarnessTerminal {
    fn new(columns: u16, rows: u16) -> Self {
        Self {
            state: Arc::new(Mutex::new(TerminalState {
                writes: String::new(),
                columns,
                rows,
                on_input: None,
                on_resize: None,
            })),
        }
    }

    fn take_writes(&self) -> String {
        let mut state = self.state.lock().expect("lock terminal state for writes");
        std::mem::take(&mut state.writes)
    }

    fn set_size(&self, columns: u16, rows: u16) {
        let mut state = self.state.lock().expect("lock terminal state for resize");
        state.columns = columns;
        state.rows = rows;
    }

    fn emit_resize(&self) {
        let mut state = self
            .state
            .lock()
            .expect("lock terminal state for resize callback");
        if let Some(callback) = state.on_resize.as_mut() {
            callback();
        }
    }

    fn emit_input(&self, data: &str) {
        let mut state = self
            .state
            .lock()
            .expect("lock terminal state for input callback");
        if let Some(callback) = state.on_input.as_mut() {
            callback(data.to_string());
        }
    }
}

impl Terminal for HarnessTerminal {
    fn start(
        &mut self,
        on_input: Box<dyn FnMut(String) + Send>,
        on_resize: Box<dyn FnMut() + Send>,
    ) -> std::io::Result<()> {
        let mut state = self.state.lock().expect("lock terminal state for start");
        state.on_input = Some(on_input);
        state.on_resize = Some(on_resize);
        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<()> {
        let mut state = self.state.lock().expect("lock terminal state for stop");
        state.on_input = None;
        state.on_resize = None;
        Ok(())
    }

    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}

    fn write(&mut self, data: &str) {
        let mut state = self.state.lock().expect("lock terminal state for write");
        state.writes.push_str(data);
    }

    fn columns(&self) -> u16 {
        let state = self.state.lock().expect("lock terminal state for columns");
        state.columns
    }

    fn rows(&self) -> u16 {
        let state = self.state.lock().expect("lock terminal state for rows");
        state.rows
    }
}

#[derive(Clone)]
struct ProbeState {
    events: Arc<Mutex<Vec<String>>>,
    focus_trace: Arc<Mutex<Vec<bool>>>,
}

impl ProbeState {
    fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            focus_trace: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().expect("lock events").clone()
    }

    fn focus_trace(&self) -> Vec<bool> {
        self.focus_trace.lock().expect("lock focus trace").clone()
    }
}

struct ProbeComponent {
    lines: Vec<String>,
    cursor: Option<CursorPos>,
    focused: bool,
    state: ProbeState,
}

impl ProbeComponent {
    fn new(lines: Vec<String>, cursor: Option<CursorPos>, state: ProbeState) -> Self {
        Self {
            lines,
            cursor,
            focused: false,
            state,
        }
    }
}

impl Component for ProbeComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }

    fn handle_event(&mut self, event: &InputEvent) {
        let entry = match event {
            InputEvent::Text { text, .. } => format!("text:{text}"),
            InputEvent::Key {
                key_id, event_type, ..
            } => format!("key:{key_id}:{event_type:?}"),
            InputEvent::Paste { text, .. } => format!("paste:{text}"),
            InputEvent::Resize { columns, rows } => format!("resize:{columns}x{rows}"),
            InputEvent::UnknownRaw { raw } => format!("raw:{raw}"),
        };
        self.state
            .events
            .lock()
            .expect("lock events for push")
            .push(entry);
    }

    fn cursor_pos(&self) -> Option<CursorPos> {
        self.cursor
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for ProbeComponent {
    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.state
            .focus_trace
            .lock()
            .expect("lock focus trace for push")
            .push(focused);
    }

    fn is_focused(&self) -> bool {
        self.focused
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FocusRoutingSnapshot {
    root_events: Vec<String>,
    surface_events: Vec<String>,
    root_focus_trace: Vec<bool>,
    surface_focus_trace: Vec<bool>,
    output: String,
    max_cursor_column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ViewportSnapshot {
    output: String,
    max_cursor_column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VisibilityToggleSnapshot {
    root_events: Vec<String>,
    surface_events: Vec<String>,
    root_focus_trace: Vec<bool>,
    surface_focus_trace: Vec<bool>,
    output: String,
}

fn max_cursor_column(output: &str) -> usize {
    let bytes = output.as_bytes();
    let mut i = 0;
    let mut max_col = 0usize;

    while i + 2 < bytes.len() {
        if bytes[i] != 0x1b || bytes[i + 1] != b'[' {
            i += 1;
            continue;
        }

        let mut j = i + 2;
        let mut first = 0usize;
        let mut has_first = false;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            first = first
                .saturating_mul(10)
                .saturating_add((bytes[j] - b'0') as usize);
            has_first = true;
            j += 1;
        }

        if has_first && j < bytes.len() && bytes[j] == b'G' {
            max_col = max_col.max(first);
            i = j + 1;
            continue;
        }

        if has_first && j < bytes.len() && bytes[j] == b';' {
            j += 1;
            let mut second = 0usize;
            let mut has_second = false;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                second = second
                    .saturating_mul(10)
                    .saturating_add((bytes[j] - b'0') as usize);
                has_second = true;
                j += 1;
            }

            if has_second && j < bytes.len() && bytes[j] == b'H' {
                max_col = max_col.max(second);
                i = j + 1;
                continue;
            }
        }

        i += 1;
    }

    max_col
}

fn run_focus_routing_snapshot() -> FocusRoutingSnapshot {
    let terminal = HarnessTerminal::new(5, 3);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal);

    runtime
        .start()
        .expect("start runtime for focus/routing snapshot");
    probe_terminal.take_writes();

    let root_state = ProbeState::new();
    let root_component = ProbeComponent::new(
        vec![
            "root-0".to_string(),
            "root-1".to_string(),
            "root-2".to_string(),
            "root-3".to_string(),
            "root-4".to_string(),
        ],
        Some(CursorPos { row: 4, col: 18 }),
        root_state.clone(),
    );
    let root_id = runtime.register_component(root_component);
    runtime.set_root(vec![root_id]);
    runtime.set_focus(root_id);

    let surface_state = ProbeState::new();
    let surface_component = ProbeComponent::new(
        vec!["surface-row".to_string()],
        Some(CursorPos { row: 0, col: 14 }),
        surface_state.clone(),
    );
    let surface_id = runtime.register_component(surface_component);

    let surface = runtime.show_surface(surface_id, None);
    runtime.render_now();
    probe_terminal.take_writes();

    runtime.handle_input("x");
    runtime.render_if_needed();

    surface.hide();
    runtime.render_now();

    runtime.handle_input("y");
    runtime.render_if_needed();

    runtime
        .stop()
        .expect("stop runtime for focus/routing snapshot");
    let output = probe_terminal.take_writes();

    FocusRoutingSnapshot {
        root_events: root_state.events(),
        surface_events: surface_state.events(),
        root_focus_trace: root_state.focus_trace(),
        surface_focus_trace: surface_state.focus_trace(),
        max_cursor_column: max_cursor_column(&output),
        output,
    }
}

fn run_viewport_snapshot() -> ViewportSnapshot {
    let terminal = HarnessTerminal::new(6, 2);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal);

    runtime
        .start()
        .expect("start runtime for viewport snapshot");
    probe_terminal.take_writes();

    let root_state = ProbeState::new();
    let root_component = ProbeComponent::new(
        vec![
            "line-0".to_string(),
            "line-1".to_string(),
            "line-2".to_string(),
            "line-3".to_string(),
            "line-4".to_string(),
            "line-5".to_string(),
        ],
        Some(CursorPos { row: 5, col: 31 }),
        root_state,
    );
    let root_id = runtime.register_component(root_component);
    runtime.set_root(vec![root_id]);
    runtime.set_focus(root_id);
    runtime.render_now();

    let output = probe_terminal.take_writes();

    runtime.stop().expect("stop runtime for viewport snapshot");

    ViewportSnapshot {
        max_cursor_column: max_cursor_column(&output),
        output,
    }
}

fn run_visibility_toggle_snapshot() -> VisibilityToggleSnapshot {
    let terminal = HarnessTerminal::new(10, 3);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal);

    runtime
        .start()
        .expect("start runtime for visibility-toggle snapshot");
    probe_terminal.take_writes();

    let root_state = ProbeState::new();
    let root_component = ProbeComponent::new(
        vec!["root-0".to_string(), "root-1".to_string()],
        Some(CursorPos { row: 1, col: 8 }),
        root_state.clone(),
    );
    let root_id = runtime.register_component(root_component);
    runtime.set_root(vec![root_id]);
    runtime.set_focus(root_id);
    runtime.run_once();

    let surface_state = ProbeState::new();
    let surface_component = ProbeComponent::new(
        vec!["surface".to_string()],
        Some(CursorPos { row: 0, col: 6 }),
        surface_state.clone(),
    );
    let surface_id = runtime.register_component(surface_component);
    let surface_handle = runtime.show_surface(
        surface_id,
        Some(SurfaceOptions {
            input_policy: SurfaceInputPolicy::Capture,
            kind: SurfaceKind::Modal,
            ..Default::default()
        }),
    );
    runtime.run_once();
    probe_terminal.take_writes();

    runtime.handle_input("a");
    runtime.render_if_needed();

    surface_handle.set_hidden(true);
    runtime.run_once();

    runtime.handle_input("b");
    runtime.render_if_needed();

    surface_handle.set_hidden(false);
    runtime.run_once();

    runtime.handle_input("c");
    runtime.render_if_needed();

    runtime
        .stop()
        .expect("stop runtime for visibility-toggle snapshot");

    VisibilityToggleSnapshot {
        root_events: root_state.events(),
        surface_events: surface_state.events(),
        root_focus_trace: root_state.focus_trace(),
        surface_focus_trace: surface_state.focus_trace(),
        output: probe_terminal.take_writes(),
    }
}

#[test]
fn deterministic_focus_routing_and_cursor_clamp_repeat_cleanly() {
    let baseline = run_focus_routing_snapshot();

    assert_eq!(baseline.surface_events, vec!["text:x".to_string()]);
    assert_eq!(baseline.root_events, vec!["text:y".to_string()]);
    assert_eq!(baseline.surface_focus_trace, vec![true, false]);
    assert!(
        baseline.root_focus_trace.ends_with(&[true, false, true]),
        "expected root focus handoff sequence, got: {:?}",
        baseline.root_focus_trace
    );
    assert!(
        baseline.max_cursor_column <= 5,
        "cursor column exceeded terminal width: {}\noutput={:?}",
        baseline.max_cursor_column,
        baseline.output
    );

    for _ in 1..SOAK_RUNS {
        let rerun = run_focus_routing_snapshot();
        assert_eq!(rerun, baseline);
    }
}

#[test]
fn deterministic_viewport_window_and_cursor_bounds_repeat_cleanly() {
    let baseline = run_viewport_snapshot();

    assert!(
        baseline.output.contains("line-0")
            && baseline.output.contains("line-3")
            && baseline.output.contains("line-5"),
        "expected inline transcript ordering to include all root lines: {:?}",
        baseline.output
    );
    assert!(
        baseline.max_cursor_column <= 6,
        "cursor column exceeded terminal width: {}\noutput={:?}",
        baseline.max_cursor_column,
        baseline.output
    );

    for _ in 1..SOAK_RUNS {
        let rerun = run_viewport_snapshot();
        assert_eq!(rerun, baseline);
    }
}

#[test]
fn deterministic_visibility_toggle_sequence_remains_stable() {
    let baseline = run_visibility_toggle_snapshot();

    assert_eq!(
        baseline.surface_events,
        vec!["text:a".to_string(), "text:c".to_string()]
    );
    assert_eq!(baseline.root_events, vec!["text:b".to_string()]);
    assert!(
        baseline.surface_focus_trace.ends_with(&[true, false, true]),
        "expected stable surface focus sequence, got: {:?}",
        baseline.surface_focus_trace
    );
    assert!(
        baseline.root_focus_trace.contains(&true),
        "expected root focus trace to include restored focus, got: {:?}",
        baseline.root_focus_trace
    );
    assert!(
        baseline.output.contains("surface") && baseline.output.contains("root-"),
        "expected deterministic output transcript to contain both root and surface bytes: {:?}",
        baseline.output
    );

    for _ in 1..SOAK_RUNS {
        let rerun = run_visibility_toggle_snapshot();
        assert_eq!(rerun, baseline);
    }
}

#[test]
fn deterministic_resize_callback_path_remains_stable() {
    let terminal = HarnessTerminal::new(10, 3);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal.clone());

    runtime
        .start()
        .expect("start runtime for resize callback deterministic path");
    probe_terminal.take_writes();

    let root_state = ProbeState::new();
    let root_component = ProbeComponent::new(
        vec!["root".to_string()],
        Some(CursorPos { row: 0, col: 0 }),
        root_state.clone(),
    );
    let root_id = runtime.register_component(root_component);
    runtime.set_root(vec![root_id]);
    runtime.set_focus(root_id);
    runtime.render_now();
    probe_terminal.take_writes();

    let mut baseline = None;
    for _ in 0..SOAK_RUNS {
        terminal.set_size(12, 4);
        terminal.emit_resize();
        runtime.run_once();

        terminal.set_size(10, 3);
        terminal.emit_resize();
        runtime.run_once();

        let events = root_state.events();
        let output = probe_terminal.take_writes();
        let snapshot = (events, output);
        if let Some(previous) = baseline.as_ref() {
            assert_eq!(snapshot, *previous);
        } else {
            baseline = Some(snapshot);
        }

        root_state.events.lock().expect("lock events clear").clear();
    }

    runtime
        .stop()
        .expect("stop runtime for resize callback deterministic path");
}

#[test]
fn deterministic_terminal_input_callback_routes_text_identically() {
    let terminal = HarnessTerminal::new(8, 3);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal.clone());

    runtime
        .start()
        .expect("start runtime for terminal callback input path");
    probe_terminal.take_writes();

    let root_state = ProbeState::new();
    let root_component = ProbeComponent::new(
        vec!["root".to_string()],
        Some(CursorPos { row: 0, col: 0 }),
        root_state.clone(),
    );
    let root_id = runtime.register_component(root_component);
    runtime.set_root(vec![root_id]);
    runtime.set_focus(root_id);
    runtime.render_now();
    probe_terminal.take_writes();

    let mut baseline = None;
    for _ in 0..SOAK_RUNS {
        terminal.emit_input("z");
        runtime.run_once();

        let events = root_state.events();
        let output = probe_terminal.take_writes();
        let snapshot = (events, output);
        if let Some(previous) = baseline.as_ref() {
            assert_eq!(snapshot, *previous);
        } else {
            baseline = Some(snapshot);
        }

        root_state.events.lock().expect("lock events clear").clear();
    }

    runtime
        .stop()
        .expect("stop runtime for terminal callback input path");
}
