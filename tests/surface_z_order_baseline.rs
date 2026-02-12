use std::sync::{Arc, Mutex};

use tape_tui::core::terminal::Terminal;
use tape_tui::{
    Component, Focusable, InputEvent, SurfaceInputPolicy, SurfaceKind, SurfaceOptions, TUI,
};

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
}

impl ProbeState {
    fn new() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            focus_trace: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().expect("lock probe events").clone()
    }

    fn focus_trace(&self) -> Vec<bool> {
        self.focus_trace
            .lock()
            .expect("lock probe focus trace")
            .clone()
    }
}

struct ProbeComponent {
    label: &'static str,
    focused: bool,
    state: ProbeState,
}

impl ProbeComponent {
    fn new(label: &'static str, state: ProbeState) -> Self {
        Self {
            label,
            focused: false,
            state,
        }
    }
}

impl Component for ProbeComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![self.label.to_string()]
    }

    fn handle_event(&mut self, event: &InputEvent) {
        if let InputEvent::Text { text, .. } = event {
            self.state
                .events
                .lock()
                .expect("lock probe events for push")
                .push(format!("text:{text}"));
        }
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
            .expect("lock probe focus trace for push")
            .push(focused);
    }

    fn is_focused(&self) -> bool {
        self.focused
    }
}

fn surface_options(input_policy: SurfaceInputPolicy) -> SurfaceOptions {
    SurfaceOptions {
        input_policy,
        kind: SurfaceKind::Modal,
        ..Default::default()
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
fn insertion_order_baseline_drives_capture_precedence() {
    let terminal = HarnessTerminal::new(20, 5);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal);

    runtime.start().expect("start runtime");
    probe_terminal.take_writes();

    let root_state = ProbeState::new();
    let root_id = runtime.register_component(ProbeComponent::new("root", root_state.clone()));
    runtime.set_root(vec![root_id]);
    runtime.set_focus(root_id);

    let surface_a_state = ProbeState::new();
    let surface_a_component =
        runtime.register_component(ProbeComponent::new("surface-a", surface_a_state.clone()));
    let surface_a = runtime.show_surface(
        surface_a_component,
        Some(surface_options(SurfaceInputPolicy::Capture)),
    );

    let surface_b_state = ProbeState::new();
    let surface_b_component =
        runtime.register_component(ProbeComponent::new("surface-b", surface_b_state.clone()));
    let surface_b = runtime.show_surface(
        surface_b_component,
        Some(surface_options(SurfaceInputPolicy::Capture)),
    );

    let surface_passthrough_state = ProbeState::new();
    let surface_passthrough_component = runtime.register_component(ProbeComponent::new(
        "surface-passthrough",
        surface_passthrough_state.clone(),
    ));
    runtime.show_surface(
        surface_passthrough_component,
        Some(surface_options(SurfaceInputPolicy::Passthrough)),
    );

    runtime.run_once();
    probe_terminal.take_writes();

    runtime.handle_input("x");
    runtime.render_if_needed();

    surface_b.set_hidden(true);
    runtime.run_once();

    runtime.handle_input("y");
    runtime.render_if_needed();

    surface_a.set_hidden(true);
    runtime.run_once();

    runtime.handle_input("z");
    runtime.render_if_needed();

    runtime.stop().expect("stop runtime");

    assert_eq!(surface_b_state.events(), vec!["text:x".to_string()]);
    assert_eq!(surface_a_state.events(), vec!["text:y".to_string()]);
    assert!(
        surface_passthrough_state.events().is_empty(),
        "passthrough surface should never capture input"
    );
    assert_eq!(root_state.events(), vec!["text:z".to_string()]);
}

#[test]
fn focus_handoff_baseline_remains_stable_across_hide_show_cycles() {
    let terminal = HarnessTerminal::new(20, 5);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal);

    runtime.start().expect("start runtime");
    probe_terminal.take_writes();

    let root_state = ProbeState::new();
    let root_id = runtime.register_component(ProbeComponent::new("root", root_state.clone()));
    runtime.set_root(vec![root_id]);
    runtime.set_focus(root_id);

    let surface_a_state = ProbeState::new();
    let surface_a_component =
        runtime.register_component(ProbeComponent::new("surface-a", surface_a_state.clone()));
    let surface_a = runtime.show_surface(
        surface_a_component,
        Some(surface_options(SurfaceInputPolicy::Capture)),
    );

    let surface_b_state = ProbeState::new();
    let surface_b_component =
        runtime.register_component(ProbeComponent::new("surface-b", surface_b_state.clone()));
    let surface_b = runtime.show_surface(
        surface_b_component,
        Some(surface_options(SurfaceInputPolicy::Capture)),
    );

    runtime.run_once();
    probe_terminal.take_writes();

    runtime.handle_input("1");
    runtime.render_if_needed();

    surface_b.set_hidden(true);
    runtime.run_once();
    runtime.handle_input("2");
    runtime.render_if_needed();

    surface_a.set_hidden(true);
    runtime.run_once();
    runtime.handle_input("3");
    runtime.render_if_needed();

    surface_b.set_hidden(false);
    runtime.run_once();
    runtime.handle_input("4");
    runtime.render_if_needed();

    runtime.stop().expect("stop runtime");

    assert_eq!(
        surface_b_state.events(),
        vec!["text:1".to_string(), "text:4".to_string()]
    );
    assert_eq!(surface_a_state.events(), vec!["text:2".to_string()]);
    assert_eq!(root_state.events(), vec!["text:3".to_string()]);

    let surface_b_focus = surface_b_state.focus_trace();
    assert!(
        contains_subsequence(&surface_b_focus, &[true, false, true]),
        "expected surface-b focus to restore after hide/show cycle, got: {:?}",
        surface_b_focus
    );

    let root_focus = root_state.focus_trace();
    assert!(
        contains_subsequence(&root_focus, &[true, false, true]),
        "expected root focus to regain focus after all surfaces are hidden, got: {:?}",
        root_focus
    );
}
