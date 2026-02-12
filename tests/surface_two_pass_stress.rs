use std::sync::{Arc, Mutex};

use tape_tui::core::terminal::Terminal;
use tape_tui::{
    Component, Focusable, InputEvent, SurfaceInputPolicy, SurfaceKind, SurfaceLayoutOptions,
    SurfaceOptions, SurfaceSizeValue, TUI,
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
        let mut state = self.state.lock().expect("lock terminal writes");
        std::mem::take(&mut state.writes)
    }

    fn set_size(&self, columns: u16, rows: u16) {
        let mut state = self.state.lock().expect("lock terminal size");
        state.columns = columns;
        state.rows = rows;
    }

    fn emit_resize(&self) {
        let mut state = self.state.lock().expect("lock resize callback");
        if let Some(callback) = state.on_resize.as_mut() {
            callback();
        }
    }
}

impl Terminal for HarnessTerminal {
    fn start(
        &mut self,
        on_input: Box<dyn FnMut(String) + Send>,
        on_resize: Box<dyn FnMut() + Send>,
    ) -> std::io::Result<()> {
        let mut state = self.state.lock().expect("lock terminal start");
        state.on_input = Some(on_input);
        state.on_resize = Some(on_resize);
        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<()> {
        let mut state = self.state.lock().expect("lock terminal stop");
        state.on_input = None;
        state.on_resize = None;
        Ok(())
    }

    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}

    fn write(&mut self, data: &str) {
        let mut state = self.state.lock().expect("lock terminal write");
        state.writes.push_str(data);
    }

    fn columns(&self) -> u16 {
        let state = self.state.lock().expect("lock terminal columns");
        state.columns
    }

    fn rows(&self) -> u16 {
        let state = self.state.lock().expect("lock terminal rows");
        state.rows
    }
}

#[derive(Clone)]
struct ProbeState {
    events: Arc<Mutex<Vec<String>>>,
    focus_trace: Arc<Mutex<Vec<bool>>>,
    viewport_calls: Arc<Mutex<Vec<(usize, usize)>>>,
}

impl ProbeState {
    fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            focus_trace: Arc::new(Mutex::new(Vec::new())),
            viewport_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().expect("lock events").clone()
    }

    fn focus_trace(&self) -> Vec<bool> {
        self.focus_trace.lock().expect("lock focus trace").clone()
    }

    fn viewport_calls(&self) -> Vec<(usize, usize)> {
        self.viewport_calls
            .lock()
            .expect("lock viewport calls")
            .clone()
    }
}

struct RootComponent;

impl Component for RootComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["root".to_string()]
    }
}

struct ViewportProbeComponent {
    lines: Vec<String>,
    state: ProbeState,
}

impl ViewportProbeComponent {
    fn new(lines: Vec<String>, state: ProbeState) -> Self {
        Self { lines, state }
    }
}

impl Component for ViewportProbeComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }

    fn set_viewport_size(&mut self, cols: usize, rows: usize) {
        self.state
            .viewport_calls
            .lock()
            .expect("lock viewport calls for push")
            .push((cols, rows));
    }
}

struct FocusProbeComponent {
    label: &'static str,
    focused: bool,
    state: ProbeState,
}

impl FocusProbeComponent {
    fn new(label: &'static str, state: ProbeState) -> Self {
        Self {
            label,
            focused: false,
            state,
        }
    }
}

impl Component for FocusProbeComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![self.label.to_string()]
    }

    fn set_viewport_size(&mut self, cols: usize, rows: usize) {
        self.state
            .viewport_calls
            .lock()
            .expect("lock viewport calls for push")
            .push((cols, rows));
    }

    fn handle_event(&mut self, event: &InputEvent) {
        if let InputEvent::Text { text, .. } = event {
            self.state
                .events
                .lock()
                .expect("lock events for push")
                .push(format!("text:{text}"));
        }
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for FocusProbeComponent {
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

fn toast_options(input_policy: SurfaceInputPolicy) -> SurfaceOptions {
    SurfaceOptions {
        input_policy,
        kind: SurfaceKind::Toast,
        layout: SurfaceLayoutOptions {
            width: Some(SurfaceSizeValue::absolute(8)),
            max_height: Some(SurfaceSizeValue::percent(100.0)),
            ..Default::default()
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AllocationStressSnapshot {
    toast_a_viewports: Vec<(usize, usize)>,
    toast_b_viewports: Vec<(usize, usize)>,
    drawer_viewports: Vec<(usize, usize)>,
    output: String,
}

fn run_allocation_stress_snapshot() -> AllocationStressSnapshot {
    let terminal = HarnessTerminal::new(10, 3);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal.clone());

    runtime.start().expect("runtime start");
    probe_terminal.take_writes();

    let root_id = runtime.register_component(RootComponent);
    runtime.set_root(vec![root_id]);

    let toast_a_state = ProbeState::new();
    let toast_a_component = runtime.register_component(ViewportProbeComponent::new(
        vec!["toast-a".to_string()],
        toast_a_state.clone(),
    ));
    let toast_a = runtime.show_surface(
        toast_a_component,
        Some(toast_options(SurfaceInputPolicy::Passthrough)),
    );

    let toast_b_state = ProbeState::new();
    let toast_b_component = runtime.register_component(ViewportProbeComponent::new(
        vec!["toast-b".to_string()],
        toast_b_state.clone(),
    ));
    runtime.show_surface(
        toast_b_component,
        Some(toast_options(SurfaceInputPolicy::Passthrough)),
    );

    let drawer_state = ProbeState::new();
    let drawer_component = runtime.register_component(ViewportProbeComponent::new(
        vec!["drawer".to_string()],
        drawer_state.clone(),
    ));
    runtime.show_surface(
        drawer_component,
        Some(SurfaceOptions {
            input_policy: SurfaceInputPolicy::Passthrough,
            kind: SurfaceKind::Drawer,
            layout: SurfaceLayoutOptions {
                width: Some(SurfaceSizeValue::absolute(8)),
                max_height: Some(SurfaceSizeValue::percent(100.0)),
                ..Default::default()
            },
        }),
    );

    runtime.run_once();

    terminal.set_size(10, 4);
    terminal.emit_resize();
    runtime.run_once();

    toast_a.set_hidden(true);
    runtime.run_once();

    toast_a.set_hidden(false);
    runtime.run_once();

    terminal.set_size(10, 2);
    terminal.emit_resize();
    runtime.run_once();

    runtime.stop().expect("runtime stop");

    AllocationStressSnapshot {
        toast_a_viewports: toast_a_state.viewport_calls(),
        toast_b_viewports: toast_b_state.viewport_calls(),
        drawer_viewports: drawer_state.viewport_calls(),
        output: probe_terminal.take_writes(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FocusChurnSnapshot {
    root_events: Vec<String>,
    surface_a_events: Vec<String>,
    surface_b_events: Vec<String>,
    root_focus_trace: Vec<bool>,
    surface_a_focus_trace: Vec<bool>,
    surface_b_focus_trace: Vec<bool>,
    output: String,
}

fn run_focus_churn_snapshot() -> FocusChurnSnapshot {
    let terminal = HarnessTerminal::new(9, 3);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal.clone());

    runtime.start().expect("runtime start");
    probe_terminal.take_writes();

    let root_state = ProbeState::new();
    let root_id = runtime.register_component(FocusProbeComponent::new("root", root_state.clone()));
    runtime.set_root(vec![root_id]);
    runtime.set_focus(root_id);

    let surface_a_state = ProbeState::new();
    let surface_a_id =
        runtime.register_component(FocusProbeComponent::new("surface-a", surface_a_state.clone()));
    runtime.show_surface(surface_a_id, Some(toast_options(SurfaceInputPolicy::Capture)));

    let surface_b_state = ProbeState::new();
    let surface_b_id =
        runtime.register_component(FocusProbeComponent::new("surface-b", surface_b_state.clone()));
    let surface_b = runtime.show_surface(
        surface_b_id,
        Some(toast_options(SurfaceInputPolicy::Capture)),
    );

    runtime.run_once();
    probe_terminal.take_writes();

    runtime.handle_input("1");
    runtime.render_if_needed();

    terminal.set_size(9, 2);
    terminal.emit_resize();
    runtime.run_once();

    runtime.handle_input("2");
    runtime.render_if_needed();

    surface_b.set_hidden(true);
    runtime.run_once();

    runtime.handle_input("3");
    runtime.render_if_needed();

    surface_b.set_hidden(false);
    runtime.run_once();

    runtime.handle_input("4");
    runtime.render_if_needed();

    runtime.stop().expect("runtime stop");

    FocusChurnSnapshot {
        root_events: root_state.events(),
        surface_a_events: surface_a_state.events(),
        surface_b_events: surface_b_state.events(),
        root_focus_trace: root_state.focus_trace(),
        surface_a_focus_trace: surface_a_state.focus_trace(),
        surface_b_focus_trace: surface_b_state.focus_trace(),
        output: probe_terminal.take_writes(),
    }
}

fn contains_subsequence(trace: &[bool], expected: &[bool]) -> bool {
    if expected.is_empty() {
        return true;
    }
    trace
        .windows(expected.len())
        .any(|window| window == expected)
}

#[test]
fn tiny_terminal_allocation_churn_repeats_identically() {
    let baseline = run_allocation_stress_snapshot();

    assert_eq!(baseline.toast_a_viewports[0], (8, 3));
    assert_eq!(baseline.toast_b_viewports[0], (8, 0));
    assert!(
        baseline.toast_b_viewports.contains(&(8, 4)),
        "expected toast-b to gain budget when toast-a is hidden: {:?}",
        baseline.toast_b_viewports
    );
    assert!(
        baseline
            .drawer_viewports
            .iter()
            .all(|(_cols, rows)| *rows <= 4),
        "drawer rows should always stay bounded by terminal height: {:?}",
        baseline.drawer_viewports
    );

    for _ in 1..SOAK_RUNS {
        let rerun = run_allocation_stress_snapshot();
        assert_eq!(rerun, baseline);
    }
}

#[test]
fn focus_and_input_routing_stays_stable_during_tiny_terminal_budget_churn() {
    let baseline = run_focus_churn_snapshot();

    assert_eq!(baseline.root_events, Vec::<String>::new());
    assert_eq!(baseline.surface_a_events, vec!["text:3".to_string()]);
    assert_eq!(
        baseline.surface_b_events,
        vec!["text:1".to_string(), "text:2".to_string(), "text:4".to_string()]
    );
    assert!(
        contains_subsequence(&baseline.surface_b_focus_trace, &[true, false, true]),
        "expected stable surface-b focus handoff sequence, got: {:?}",
        baseline.surface_b_focus_trace
    );
    assert!(
        contains_subsequence(&baseline.surface_a_focus_trace, &[true, false, true, false]),
        "expected stable surface-a focus sequence, got: {:?}",
        baseline.surface_a_focus_trace
    );

    for _ in 1..SOAK_RUNS {
        let rerun = run_focus_churn_snapshot();
        assert_eq!(rerun, baseline);
    }
}
