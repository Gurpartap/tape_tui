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
use crate::render::overlay::{composite_overlays, resolve_overlay_layout, OverlayOptions, RenderedOverlay};
use crate::render::renderer::DiffRenderer;
use crate::runtime::focus::FocusState;
use crate::runtime::ime::{extract_cursor_position, position_hardware_cursor};

const STOP_DRAIN_MAX_MS: u64 = 1000;
const STOP_DRAIN_IDLE_MS: u64 = 50;

pub struct TuiRuntime<T: Terminal> {
    terminal: T,
    root: Rc<RefCell<Box<dyn Component>>>,
    renderer: DiffRenderer,
    focus: FocusState,
    overlays: Rc<RefCell<OverlayState>>,
    on_debug: Option<Box<dyn FnMut()>>,
    clear_on_shrink: bool,
    show_hardware_cursor: bool,
    render_requested: Arc<AtomicBool>,
    stopped: bool,
    pending_inputs: Arc<Mutex<Vec<String>>>,
    pending_resize: Arc<AtomicBool>,
    input_buffer: String,
    cell_size_query_pending: bool,
}

#[derive(Default)]
struct OverlayState {
    entries: Vec<OverlayEntry>,
    next_id: u64,
    dirty: bool,
    pending_pre_focus: Option<Rc<RefCell<Box<dyn Component>>>>,
}

struct OverlayEntry {
    id: u64,
    component: Rc<RefCell<Box<dyn Component>>>,
    options: Option<OverlayOptions>,
    pre_focus: Option<Rc<RefCell<Box<dyn Component>>>>,
    hidden: bool,
}

pub struct OverlayHandle {
    id: u64,
    state: std::rc::Weak<RefCell<OverlayState>>,
}

#[derive(Clone)]
pub struct RenderHandle {
    requested: Arc<AtomicBool>,
}

impl RenderHandle {
    pub fn request_render(&self) {
        self.requested.store(true, Ordering::SeqCst);
    }
}

impl OverlayHandle {
    pub fn hide(&self) {
        if let Some(state) = self.state.upgrade() {
            let mut state = state.borrow_mut();
            if let Some(index) = state.entries.iter().position(|entry| entry.id == self.id) {
                let entry = state.entries.remove(index);
                if let Some(pre_focus) = entry.pre_focus {
                    state.pending_pre_focus = Some(pre_focus);
                }
                state.dirty = true;
            }
        }
    }

    pub fn set_hidden(&self, hidden: bool) {
        if let Some(state) = self.state.upgrade() {
            let mut state = state.borrow_mut();
            if let Some(entry) = state.entries.iter_mut().find(|entry| entry.id == self.id) {
                if entry.hidden != hidden {
                    entry.hidden = hidden;
                    state.dirty = true;
                }
            }
        }
    }

    pub fn is_hidden(&self) -> bool {
        if let Some(state) = self.state.upgrade() {
            let state = state.borrow();
            if let Some(entry) = state.entries.iter().find(|entry| entry.id == self.id) {
                return entry.hidden;
            }
        }
        false
    }
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
            overlays: Rc::new(RefCell::new(OverlayState::default())),
            on_debug: None,
            clear_on_shrink,
            show_hardware_cursor,
            render_requested: Arc::new(AtomicBool::new(false)),
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

    pub fn render_handle(&self) -> RenderHandle {
        RenderHandle {
            requested: Arc::clone(&self.render_requested),
        }
    }

    pub fn set_focus(&mut self, target: Rc<RefCell<Box<dyn Component>>>) {
        self.focus.set_focus(Some(target));
    }

    pub fn clear_focus(&mut self) {
        self.focus.clear();
    }

    pub fn show_overlay(
        &mut self,
        component: Rc<RefCell<Box<dyn Component>>>,
        options: Option<OverlayOptions>,
    ) -> OverlayHandle {
        let pre_focus = self.focus.focused();
        let entry = OverlayEntry {
            id: 0,
            component: Rc::clone(&component),
            options,
            pre_focus,
            hidden: false,
        };
        let visible = self.is_overlay_visible(&entry);
        let mut state = self.overlays.borrow_mut();
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        let mut entry = entry;
        entry.id = id;
        state.entries.push(entry);
        state.dirty = true;
        drop(state);
        if visible {
            self.focus.set_focus(Some(component));
        }
        self.request_render();
        OverlayHandle {
            id,
            state: Rc::downgrade(&self.overlays),
        }
    }

    pub fn hide_overlay(&mut self) {
        let mut state = self.overlays.borrow_mut();
        let entry = state.entries.pop();
        if entry.is_some() {
            state.dirty = true;
        }
        drop(state);
        if entry.is_some() {
            self.reconcile_focus();
            self.request_render();
        }
    }

    pub fn has_overlay(&self) -> bool {
        let state = self.overlays.borrow();
        state.entries.iter().any(|entry| self.is_overlay_visible(entry))
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
        self.place_cursor_at_end();
        self.terminal.show_cursor();
        self.terminal
            .drain_input(STOP_DRAIN_MAX_MS, STOP_DRAIN_IDLE_MS);
        self.terminal.stop();
        self.stopped = true;
    }

    pub fn run_once(&mut self) {
        if self.stopped {
            return;
        }

        if self.take_overlay_dirty() {
            self.reconcile_focus();
            self.request_render();
        }

        if self.pending_resize.swap(false, Ordering::SeqCst) {
            self.reconcile_focus();
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
        self.reconcile_focus();

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
        self.render_requested.store(true, Ordering::SeqCst);
    }

    pub fn render_if_needed(&mut self) {
        if !self.render_requested.swap(false, Ordering::SeqCst) {
            return;
        }
        self.do_render();
    }

    pub fn render_now(&mut self) {
        self.render_requested.store(false, Ordering::SeqCst);
        self.do_render();
    }

    fn do_render(&mut self) {
        let width = self.terminal.columns() as usize;
        let height = self.terminal.rows() as usize;
        self.reconcile_focus();
        let mut lines = {
            let mut root = self.root.borrow_mut();
            root.set_terminal_rows(height);
            root.render(width)
        };

        if self.has_overlay() {
            lines = self.composite_overlay_lines(lines, width, height);
        }

        let total_lines = lines.len();
        let cursor_pos = extract_cursor_position(&mut lines, height);
        let has_overlays = self.has_overlay();
        self.renderer
            .render(&mut self.terminal, lines, is_image_line, self.clear_on_shrink, has_overlays);

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

    fn composite_overlay_lines(&mut self, lines: Vec<String>, width: usize, height: usize) -> Vec<String> {
        let overlays = {
            let state = self.overlays.borrow();
            let mut rendered = Vec::new();
            for entry in state.entries.iter() {
                if !self.is_overlay_visible(entry) {
                    continue;
                }
                let layout = resolve_overlay_layout(entry.options.as_ref(), 0, width, height);
                let mut component = entry.component.borrow_mut();
                component.set_terminal_rows(height);
                let mut overlay_lines = component.render(layout.width);
                if let Some(max_height) = layout.max_height {
                    if overlay_lines.len() > max_height {
                        overlay_lines.truncate(max_height);
                    }
                }
                let final_layout =
                    resolve_overlay_layout(entry.options.as_ref(), overlay_lines.len(), width, height);
                rendered.push(RenderedOverlay {
                    lines: overlay_lines,
                    row: final_layout.row,
                    col: final_layout.col,
                    width: final_layout.width,
                });
            }
            rendered
        };

        composite_overlays(
            lines,
            &overlays,
            width,
            height,
            self.renderer.max_lines_rendered(),
            is_image_line,
        )
    }

    fn is_overlay_visible(&self, entry: &OverlayEntry) -> bool {
        if entry.hidden {
            return false;
        }
        match entry.options.as_ref().and_then(|opt| opt.visible.as_ref()) {
            Some(visible) => visible(self.terminal.columns() as usize, self.terminal.rows() as usize),
            None => true,
        }
    }

    fn reconcile_focus(&mut self) {
        let focused = self.focus.focused();
        if let Some(focused) = focused {
            if let Some(pre_focus) = self.pre_focus_for_overlay(&focused) {
                if !self.is_overlay_visible_for_component(&focused) {
                    let next = self.topmost_visible_overlay().or(pre_focus);
                    if let Some(next) = next {
                        self.focus.set_focus(Some(next));
                    } else if let Some(restore) = self.take_pending_pre_focus() {
                        self.focus.set_focus(Some(restore));
                    } else {
                        self.focus.clear();
                    }
                }
                return;
            }
        }

        if let Some(topmost) = self.topmost_visible_overlay() {
            self.focus.set_focus(Some(topmost));
        } else if let Some(restore) = self.take_pending_pre_focus() {
            self.focus.set_focus(Some(restore));
        }
    }

    fn topmost_visible_overlay(&self) -> Option<Rc<RefCell<Box<dyn Component>>>> {
        let state = self.overlays.borrow();
        for entry in state.entries.iter().rev() {
            if self.is_overlay_visible(entry) {
                return Some(Rc::clone(&entry.component));
            }
        }
        None
    }

    fn pre_focus_for_overlay(
        &self,
        component: &Rc<RefCell<Box<dyn Component>>>,
    ) -> Option<Option<Rc<RefCell<Box<dyn Component>>>>> {
        let state = self.overlays.borrow();
        state
            .entries
            .iter()
            .find(|entry| Rc::ptr_eq(&entry.component, component))
            .map(|entry| entry.pre_focus.clone())
    }

    fn is_overlay_visible_for_component(&self, component: &Rc<RefCell<Box<dyn Component>>>) -> bool {
        let state = self.overlays.borrow();
        if let Some(entry) = state
            .entries
            .iter()
            .find(|entry| Rc::ptr_eq(&entry.component, component))
        {
            return self.is_overlay_visible(entry);
        }
        false
    }

    fn take_overlay_dirty(&mut self) -> bool {
        let mut state = self.overlays.borrow_mut();
        let dirty = state.dirty;
        state.dirty = false;
        dirty
    }

    fn take_pending_pre_focus(&mut self) -> Option<Rc<RefCell<Box<dyn Component>>>> {
        let mut state = self.overlays.borrow_mut();
        state.pending_pre_focus.take()
    }
}

impl<T: Terminal> Drop for TuiRuntime<T> {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }

        // Best-effort cleanup: never panic in Drop (especially during unwind).
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.stop();
        }));
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
    use super::{find_cell_size_response, OverlayOptions, TuiRuntime};
    use crate::core::component::Component;
    use crate::core::terminal_image::{get_cell_dimensions, reset_capabilities_cache};
    use crate::core::terminal::Terminal;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;

    #[derive(Default)]
    struct TestTerminal {
        output: String,
        columns: u16,
        rows: u16,
    }

    impl TestTerminal {
        fn new(columns: u16, rows: u16) -> Self {
            Self {
                output: String::new(),
                columns,
                rows,
            }
        }
    }

    impl Terminal for TestTerminal {
        fn start(&mut self, _on_input: Box<dyn FnMut(String) + Send>, _on_resize: Box<dyn FnMut() + Send>) {}
        fn stop(&mut self) {}
        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
        fn write(&mut self, data: &str) {
            self.output.push_str(data);
        }
        fn columns(&self) -> u16 {
            if self.columns == 0 { 80 } else { self.columns }
        }
        fn rows(&self) -> u16 {
            if self.rows == 0 { 24 } else { self.rows }
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
        focused: Rc<RefCell<bool>>,
    }

    impl TestComponent {
        fn new(
            wants_release: bool,
            inputs: Rc<RefCell<Vec<String>>>,
            focused: Rc<RefCell<bool>>,
        ) -> Self {
            Self {
                inputs,
                wants_release,
                focused,
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

        fn as_focusable(&mut self) -> Option<&mut dyn crate::core::component::Focusable> {
            Some(self)
        }
    }

    impl crate::core::component::Focusable for TestComponent {
        fn set_focused(&mut self, focused: bool) {
            *self.focused.borrow_mut() = focused;
        }

        fn is_focused(&self) -> bool {
            *self.focused.borrow()
        }
    }

    #[test]
    fn key_release_filtered_unless_requested() {
        let terminal = TestTerminal::default();
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(DummyComponent::default())));
        let mut runtime = TuiRuntime::new(terminal, root);

        let inputs = Rc::new(RefCell::new(Vec::new()));
        let focused = Rc::new(RefCell::new(false));
        let component = TestComponent::new(false, Rc::clone(&inputs), focused);
        let component_handle: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(component)));
        runtime.set_focus(component_handle);
        runtime.handle_input("\x1b[32:3u");
        assert!(inputs.borrow().is_empty());

        let inputs_release = Rc::new(RefCell::new(Vec::new()));
        let focused_release = Rc::new(RefCell::new(false));
        let component_release =
            TestComponent::new(true, Rc::clone(&inputs_release), focused_release);
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

    #[test]
    fn overlay_focus_handoff_and_restore() {
        let terminal = TestTerminal::new(80, 24);
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::new(RefCell::new(Vec::new())), Rc::clone(&root_focus));
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(root_component)));
        let mut runtime = TuiRuntime::new(terminal, Rc::clone(&root));

        runtime.start();
        runtime.set_focus(Rc::clone(&root));
        assert!(*root_focus.borrow());

        let overlay_focus = Rc::new(RefCell::new(false));
        let overlay_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&overlay_focus),
        );
        let overlay: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(overlay_component)));
        let handle = runtime.show_overlay(Rc::clone(&overlay), None);
        runtime.run_once();
        assert!(*overlay_focus.borrow());

        handle.hide();
        runtime.run_once();
        assert!(*root_focus.borrow());
    }

    #[test]
    fn overlay_visibility_callback_on_resize() {
        let terminal = TestTerminal::new(5, 10);
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::new(RefCell::new(Vec::new())), Rc::clone(&root_focus));
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(root_component)));
        let mut runtime = TuiRuntime::new(terminal, Rc::clone(&root));
        runtime.start();
        runtime.set_focus(Rc::clone(&root));

        let overlay_focus = Rc::new(RefCell::new(false));
        let overlay_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&overlay_focus),
        );
        let overlay: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(overlay_component)));
        let mut options = OverlayOptions::default();
        options.visible = Some(Box::new(|w, _| w >= 10));

        runtime.show_overlay(Rc::clone(&overlay), Some(options));
        runtime.run_once();
        assert!(!*overlay_focus.borrow());

        runtime.terminal.columns = 20;
        runtime.pending_resize.store(true, Ordering::SeqCst);
        runtime.run_once();
        assert!(*overlay_focus.borrow());
    }

    #[test]
    fn render_handle_triggers_render_from_background_task() {
        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(component)));
        let mut runtime = TuiRuntime::new(terminal, root);

        runtime.start();
        runtime.render_if_needed();
        let baseline = state.borrow().renders;

        let handle = runtime.render_handle();
        let join = thread::spawn(move || {
            handle.request_render();
        });
        join.join().expect("join render thread");

        runtime.run_once();
        assert_eq!(state.borrow().renders, baseline + 1);
    }

    fn env_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[derive(Default, Debug)]
    struct TrackingState {
        show_cursor_calls: usize,
        drain_input_calls: usize,
        stop_calls: usize,
        drain_max_ms: Option<u64>,
        drain_idle_ms: Option<u64>,
    }

    #[derive(Clone)]
    struct TrackingTerminal {
        state: Arc<Mutex<TrackingState>>,
    }

    impl TrackingTerminal {
        fn new(state: Arc<Mutex<TrackingState>>) -> Self {
            Self { state }
        }

        fn with_state<F: FnOnce(&TrackingState)>(state: &Arc<Mutex<TrackingState>>, f: F) {
            let state = state.lock().expect("tracking state lock poisoned");
            f(&state);
        }
    }

    impl Terminal for TrackingTerminal {
        fn start(
            &mut self,
            _on_input: Box<dyn FnMut(String) + Send>,
            _on_resize: Box<dyn FnMut() + Send>,
        ) {
        }

        fn stop(&mut self) {
            let mut state = self.state.lock().expect("tracking state lock poisoned");
            state.stop_calls += 1;
        }

        fn drain_input(&mut self, max_ms: u64, idle_ms: u64) {
            let mut state = self.state.lock().expect("tracking state lock poisoned");
            state.drain_input_calls += 1;
            state.drain_max_ms = Some(max_ms);
            state.drain_idle_ms = Some(idle_ms);
        }

        fn write(&mut self, _data: &str) {}

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

        fn show_cursor(&mut self) {
            let mut state = self.state.lock().expect("tracking state lock poisoned");
            state.show_cursor_calls += 1;
        }

        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {}
        fn set_title(&mut self, _title: &str) {}
    }

    #[test]
    fn drop_stops_terminal_when_started() {
        let state = Arc::new(Mutex::new(TrackingState::default()));
        let terminal = TrackingTerminal::new(Arc::clone(&state));
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(DummyComponent::default())));

        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.start();
        drop(runtime);

        TrackingTerminal::with_state(&state, |state| {
            assert_eq!(state.show_cursor_calls, 1);
            assert_eq!(state.drain_input_calls, 1);
            assert_eq!(state.stop_calls, 1);
            assert_eq!(state.drain_max_ms, Some(super::STOP_DRAIN_MAX_MS));
            assert_eq!(state.drain_idle_ms, Some(super::STOP_DRAIN_IDLE_MS));
        });
    }

    #[test]
    fn stop_then_drop_does_not_double_teardown() {
        let state = Arc::new(Mutex::new(TrackingState::default()));
        let terminal = TrackingTerminal::new(Arc::clone(&state));
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(DummyComponent::default())));

        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.start();
        runtime.stop();
        drop(runtime);

        TrackingTerminal::with_state(&state, |state| {
            assert_eq!(state.show_cursor_calls, 1);
            assert_eq!(state.drain_input_calls, 1);
            assert_eq!(state.stop_calls, 1);
            assert_eq!(state.drain_max_ms, Some(super::STOP_DRAIN_MAX_MS));
            assert_eq!(state.drain_idle_ms, Some(super::STOP_DRAIN_IDLE_MS));
        });
    }

    #[test]
    fn drop_does_nothing_when_never_started() {
        let state = Arc::new(Mutex::new(TrackingState::default()));
        let terminal = TrackingTerminal::new(Arc::clone(&state));
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(DummyComponent::default())));

        drop(TuiRuntime::new(terminal, root));

        TrackingTerminal::with_state(&state, |state| {
            assert_eq!(state.show_cursor_calls, 0);
            assert_eq!(state.drain_input_calls, 0);
            assert_eq!(state.stop_calls, 0);
        });
    }
}
