use std::sync::{Arc, Mutex};

use tape_tui::core::terminal::Terminal;
use tape_tui::{
    Component, SurfaceInputPolicy, SurfaceKind, SurfaceLayoutOptions, SurfaceOptions,
    SurfaceSizeValue, TUI,
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

    fn set_size(&self, columns: u16, rows: u16) {
        let mut state = self.state.lock().expect("lock terminal size");
        state.columns = columns;
        state.rows = rows;
    }

    fn emit_resize(&self) {
        let mut state = self.state.lock().expect("lock terminal resize callback");
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

#[derive(Default)]
struct RootComponent;

impl Component for RootComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec!["root".to_string()]
    }
}

struct ViewportProbeComponent {
    lines: Vec<String>,
    viewport_calls: Arc<Mutex<Vec<(usize, usize)>>>,
}

impl ViewportProbeComponent {
    fn new(lines: Vec<String>, viewport_calls: Arc<Mutex<Vec<(usize, usize)>>>) -> Self {
        Self {
            lines,
            viewport_calls,
        }
    }
}

impl Component for ViewportProbeComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }

    fn set_viewport_size(&mut self, cols: usize, rows: usize) {
        self.viewport_calls
            .lock()
            .expect("lock viewport calls")
            .push((cols, rows));
    }
}

fn current_calls(calls: &Arc<Mutex<Vec<(usize, usize)>>>) -> Vec<(usize, usize)> {
    calls.lock().expect("lock viewport calls for snapshot").clone()
}

fn toast_options() -> SurfaceOptions {
    SurfaceOptions {
        input_policy: SurfaceInputPolicy::Passthrough,
        kind: SurfaceKind::Toast,
        layout: SurfaceLayoutOptions {
            width: Some(SurfaceSizeValue::absolute(8)),
            max_height: Some(SurfaceSizeValue::percent(100.0)),
            ..Default::default()
        },
    }
}

#[test]
fn small_terminal_two_pass_allocation_clamps_late_lanes_to_zero_budget() {
    let terminal = HarnessTerminal::new(9, 3);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal);

    runtime.start().expect("runtime start");
    probe_terminal.take_writes();

    let root_id = runtime.register_component(RootComponent);
    runtime.set_root(vec![root_id]);

    let toast_a_calls = Arc::new(Mutex::new(Vec::new()));
    let toast_a = runtime.register_component(ViewportProbeComponent::new(
        vec!["toast-a".to_string()],
        Arc::clone(&toast_a_calls),
    ));

    let toast_b_calls = Arc::new(Mutex::new(Vec::new()));
    let toast_b = runtime.register_component(ViewportProbeComponent::new(
        vec!["toast-b".to_string()],
        Arc::clone(&toast_b_calls),
    ));

    let drawer_calls = Arc::new(Mutex::new(Vec::new()));
    let drawer = runtime.register_component(ViewportProbeComponent::new(
        vec!["drawer".to_string()],
        Arc::clone(&drawer_calls),
    ));

    runtime.show_surface(toast_a, Some(toast_options()));
    runtime.show_surface(toast_b, Some(toast_options()));
    runtime.show_surface(
        drawer,
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
    runtime.stop().expect("runtime stop");

    assert_eq!(current_calls(&toast_a_calls), vec![(8, 3)]);
    assert_eq!(current_calls(&toast_b_calls), vec![(8, 0)]);
    assert_eq!(current_calls(&drawer_calls), vec![(8, 0)]);
}

#[test]
fn hidden_surfaces_are_excluded_from_budget_until_shown_again() {
    let terminal = HarnessTerminal::new(10, 4);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal);

    runtime.start().expect("runtime start");
    probe_terminal.take_writes();

    let root_id = runtime.register_component(RootComponent);
    runtime.set_root(vec![root_id]);

    let hidden_calls = Arc::new(Mutex::new(Vec::new()));
    let hidden_surface = runtime.register_component(ViewportProbeComponent::new(
        vec!["hidden".to_string()],
        Arc::clone(&hidden_calls),
    ));

    let visible_calls = Arc::new(Mutex::new(Vec::new()));
    let visible_surface = runtime.register_component(ViewportProbeComponent::new(
        vec!["visible".to_string()],
        Arc::clone(&visible_calls),
    ));

    let hidden_handle = runtime.show_surface(hidden_surface, Some(toast_options()));
    runtime.show_surface(visible_surface, Some(toast_options()));
    hidden_handle.set_hidden(true);

    runtime.run_once();

    assert!(current_calls(&hidden_calls).is_empty());
    assert_eq!(current_calls(&visible_calls), vec![(8, 4)]);

    hidden_handle.show();
    runtime.run_once();
    runtime.stop().expect("runtime stop");

    assert_eq!(current_calls(&hidden_calls), vec![(8, 0)]);
    assert_eq!(current_calls(&visible_calls), vec![(8, 4), (8, 4)]);
}

#[test]
fn resize_recomputes_surface_budget_deterministically() {
    let terminal = HarnessTerminal::new(12, 6);
    let probe_terminal = terminal.clone();
    let mut runtime = TUI::new(terminal.clone());

    runtime.start().expect("runtime start");
    probe_terminal.take_writes();

    let root_id = runtime.register_component(RootComponent);
    runtime.set_root(vec![root_id]);

    let calls = Arc::new(Mutex::new(Vec::new()));
    let surface_id = runtime.register_component(ViewportProbeComponent::new(
        vec!["surface".to_string()],
        Arc::clone(&calls),
    ));

    runtime.show_surface(
        surface_id,
        Some(SurfaceOptions {
            input_policy: SurfaceInputPolicy::Passthrough,
            kind: SurfaceKind::Modal,
            layout: SurfaceLayoutOptions {
                width: Some(SurfaceSizeValue::absolute(10)),
                max_height: Some(SurfaceSizeValue::percent(50.0)),
                ..Default::default()
            },
        }),
    );
    runtime.run_once();

    terminal.set_size(12, 4);
    terminal.emit_resize();
    runtime.run_once();

    terminal.set_size(12, 6);
    terminal.emit_resize();
    runtime.run_once();
    runtime.stop().expect("runtime stop");

    assert_eq!(current_calls(&calls), vec![(10, 3), (10, 2), (10, 3)]);
}
