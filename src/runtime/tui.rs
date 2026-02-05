//! TUI runtime (Phase 5).

use std::env;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::component::Component;
use crate::core::input::{is_key_release, matches_key};
use crate::core::terminal::Terminal;
use crate::core::terminal_image::{get_capabilities, is_image_line, set_cell_dimensions, CellDimensions};
use crate::render::renderer::DiffRenderer;
use crate::runtime::focus::FocusState;
use crate::runtime::ime::{extract_cursor_position, position_hardware_cursor};

pub struct TuiRuntime<T: Terminal> {
    terminal: T,
    root: Rc<RefCell<Box<dyn Component>>>,
    renderer: DiffRenderer,
    focus: FocusState,
    on_debug: Option<Box<dyn FnMut()>>,
    clear_on_shrink: bool,
    show_hardware_cursor: bool,
    render_requested: bool,
    stopped: bool,
    pending_inputs: Arc<Mutex<Vec<String>>>,
    pending_resize: Arc<AtomicBool>,
    input_buffer: String,
    cell_size_query_pending: bool,
}

impl<T: Terminal> TuiRuntime<T> {
    pub fn new(terminal: T, root: Rc<RefCell<Box<dyn Component>>>) -> Self {
        let clear_on_shrink = env_flag("PI_CLEAR_ON_SHRINK");
        let show_hardware_cursor = env_flag("PI_HARDWARE_CURSOR");
        Self {
            terminal,
            root,
            renderer: DiffRenderer::new(),
            focus: FocusState::new(),
            on_debug: None,
            clear_on_shrink,
            show_hardware_cursor,
            render_requested: false,
            stopped: true,
            pending_inputs: Arc::new(Mutex::new(Vec::new())),
            pending_resize: Arc::new(AtomicBool::new(false)),
            input_buffer: String::new(),
            cell_size_query_pending: false,
        }
    }

    pub fn set_on_debug(&mut self, handler: Option<Box<dyn FnMut()>>) {
        self.on_debug = handler;
    }

    pub fn set_focus(&mut self, target: Rc<RefCell<Box<dyn Component>>>) {
        self.focus.set_focus(Some(target));
    }

    pub fn clear_focus(&mut self) {
        self.focus.clear();
    }

    pub fn start(&mut self) {
        self.stopped = false;
        let input_queue = Arc::clone(&self.pending_inputs);
        let resize_flag = Arc::clone(&self.pending_resize);
        self.terminal.start(
            Box::new(move |data| {
                if let Ok(mut queue) = input_queue.lock() {
                    queue.push(data);
                }
            }),
            Box::new(move || {
                resize_flag.store(true, Ordering::SeqCst);
            }),
        );
        self.terminal.hide_cursor();
        self.query_cell_size();
        self.request_render();
    }

    pub fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.place_cursor_at_end();
        self.terminal.show_cursor();
        self.terminal.stop();
    }

    pub fn run_once(&mut self) {
        if self.stopped {
            return;
        }

        if self.pending_resize.swap(false, Ordering::SeqCst) {
            self.request_render();
        }

        let inputs = {
            let mut queue = match self.pending_inputs.lock() {
                Ok(queue) => queue,
                Err(poisoned) => poisoned.into_inner(),
            };
            queue.drain(..).collect::<Vec<_>>()
        };

        for data in inputs {
            self.handle_input(&data);
        }

        self.render_if_needed();
    }

    pub fn handle_input(&mut self, data: &str) {
        let mut data = data;
        let owned;
        if self.cell_size_query_pending {
            let filtered = self.filter_cell_size_response(data);
            let Some(filtered) = filtered else {
                return;
            };
            if filtered.is_empty() {
                return;
            }
            owned = filtered;
            data = &owned;
        }

        if matches_key(data, "shift+ctrl+d") {
            if let Some(handler) = self.on_debug.as_mut() {
                handler();
            }
            return;
        }

        let Some(component) = self.focus.focused() else {
            return;
        };

        let mut component = component.borrow_mut();
        if is_key_release(data) && !component.wants_key_release() {
            return;
        }

        component.handle_input(data);
        self.request_render();
    }

    pub fn request_render(&mut self) {
        self.render_requested = true;
    }

    pub fn render_if_needed(&mut self) {
        if !self.render_requested {
            return;
        }
        self.render_requested = false;
        self.do_render();
    }

    pub fn render_now(&mut self) {
        self.render_requested = false;
        self.do_render();
    }

    fn do_render(&mut self) {
        let width = self.terminal.columns() as usize;
        let height = self.terminal.rows() as usize;
        let mut root = self.root.borrow_mut();
        let mut lines = root.render(width);
        let total_lines = lines.len();
        let cursor_pos = extract_cursor_position(&mut lines, height);
        self.renderer
            .render(&mut self.terminal, lines, is_image_line, self.clear_on_shrink, false);

        let updated_row = position_hardware_cursor(
            &mut self.terminal,
            cursor_pos,
            total_lines,
            self.renderer.hardware_cursor_row(),
            self.show_hardware_cursor,
        );
        self.renderer.set_hardware_cursor_row(updated_row);
    }

    fn place_cursor_at_end(&mut self) {
        let total_lines = self.renderer.previous_lines_len();
        if total_lines == 0 {
            return;
        }
        let target_row = total_lines;
        let current_row = self.renderer.hardware_cursor_row();
        let diff = target_row as i32 - current_row as i32;
        if diff > 0 {
            self.terminal.write(&format!("\x1b[{}B", diff));
        } else if diff < 0 {
            self.terminal.write(&format!("\x1b[{}A", -diff));
        }
        self.terminal.write("\r\n");
        self.renderer.set_hardware_cursor_row(target_row);
    }

    fn query_cell_size(&mut self) {
        if get_capabilities().images.is_none() {
            return;
        }
        self.cell_size_query_pending = true;
        self.terminal.write("\x1b[16t");
    }

    fn filter_cell_size_response(&mut self, data: &str) -> Option<String> {
        self.input_buffer.push_str(data);

        if let Some((start, end, height_px, width_px)) = find_cell_size_response(&self.input_buffer) {
            if height_px > 0 && width_px > 0 {
                set_cell_dimensions(CellDimensions {
                    width_px,
                    height_px,
                });
                {
                    let mut root = self.root.borrow_mut();
                    root.invalidate();
                }
                self.request_render();
            }
            self.input_buffer.replace_range(start..end, "");
            self.cell_size_query_pending = false;
        }

        if self.cell_size_query_pending && is_partial_cell_size(&self.input_buffer) {
            return None;
        }

        let result = self.input_buffer.clone();
        self.input_buffer.clear();
        self.cell_size_query_pending = false;
        Some(result)
    }
}

fn env_flag(name: &str) -> bool {
    env::var(name).map(|value| value == "1").unwrap_or(false)
}

fn find_cell_size_response(buffer: &str) -> Option<(usize, usize, u32, u32)> {
    let bytes = buffer.as_bytes();
    let mut i = 0;
    while i + 4 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' && bytes[i + 2] == b'6' && bytes[i + 3] == b';' {
            let mut j = i + 4;
            let mut height: u32 = 0;
            let mut has_height = false;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                height = height.saturating_mul(10).saturating_add((bytes[j] - b'0') as u32);
                has_height = true;
                j += 1;
            }
            if !has_height || j >= bytes.len() || bytes[j] != b';' {
                i += 1;
                continue;
            }
            j += 1;
            let mut width: u32 = 0;
            let mut has_width = false;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                width = width.saturating_mul(10).saturating_add((bytes[j] - b'0') as u32);
                has_width = true;
                j += 1;
            }
            if !has_width || j >= bytes.len() || bytes[j] != b't' {
                i += 1;
                continue;
            }
            return Some((i, j + 1, height, width));
        }
        i += 1;
    }
    None
}

fn is_partial_cell_size(buffer: &str) -> bool {
    let Some(start) = buffer.rfind("\x1b[6") else {
        return false;
    };
    let tail = &buffer[start..];
    if tail.contains('t') {
        return false;
    }
    tail.chars()
        .all(|ch| ch == '\x1b' || ch == '[' || ch == '6' || ch == ';' || ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{find_cell_size_response, TuiRuntime};
    use crate::core::component::Component;
    use crate::core::terminal_image::{get_cell_dimensions, reset_capabilities_cache};
    use crate::core::terminal::Terminal;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Mutex, OnceLock};

    #[derive(Default)]
    struct TestTerminal {
        output: String,
    }

    impl Terminal for TestTerminal {
        fn start(&mut self, _on_input: Box<dyn FnMut(String) + Send>, _on_resize: Box<dyn FnMut() + Send>) {}
        fn stop(&mut self) {}
        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
        fn write(&mut self, data: &str) {
            self.output.push_str(data);
        }
        fn columns(&self) -> u16 {
            80
        }
        fn rows(&self) -> u16 {
            24
        }
        fn kitty_protocol_active(&self) -> bool {
            false
        }
        fn move_by(&mut self, _lines: i32) {}
        fn hide_cursor(&mut self) {}
        fn show_cursor(&mut self) {}
        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {}
        fn set_title(&mut self, _title: &str) {}
    }

    #[derive(Default)]
    struct DummyComponent;

    impl Component for DummyComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }
    }

    #[derive(Default)]
    struct RenderState {
        renders: usize,
        invalidates: usize,
    }

    struct CountingComponent {
        state: Rc<RefCell<RenderState>>,
    }

    impl Component for CountingComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.state.borrow_mut().renders += 1;
            Vec::new()
        }

        fn invalidate(&mut self) {
            self.state.borrow_mut().invalidates += 1;
        }
    }

    struct TestComponent {
        inputs: Rc<RefCell<Vec<String>>>,
        wants_release: bool,
    }

    impl TestComponent {
        fn new(wants_release: bool, inputs: Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                inputs,
                wants_release,
            }
        }
    }

    impl Component for TestComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }

        fn handle_input(&mut self, data: &str) {
            self.inputs.borrow_mut().push(data.to_string());
        }

        fn wants_key_release(&self) -> bool {
            self.wants_release
        }
    }

    #[test]
    fn key_release_filtered_unless_requested() {
        let terminal = TestTerminal::default();
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(DummyComponent::default())));
        let mut runtime = TuiRuntime::new(terminal, root);

        let inputs = Rc::new(RefCell::new(Vec::new()));
        let component = TestComponent::new(false, Rc::clone(&inputs));
        let component_handle: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(component)));
        runtime.set_focus(component_handle);
        runtime.handle_input("\x1b[32:3u");
        assert!(inputs.borrow().is_empty());

        let inputs_release = Rc::new(RefCell::new(Vec::new()));
        let component_release = TestComponent::new(true, Rc::clone(&inputs_release));
        let component_release_handle: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(component_release)));
        runtime.set_focus(component_release_handle);
        runtime.handle_input("\x1b[32:3u");
        assert_eq!(inputs_release.borrow().len(), 1);
    }

    #[test]
    fn parse_cell_size_response_extracts_dimensions() {
        let data = "\x1b[6;18;9t";
        let parsed = find_cell_size_response(data);
        assert_eq!(parsed, Some((0, data.len(), 18, 9)));
    }

    #[test]
    fn cell_size_query_triggers_invalidate_and_render() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        reset_capabilities_cache();
        std::env::set_var("TERM_PROGRAM", "kitty");

        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(component)));
        let mut runtime = TuiRuntime::new(terminal, root);

        runtime.start();
        assert!(runtime.terminal.output.contains("\x1b[16t"));
        runtime.render_if_needed();
        assert_eq!(state.borrow().renders, 1);

        runtime.handle_input("\x1b[6;20;10t");
        runtime.render_if_needed();
        assert_eq!(state.borrow().invalidates, 1);
        assert_eq!(state.borrow().renders, 2);

        let dims = get_cell_dimensions();
        assert_eq!(dims.width_px, 10);
        assert_eq!(dims.height_px, 20);

        std::env::remove_var("TERM_PROGRAM");
        reset_capabilities_cache();
    }

    fn env_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
}
