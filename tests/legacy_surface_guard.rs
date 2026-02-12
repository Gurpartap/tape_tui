use std::sync::{Arc, Mutex};

use tape_tui::core::terminal::Terminal;
use tape_tui::runtime::Command;
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
        let mut state = self.state.lock().expect("lock terminal state for writes");
        std::mem::take(&mut state.writes)
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

    fn last_focus(&self) -> Option<bool> {
        self.focus_trace
            .lock()
            .expect("lock focus trace")
            .last()
            .copied()
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

#[test]
fn legacy_surface_command_lifecycle_remains_stable() {
    let terminal = HarnessTerminal::new(80, 24);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal);

    runtime.start().expect("runtime start");
    probe_terminal.take_writes();

    let root_state = ProbeState::new();
    let root_id = runtime.register_component(ProbeComponent::new("root", root_state.clone()));
    runtime.set_root(vec![root_id]);
    runtime.set_focus(root_id);
    runtime.run_once();

    let surface_state = ProbeState::new();
    let surface_component_id =
        runtime.register_component(ProbeComponent::new("surface", surface_state.clone()));

    let handle = runtime.runtime_handle();
    let surface_id = handle.alloc_surface_id();

    handle.dispatch(Command::ShowSurface {
        surface_id,
        component: surface_component_id,
        options: Some(SurfaceOptions {
            input_policy: SurfaceInputPolicy::Capture,
            kind: SurfaceKind::Modal,
            ..Default::default()
        }),
        hidden: false,
    });
    handle.dispatch(Command::SetSurfaceHidden {
        surface_id,
        hidden: true,
    });
    handle.dispatch(Command::SetSurfaceHidden {
        surface_id,
        hidden: false,
    });
    handle.dispatch(Command::UpdateSurfaceOptions {
        surface_id,
        options: Some(SurfaceOptions {
            input_policy: SurfaceInputPolicy::Passthrough,
            kind: SurfaceKind::Corner,
            ..Default::default()
        }),
    });

    runtime.run_once();

    assert_eq!(root_state.last_focus(), Some(true));
    assert_eq!(surface_state.last_focus(), Some(false));

    runtime.handle_input("a");
    runtime.render_if_needed();
    assert_eq!(root_state.events(), vec!["text:a".to_string()]);
    assert!(surface_state.events().is_empty());

    handle.dispatch(Command::HideSurface(surface_id));
    runtime.run_once();

    runtime.handle_input("b");
    runtime.render_if_needed();
    assert_eq!(
        root_state.events(),
        vec!["text:a".to_string(), "text:b".to_string()]
    );
    assert!(surface_state.events().is_empty());

    runtime.stop().expect("runtime stop");
}
