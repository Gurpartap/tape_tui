//! TUI runtime (Phase 5).

use std::env;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::component::Component;
use crate::core::input::{is_key_release, matches_key};
use crate::core::terminal::Terminal;
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
}

fn env_flag(name: &str) -> bool {
    env::var(name).map(|value| value == "1").unwrap_or(false)
}

fn is_image_line(_line: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::TuiRuntime;
    use crate::core::component::Component;
    use crate::core::terminal::Terminal;
    use std::cell::RefCell;
    use std::rc::Rc;

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
}
