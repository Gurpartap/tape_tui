//! TUI runtime (Phase 5).

use std::cell::RefCell;
use std::env;
use std::io;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::core::component::Component;
use crate::core::cursor::{CursorPos, CURSOR_MARKER};
use crate::core::input::{is_kitty_query_response, KeyEventType};
use crate::core::input_event::{parse_input_events, InputEvent};
use crate::core::output::{osc_title_sequence, OutputGate, TerminalCmd};
use crate::core::terminal::Terminal;
use crate::core::terminal_image::{
    get_capabilities, is_image_line, set_cell_dimensions, CellDimensions, TerminalImageState,
};
use crate::render::overlay::{
    composite_overlays, resolve_overlay_layout, OverlayOptions, RenderedOverlay,
};
use crate::render::renderer::DiffRenderer;
use crate::render::Frame;
use crate::runtime::focus::FocusState;
use crate::runtime::ime::position_hardware_cursor;

const STOP_DRAIN_MAX_MS: u64 = 1000;
const STOP_DRAIN_IDLE_MS: u64 = 50;
const COALESCE_MAX_DURATION_MS: u64 = 2;
const COALESCE_MAX_ITERATIONS: usize = 8;

type ComponentRc = Rc<RefCell<Box<dyn Component>>>;

#[derive(Clone, Copy, Debug)]
struct CoalesceBudget {
    max_duration: Duration,
    max_iterations: usize,
}

impl Default for CoalesceBudget {
    fn default() -> Self {
        Self {
            max_duration: Duration::from_millis(COALESCE_MAX_DURATION_MS),
            max_iterations: COALESCE_MAX_ITERATIONS,
        }
    }
}

impl CoalesceBudget {
    fn allows(&self, start: Instant, iterations: usize) -> bool {
        start.elapsed() < self.max_duration && iterations < self.max_iterations
    }
}

#[derive(Debug, Default)]
struct CrashCleanup {
    ran: AtomicBool,
}

impl CrashCleanup {
    fn run<T: Terminal>(&self, terminal: &mut T) {
        if self.ran.swap(true, Ordering::SeqCst) {
            return;
        }

        // Crash/signal cleanup is best-effort: we may not know which protocol toggles
        // actually succeeded before the failure. These control sequences are safe and
        // idempotent (and are ignored by terminals that don't implement them).
        let mut output = OutputGate::new();
        output.push(TerminalCmd::ShowCursor);
        output.push(TerminalCmd::BracketedPasteDisable);
        output.push(TerminalCmd::KittyDisable);
        output.flush(terminal);
    }

    #[cfg(all(unix, not(test)))]
    fn run_best_effort(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut terminal = crate::platform::process_terminal::HookTerminal::new();
            self.run(&mut terminal);
        }));
    }
}

pub struct TuiRuntime<T: Terminal> {
    terminal: T,
    output: OutputGate,
    terminal_image_state: Arc<TerminalImageState>,
    root: ComponentRc,
    renderer: DiffRenderer,
    focus: FocusState,
    overlays: Rc<RefCell<OverlayState>>,
    on_debug: Option<Box<dyn FnMut()>>,
    clear_on_shrink: bool,
    show_hardware_cursor: bool,
    stopped: bool,
    wake: Arc<RuntimeWake>,
    coalesce_budget: CoalesceBudget,
    input_buffer: String,
    cell_size_query_pending: bool,
    kitty_keyboard_enabled: bool,
    kitty_enable_pending: bool,
    #[cfg(all(unix, not(test)))]
    signal_hook_guard: Option<crate::platform::SignalHookGuard>,
    #[cfg(all(unix, not(test)))]
    panic_hook_guard: Option<crate::platform::PanicHookGuard>,
}

#[derive(Default)]
struct OverlayState {
    entries: Vec<OverlayEntry>,
    next_id: u64,
    dirty: bool,
    pending_pre_focus: Option<ComponentRc>,
}

struct OverlayEntry {
    id: u64,
    component: ComponentRc,
    options: Option<OverlayOptions>,
    pre_focus: Option<ComponentRc>,
    hidden: bool,
}

pub struct OverlayHandle {
    id: u64,
    state: std::rc::Weak<RefCell<OverlayState>>,
    wake: Arc<RuntimeWake>,
}

#[derive(Default)]
struct RuntimeWakeState {
    pending_inputs: Vec<String>,
    pending_resize: bool,
    render_requested: bool,
    pending_title: Option<String>,
    stop_requested: bool,
}

#[derive(Default)]
struct RuntimeWake {
    state: Mutex<RuntimeWakeState>,
    cvar: Condvar,
}

impl RuntimeWake {
    fn wait_for_event(&self) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };

        while !state.stop_requested
            && state.pending_inputs.is_empty()
            && !state.pending_resize
            && !state.render_requested
            && state.pending_title.is_none()
        {
            state = self
                .cvar
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }

        !state.stop_requested
    }

    fn enqueue_input(&self, data: String) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.pending_inputs.push(data);
        self.cvar.notify_one();
    }

    fn signal_resize(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.pending_resize = true;
        self.cvar.notify_one();
    }

    fn request_render(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.render_requested = true;
        self.cvar.notify_one();
    }

    fn set_title(&self, title: String) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.pending_title = Some(title);
        self.cvar.notify_one();
    }

    fn take_pending_resize(&self) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let pending = state.pending_resize;
        state.pending_resize = false;
        pending
    }

    fn take_pending_title(&self) -> Option<String> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.pending_title.take()
    }

    fn drain_inputs(&self) -> Vec<String> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut state.pending_inputs)
    }

    fn take_render_requested(&self) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let requested = state.render_requested;
        state.render_requested = false;
        requested
    }

    fn peek_render_requested(&self) -> bool {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.render_requested
    }

    fn clear_render_requested(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.render_requested = false;
    }

    fn has_pending_non_render(&self) -> bool {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.pending_resize || !state.pending_inputs.is_empty() || state.pending_title.is_some()
    }

    fn reset_for_start(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.stop_requested = false;
        state.pending_resize = false;
        state.pending_inputs.clear();
        state.render_requested = false;
        state.pending_title = None;
    }

    fn request_stop(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.stop_requested = true;
        self.cvar.notify_all();
    }

    #[cfg(test)]
    fn wait_for_event_with_before_wait<F: FnOnce()>(&self, before_wait: F) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };

        let mut before_wait = Some(before_wait);
        while !state.stop_requested
            && state.pending_inputs.is_empty()
            && !state.pending_resize
            && !state.render_requested
            && state.pending_title.is_none()
        {
            if let Some(before_wait) = before_wait.take() {
                before_wait();
            }
            state = self
                .cvar
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }

        !state.stop_requested
    }
}

#[derive(Clone)]
pub struct RenderHandle {
    wake: Arc<RuntimeWake>,
}

impl RenderHandle {
    pub fn request_render(&self) {
        self.wake.request_render();
    }

    pub fn set_title(&self, title: impl Into<String>) {
        self.wake.set_title(title.into());
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
                self.wake.request_render();
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
                    self.wake.request_render();
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
    pub fn new(terminal: T, root: ComponentRc) -> Self {
        let clear_on_shrink = env_flag("PI_CLEAR_ON_SHRINK");
        let show_hardware_cursor = env_flag("PI_HARDWARE_CURSOR");
        Self {
            terminal,
            output: OutputGate::new(),
            terminal_image_state: Arc::new(TerminalImageState::default()),
            root,
            renderer: DiffRenderer::new(),
            focus: FocusState::new(),
            overlays: Rc::new(RefCell::new(OverlayState::default())),
            on_debug: None,
            clear_on_shrink,
            show_hardware_cursor,
            stopped: true,
            wake: Arc::new(RuntimeWake::default()),
            coalesce_budget: CoalesceBudget::default(),
            input_buffer: String::new(),
            cell_size_query_pending: false,
            kitty_keyboard_enabled: false,
            kitty_enable_pending: false,
            #[cfg(all(unix, not(test)))]
            signal_hook_guard: None,
            #[cfg(all(unix, not(test)))]
            panic_hook_guard: None,
        }
    }

    pub fn set_on_debug(&mut self, handler: Option<Box<dyn FnMut()>>) {
        self.on_debug = handler;
    }

    #[cfg(test)]
    fn set_coalesce_budget_for_tests(&mut self, budget: CoalesceBudget) {
        self.coalesce_budget = budget;
    }

    pub fn render_handle(&self) -> RenderHandle {
        RenderHandle {
            wake: Arc::clone(&self.wake),
        }
    }

    /// Set the terminal window/tab title.
    ///
    /// While running, this queues a title update that flushes on the runtime thread
    /// without forcing a render. When stopped, it writes immediately.
    pub fn set_title(&mut self, title: impl Into<String>) {
        let title = title.into();
        if self.stopped {
            let mut output = OutputGate::new();
            output.push(TerminalCmd::Bytes(osc_title_sequence(&title)));
            output.flush(&mut self.terminal);
            return;
        }
        self.wake.set_title(title);
    }

    /// Enqueue a show-cursor command.
    ///
    /// This only enqueues terminal protocol bytes into the runtime output gate. The bytes are
    /// normally flushed at tick boundaries (e.g. `run_once` / `render_if_needed` / `render_now`).
    /// If you need the cursor visibility to change immediately without forcing a render, call
    /// [`TuiRuntime::flush_pending_output`].
    ///
    /// No-op when stopped to avoid perturbing the renderer's first-render baseline.
    pub fn show_cursor(&mut self) {
        if self.stopped {
            return;
        }
        self.output.push(TerminalCmd::ShowCursor);
    }

    /// Enqueue a hide-cursor command.
    ///
    /// This only enqueues terminal protocol bytes into the runtime output gate. The bytes are
    /// normally flushed at tick boundaries (e.g. `run_once` / `render_if_needed` / `render_now`).
    /// If you need the cursor visibility to change immediately without forcing a render, call
    /// [`TuiRuntime::flush_pending_output`].
    ///
    /// No-op when stopped to avoid perturbing the renderer's first-render baseline.
    pub fn hide_cursor(&mut self) {
        if self.stopped {
            return;
        }
        self.output.push(TerminalCmd::HideCursor);
    }

    /// Clear from the cursor to end-of-line, then force a full redraw next render.
    ///
    /// No-op when stopped to avoid perturbing the renderer's first-render baseline.
    pub fn clear_line(&mut self) {
        if self.stopped {
            return;
        }
        self.output.push(TerminalCmd::ClearLine);
        self.renderer.request_full_redraw_next();
        self.request_render();
    }

    /// Clear from cursor to end of screen, then force a full redraw next render.
    ///
    /// No-op when stopped to avoid perturbing the renderer's first-render baseline.
    pub fn clear_from_cursor(&mut self) {
        if self.stopped {
            return;
        }
        self.output.push(TerminalCmd::ClearFromCursor);
        self.renderer.request_full_redraw_next();
        self.request_render();
    }

    /// Clear the screen and reset the renderer as if the terminal was externally cleared.
    ///
    /// No-op when stopped to avoid perturbing the renderer's first-render baseline.
    pub fn clear_screen(&mut self) {
        if self.stopped {
            return;
        }
        self.output.push(TerminalCmd::ClearScreen);
        self.renderer.reset_for_external_clear_screen();
        self.request_render();
    }

    /// Move the cursor up/down by a number of lines, without requesting a render.
    ///
    /// This updates the renderer's internal cursor model so the next render can place
    /// the cursor deterministically without desync.
    ///
    /// This only enqueues terminal protocol bytes into the runtime output gate. The bytes are
    /// normally flushed at tick boundaries (e.g. `run_once` / `render_if_needed` / `render_now`).
    /// If you need the cursor move to take effect immediately without forcing a render, call
    /// [`TuiRuntime::flush_pending_output`].
    ///
    /// No-op when stopped to avoid perturbing the renderer's first-render baseline.
    pub fn move_by(&mut self, lines: i32) {
        if self.stopped {
            return;
        }
        if lines == 0 {
            return;
        }
        if lines > 0 {
            self.output.push(TerminalCmd::MoveDown(lines as usize));
        } else {
            self.output.push(TerminalCmd::MoveUp((-lines) as usize));
        }
        self.renderer
            .apply_out_of_band_move_by(lines, self.terminal.rows() as usize);
    }

    pub fn terminal_rows(&self) -> u16 {
        self.terminal.rows()
    }

    pub fn terminal_columns(&self) -> u16 {
        self.terminal.columns()
    }

    pub fn kitty_protocol_active(&self) -> bool {
        self.kitty_keyboard_enabled || self.kitty_enable_pending
    }

    /// Force the next render to redraw the entire viewport (without clearing scrollback).
    ///
    /// No-op when stopped to avoid perturbing the renderer's first-render baseline.
    pub fn request_full_redraw(&mut self) {
        if self.stopped {
            return;
        }
        self.renderer.request_full_redraw_next();
        self.request_render();
    }

    /// Enable/disable showing the terminal's hardware cursor.
    ///
    /// When disabling, we enqueue an explicit hide-cursor command immediately to keep
    /// terminal state consistent even if no further render happens soon.
    ///
    /// No-op when stopped to avoid perturbing the renderer's first-render baseline.
    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        if self.stopped {
            return;
        }
        if self.show_hardware_cursor == enabled {
            return;
        }
        self.show_hardware_cursor = enabled;
        if !enabled {
            self.output.push(TerminalCmd::HideCursor);
        }
        self.request_render();
    }

    /// Enable/disable clearing behavior when the terminal shrinks.
    ///
    /// No-op when stopped to avoid perturbing the renderer's first-render baseline.
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        if self.stopped {
            return;
        }
        self.clear_on_shrink = enabled;
    }

    pub fn terminal_image_state(&self) -> Arc<TerminalImageState> {
        Arc::clone(&self.terminal_image_state)
    }

    pub fn set_focus(&mut self, target: Rc<RefCell<Box<dyn Component>>>) {
        self.focus.set_focus(Some(target));
    }

    pub fn clear_focus(&mut self) {
        self.focus.clear();
    }

    pub fn show_overlay(
        &mut self,
        component: ComponentRc,
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
            wake: Arc::clone(&self.wake),
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
        state
            .entries
            .iter()
            .any(|entry| self.is_overlay_visible(entry))
    }

    pub fn start(&mut self) -> io::Result<()> {
        self.output.clear();
        self.kitty_keyboard_enabled = false;
        self.kitty_enable_pending = false;
        self.wake.reset_for_start();

        // Mark running early so Drop can attempt cleanup if `Terminal::start()` panics.
        self.stopped = false;

        #[cfg(all(unix, not(test)))]
        self.install_cleanup_hooks();

        let wake_input = Arc::clone(&self.wake);
        let wake_resize = Arc::clone(&self.wake);
        if let Err(err) = self.terminal.start(
            Box::new(move |data| {
                wake_input.enqueue_input(data);
            }),
            Box::new(move || {
                wake_resize.signal_resize();
            }),
        ) {
            self.stopped = true;
            #[cfg(all(unix, not(test)))]
            self.uninstall_cleanup_hooks();
            return Err(err);
        }

        self.output.push(TerminalCmd::BracketedPasteEnable);
        self.output.push(TerminalCmd::KittyQuery);
        self.output.push(TerminalCmd::HideCursor);
        self.query_cell_size();
        self.flush_output();
        self.request_render();

        Ok(())
    }

    pub fn stop(&mut self) -> io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.wake.request_stop();
        self.place_cursor_at_end();
        self.output.push(TerminalCmd::ShowCursor);
        self.output.push(TerminalCmd::BracketedPasteDisable);
        if self.kitty_keyboard_enabled || self.kitty_enable_pending {
            self.output.push(TerminalCmd::KittyDisable);
        }
        self.flush_output();
        self.kitty_keyboard_enabled = false;
        self.kitty_enable_pending = false;
        self.terminal
            .drain_input(STOP_DRAIN_MAX_MS, STOP_DRAIN_IDLE_MS);
        let result = self.terminal.stop();
        self.stopped = true;
        #[cfg(all(unix, not(test)))]
        self.uninstall_cleanup_hooks();
        result
    }

    #[cfg(all(unix, not(test)))]
    fn install_cleanup_hooks(&mut self) {
        let cleanup = Arc::new(CrashCleanup::default());
        let signal_cleanup = Arc::clone(&cleanup);
        let panic_cleanup = Arc::clone(&cleanup);
        self.signal_hook_guard = Some(crate::platform::install_signal_handlers(move || {
            signal_cleanup.run_best_effort()
        }));
        self.panic_hook_guard = Some(crate::platform::install_panic_hook(move || {
            panic_cleanup.run_best_effort()
        }));
    }

    #[cfg(all(unix, not(test)))]
    fn uninstall_cleanup_hooks(&mut self) {
        self.signal_hook_guard = None;
        self.panic_hook_guard = None;
    }

    /// Block until at least one input/resize/render event is available, then
    /// coalesce work and render once (bounded).
    ///
    /// Note: this does **not** run an event loop until stopped; callers typically
    /// call this in a loop.
    pub fn run_blocking_once(&mut self) {
        if self.stopped {
            return;
        }

        if !self.wake.wait_for_event() {
            return;
        }

        self.run_coalesced_once();
    }

    /// Alias for [`TuiRuntime::run_blocking_once`]. Kept for compatibility.
    pub fn run(&mut self) {
        self.run_blocking_once();
    }

    #[cfg(test)]
    fn run_with_before_wait<F: FnOnce()>(&mut self, before_wait: F) {
        if self.stopped {
            return;
        }

        if !self.wake.wait_for_event_with_before_wait(before_wait) {
            return;
        }

        self.run_coalesced_once();
    }

    fn run_coalesced_once(&mut self) {
        // Coalescing contract:
        // - We drain all work already queued (and any work that arrives during the
        //   non-blocking coalescing window).
        // - If the coalescing budget expires while work remains queued, we render using
        //   the work drained so far and intentionally defer the remaining work to the next tick.
        // This is a deliberate behavior change to batch renders while bounding latency.
        let start = Instant::now();
        let mut iterations = 0;
        let mut yielded = false;

        loop {
            let mut did_work = false;

            if self.take_overlay_dirty() {
                self.reconcile_focus();
                self.request_render();
                did_work = true;
            }

            if self.wake.take_pending_resize() {
                self.reconcile_focus();
                if let Some(component) = self.focus.focused() {
                    let event = InputEvent::Resize {
                        columns: self.terminal.columns(),
                        rows: self.terminal.rows(),
                    };
                    component.borrow_mut().handle_event(&event);
                }
                self.request_render();
                did_work = true;
            }

            let inputs = self.wake.drain_inputs();
            if !inputs.is_empty() {
                for data in inputs {
                    self.handle_input(&data);
                }
                did_work = true;
            }

            if let Some(title) = self.wake.take_pending_title() {
                self.output
                    .push(TerminalCmd::Bytes(osc_title_sequence(&title)));
                did_work = true;
            }

            if !did_work {
                if self.wake.peek_render_requested() {
                    self.wake.clear_render_requested();
                    self.do_render();
                }
                break;
            }

            if !self.coalesce_budget.allows(start, iterations) {
                if self.wake.peek_render_requested() {
                    self.wake.clear_render_requested();
                    self.do_render();
                }
                break;
            }

            iterations += 1;

            if !yielded && !self.has_pending_non_render() && self.wake.peek_render_requested() {
                std::thread::yield_now();
                yielded = true;
            }
        }

        self.flush_output();
    }

    pub fn run_once(&mut self) {
        if self.stopped {
            return;
        }

        if self.take_overlay_dirty() {
            self.reconcile_focus();
            self.request_render();
        }

        if self.wake.take_pending_resize() {
            self.reconcile_focus();
            if let Some(component) = self.focus.focused() {
                let event = InputEvent::Resize {
                    columns: self.terminal.columns(),
                    rows: self.terminal.rows(),
                };
                component.borrow_mut().handle_event(&event);
            }
            self.request_render();
        }

        let inputs = self.wake.drain_inputs();

        for data in inputs {
            self.handle_input(&data);
        }

        if let Some(title) = self.wake.take_pending_title() {
            self.output
                .push(TerminalCmd::Bytes(osc_title_sequence(&title)));
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

        if is_kitty_query_response(data) {
            if !self.kitty_keyboard_enabled && !self.kitty_enable_pending {
                self.output.push(TerminalCmd::KittyEnable);
                self.kitty_enable_pending = true;
            }
            return;
        }

        let events = parse_input_events(data, self.kitty_keyboard_enabled);
        if events.is_empty() {
            return;
        }

        let Some(component) = self.focus.focused() else {
            return;
        };

        let mut component = component.borrow_mut();

        let mut handled = false;
        for event in events {
            if let InputEvent::Key {
                key_id, event_type, ..
            } = &event
            {
                if *event_type == KeyEventType::Press && key_id == "ctrl+shift+d" {
                    if let Some(handler) = self.on_debug.as_mut() {
                        handler();
                    }
                    continue;
                }
                if *event_type == KeyEventType::Release && !component.wants_key_release() {
                    continue;
                }
            }

            component.handle_event(&event);
            handled = true;
        }

        if handled {
            self.request_render();
        }
    }

    pub fn request_render(&mut self) {
        self.wake.request_render();
    }

    pub fn render_if_needed(&mut self) {
        if self.wake.take_render_requested() {
            self.do_render();
        }
        self.flush_output();
    }

    pub fn render_now(&mut self) {
        self.wake.clear_render_requested();
        self.do_render();
        self.flush_output();
    }

    /// Flush queued terminal protocol bytes without rendering.
    ///
    /// Many helpers (such as [`TuiRuntime::hide_cursor`], [`TuiRuntime::show_cursor`], and
    /// [`TuiRuntime::move_by`]) enqueue bytes into the runtime output gate but do not flush
    /// immediately. The runtime typically flushes at tick boundaries (e.g. `run_once` /
    /// `render_if_needed` / `render_now`) to preserve a single, deterministic write gate.
    ///
    /// Call this when you need immediate terminal effects without forcing a render.
    pub fn flush_pending_output(&mut self) {
        if self.stopped {
            return;
        }
        self.flush_output();
    }

    fn do_render(&mut self) {
        let width = self.terminal.columns() as usize;
        let height = self.terminal.rows() as usize;
        self.reconcile_focus();
        let (mut lines, mut cursor_pos) = {
            let mut root = self.root.borrow_mut();
            root.set_terminal_rows(height);
            let lines = root.render(width);
            let cursor_pos = root.cursor_pos();
            (lines, cursor_pos)
        };

        if self.has_overlay() {
            let (composited, overlay_cursor) = self.composite_overlay_lines(lines, width, height);
            lines = composited;
            if overlay_cursor.is_some() {
                cursor_pos = overlay_cursor;
            }
        }

        if let Some(pos) = cursor_pos {
            let viewport_top = lines.len().saturating_sub(height);
            if pos.row < viewport_top || pos.row >= lines.len() {
                cursor_pos = None;
            }
        }

        // Components may emit the legacy CURSOR_MARKER APC sequence. Ensure it never
        // reaches the renderer/terminal output. If a component didn't provide typed
        // cursor metadata, use the extracted marker position as a fallback.
        let extracted_marker_pos = crate::core::cursor::extract_cursor_marker(&mut lines, height);
        for line in lines.iter_mut() {
            while let Some(idx) = line.find(CURSOR_MARKER) {
                let end = idx + CURSOR_MARKER.len();
                line.replace_range(idx..end, "");
            }
        }
        if cursor_pos.is_none() {
            cursor_pos = extracted_marker_pos;
        }

        // Clamp cursor column to the terminal width to avoid emitting huge `CSI n G` moves.
        if let Some(mut pos) = cursor_pos {
            pos.col = pos.col.min(width.saturating_sub(1));
            cursor_pos = Some(pos);
        }

        let has_overlays = self.has_overlay();
        let frame = Frame::from(lines).with_cursor(cursor_pos);
        let cursor_pos = frame.cursor();
        let total_lines = frame.lines().len();
        let render_cmds =
            self.renderer
                .render(frame, width, height, self.clear_on_shrink, has_overlays);
        self.output.extend(render_cmds);

        let (updated_row, cursor_cmds) = position_hardware_cursor(
            cursor_pos,
            total_lines,
            self.renderer.hardware_cursor_row(),
            self.show_hardware_cursor,
        );
        self.output.extend(cursor_cmds);
        self.renderer.set_hardware_cursor_row(updated_row);
    }

    fn flush_output(&mut self) {
        if self.output.is_empty() {
            return;
        }
        self.output.flush(&mut self.terminal);
        if self.kitty_enable_pending {
            self.kitty_keyboard_enabled = true;
            self.kitty_enable_pending = false;
        }
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
            self.output.push(TerminalCmd::MoveDown(diff as usize));
        } else if diff < 0 {
            self.output.push(TerminalCmd::MoveUp((-diff) as usize));
        }
        self.output.push(TerminalCmd::BytesStatic("\r\n"));
        self.renderer.set_hardware_cursor_row(target_row);
    }

    fn query_cell_size(&mut self) {
        if get_capabilities(self.terminal_image_state.as_ref())
            .images
            .is_none()
        {
            return;
        }
        self.cell_size_query_pending = true;
        self.output.push(TerminalCmd::QueryCellSize);
    }

    fn filter_cell_size_response(&mut self, data: &str) -> Option<String> {
        self.input_buffer.push_str(data);

        if let Some((start, end, height_px, width_px)) = find_cell_size_response(&self.input_buffer)
        {
            if height_px > 0 && width_px > 0 {
                set_cell_dimensions(
                    self.terminal_image_state.as_ref(),
                    CellDimensions {
                        width_px,
                        height_px,
                    },
                );
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

    fn composite_overlay_lines(
        &mut self,
        lines: Vec<String>,
        width: usize,
        height: usize,
    ) -> (Vec<String>, Option<CursorPos>) {
        let (overlays, overlay_cursor) = {
            let state = self.overlays.borrow();
            let mut rendered: Vec<(RenderedOverlay, Option<CursorPos>)> = Vec::new();
            for entry in state.entries.iter() {
                if !self.is_overlay_visible(entry) {
                    continue;
                }
                let layout = resolve_overlay_layout(entry.options.as_ref(), 0, width, height);
                let mut component = entry.component.borrow_mut();
                component.set_terminal_rows(height);
                let mut overlay_lines = component.render(layout.width);
                let mut cursor_pos = component.cursor_pos();
                if let Some(max_height) = layout.max_height {
                    if overlay_lines.len() > max_height {
                        overlay_lines.truncate(max_height);
                    }
                }
                if let Some(pos) = cursor_pos {
                    if pos.row >= overlay_lines.len() {
                        cursor_pos = None;
                    }
                }
                let final_layout = resolve_overlay_layout(
                    entry.options.as_ref(),
                    overlay_lines.len(),
                    width,
                    height,
                );
                rendered.push((
                    RenderedOverlay {
                        lines: overlay_lines,
                        row: final_layout.row,
                        col: final_layout.col,
                        width: final_layout.width,
                    },
                    cursor_pos,
                ));
            }

            let mut min_lines_needed = lines.len();
            for (overlay, _) in rendered.iter() {
                min_lines_needed = min_lines_needed.max(overlay.row + overlay.lines.len());
            }
            let working_height = self.renderer.max_lines_rendered().max(min_lines_needed);
            let viewport_start = working_height.saturating_sub(height);

            let mut overlay_cursor: Option<CursorPos> = None;
            for (overlay, cursor_pos) in rendered.iter() {
                let Some(cursor_pos) = cursor_pos else {
                    continue;
                };
                if cursor_pos.row >= overlay.lines.len() || cursor_pos.col >= overlay.width {
                    continue;
                }
                // Never place the hardware cursor onto an image line.
                if is_image_line(&overlay.lines[cursor_pos.row]) {
                    continue;
                }
                let abs_row = viewport_start
                    .saturating_add(overlay.row)
                    .saturating_add(cursor_pos.row);
                if abs_row < lines.len() && is_image_line(&lines[abs_row]) {
                    continue;
                }
                overlay_cursor = Some(CursorPos {
                    row: abs_row,
                    col: overlay.col.saturating_add(cursor_pos.col),
                });
            }

            let overlays = rendered
                .into_iter()
                .map(|(overlay, _)| overlay)
                .collect::<Vec<_>>();
            (overlays, overlay_cursor)
        };

        let composited = composite_overlays(
            lines,
            &overlays,
            width,
            height,
            self.renderer.max_lines_rendered(),
            is_image_line,
        );

        (composited, overlay_cursor)
    }

    fn is_overlay_visible(&self, entry: &OverlayEntry) -> bool {
        if entry.hidden {
            return false;
        }
        match entry.options.as_ref().and_then(|opt| opt.visible.as_ref()) {
            Some(visible) => visible(
                self.terminal.columns() as usize,
                self.terminal.rows() as usize,
            ),
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

    fn pre_focus_for_overlay(&self, component: &ComponentRc) -> Option<Option<ComponentRc>> {
        let state = self.overlays.borrow();
        state
            .entries
            .iter()
            .find(|entry| Rc::ptr_eq(&entry.component, component))
            .map(|entry| entry.pre_focus.clone())
    }

    fn is_overlay_visible_for_component(&self, component: &ComponentRc) -> bool {
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

    fn peek_overlay_dirty(&self) -> bool {
        let state = self.overlays.borrow();
        state.dirty
    }

    fn has_pending_non_render(&self) -> bool {
        self.peek_overlay_dirty() || self.wake.has_pending_non_render()
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
            let _ = self.stop();
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
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' && bytes[i + 2] == b'6' && bytes[i + 3] == b';'
        {
            let mut j = i + 4;
            let mut height: u32 = 0;
            let mut has_height = false;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                height = height
                    .saturating_mul(10)
                    .saturating_add((bytes[j] - b'0') as u32);
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
                width = width
                    .saturating_mul(10)
                    .saturating_add((bytes[j] - b'0') as u32);
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
    use super::{
        find_cell_size_response, CoalesceBudget, CrashCleanup, OverlayOptions, RenderHandle,
        TuiRuntime,
    };
    use crate::core::component::Component;
    use crate::core::cursor::CursorPos;
    use crate::core::output::TerminalCmd;
    use crate::core::terminal::Terminal;
    use crate::core::terminal_image::get_cell_dimensions;
    use crate::render::overlay::SizeValue;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;
    use std::time::Duration;

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
        fn start(
            &mut self,
            _on_input: Box<dyn FnMut(String) + Send>,
            _on_resize: Box<dyn FnMut() + Send>,
        ) -> std::io::Result<()> {
            Ok(())
        }
        fn stop(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
        fn write(&mut self, data: &str) {
            self.output.push_str(data);
        }
        fn columns(&self) -> u16 {
            if self.columns == 0 {
                80
            } else {
                self.columns
            }
        }
        fn rows(&self) -> u16 {
            if self.rows == 0 {
                24
            } else {
                self.rows
            }
        }
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

        fn handle_event(&mut self, event: &crate::core::input_event::InputEvent) {
            let raw = match event {
                crate::core::input_event::InputEvent::Key { raw, .. } => raw.as_str(),
                crate::core::input_event::InputEvent::Text { raw, .. } => raw.as_str(),
                crate::core::input_event::InputEvent::Paste { raw, .. } => raw.as_str(),
                crate::core::input_event::InputEvent::UnknownRaw { raw } => raw.as_str(),
                crate::core::input_event::InputEvent::Resize { .. } => return,
            };
            self.inputs.borrow_mut().push(raw.to_string());
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
    fn crash_cleanup_writes_expected_bytes_and_is_idempotent() {
        let cleanup = CrashCleanup::default();
        let mut terminal = TestTerminal::default();

        cleanup.run(&mut terminal);
        cleanup.run(&mut terminal);

        assert_eq!(terminal.output, "\x1b[?25h\x1b[?2004l\x1b[<u");
    }

    #[test]
    fn key_release_filtered_unless_requested() {
        let terminal = TestTerminal::default();
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(DummyComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);

        let inputs = Rc::new(RefCell::new(Vec::new()));
        let focused = Rc::new(RefCell::new(false));
        let component = TestComponent::new(false, Rc::clone(&inputs), focused);
        let component_handle: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(component)));
        runtime.set_focus(component_handle);
        runtime.handle_input("\x1b[32;1:3u");
        assert!(inputs.borrow().is_empty());

        let inputs_release = Rc::new(RefCell::new(Vec::new()));
        let focused_release = Rc::new(RefCell::new(false));
        let component_release =
            TestComponent::new(true, Rc::clone(&inputs_release), focused_release);
        let component_release_handle: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(component_release)));
        runtime.set_focus(component_release_handle);
        runtime.handle_input("\x1b[32;1:3u");
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
        std::env::set_var("TERM_PROGRAM", "kitty");

        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(component)));
        let mut runtime = TuiRuntime::new(terminal, root);

        runtime.start().expect("runtime start");
        assert!(runtime.terminal.output.contains("\x1b[16t"));
        runtime.render_if_needed();
        assert_eq!(state.borrow().renders, 1);

        runtime.handle_input("\x1b[6;20;10t");
        runtime.render_if_needed();
        assert_eq!(state.borrow().invalidates, 1);
        assert_eq!(state.borrow().renders, 2);

        let dims = get_cell_dimensions(runtime.terminal_image_state.as_ref());
        assert_eq!(dims.width_px, 10);
        assert_eq!(dims.height_px, 20);

        std::env::remove_var("TERM_PROGRAM");
    }

    #[test]
    fn output_order_is_protocol_then_frame_then_cursor() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        #[derive(Default)]
        struct CursorPosComponent;

        impl Component for CursorPosComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["hello".to_string()]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 5 })
            }
        }

        let terminal = TestTerminal::new(80, 24);
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(CursorPosComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        runtime.handle_input("\x1b[?1u");
        runtime.render_now();

        let output = runtime.terminal.output.as_str();
        let expected = "\x1b[>7u\x1b[?2026hhello\x1b[0m\x1b]8;;\x07\x1b[?2026l\x1b[6G\x1b[?25l";
        assert_eq!(output, expected);
    }

    #[test]
    fn cursor_col_is_clamped_to_terminal_width() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        #[derive(Default)]
        struct WideCursorComponent;

        impl Component for WideCursorComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["hello".to_string()]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 999 })
            }
        }

        let terminal = TestTerminal::new(10, 24);
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(WideCursorComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        runtime.handle_input("\x1b[?1u");
        runtime.render_now();

        let output = runtime.terminal.output.as_str();
        assert!(
            output.ends_with("\x1b[10G\x1b[?25l"),
            "unexpected output suffix: {output:?}"
        );
        assert!(
            !output.contains("\x1b[1000G"),
            "expected cursor col clamp to avoid huge column move: {output:?}"
        );
    }

    #[test]
    fn cursor_marker_is_stripped_from_output_and_used_as_fallback_cursor_pos() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        #[derive(Default)]
        struct CursorMarkerComponent;

        impl Component for CursorMarkerComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec![format!(
                    "hello{}{}",
                    crate::core::cursor::CURSOR_MARKER,
                    crate::core::cursor::CURSOR_MARKER
                )]
            }
        }

        let terminal = TestTerminal::new(80, 24);
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(CursorMarkerComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        runtime.handle_input("\x1b[?1u");
        runtime.render_now();

        let output = runtime.terminal.output.as_str();
        assert!(
            !output.contains(crate::core::cursor::CURSOR_MARKER),
            "cursor marker leaked into output: {output:?}"
        );
        assert!(
            output.contains("hello"),
            "expected hello in output: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[6G\x1b[?25l"),
            "unexpected output suffix: {output:?}"
        );
    }

    #[test]
    fn cursor_marker_stripping_removes_all_occurrences_across_multiple_lines() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        #[derive(Default)]
        struct MultiLineCursorMarkerComponent;

        impl Component for MultiLineCursorMarkerComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec![
                    format!("top{}X", crate::core::cursor::CURSOR_MARKER),
                    format!(
                        "bottom{}Y{}Z",
                        crate::core::cursor::CURSOR_MARKER,
                        crate::core::cursor::CURSOR_MARKER
                    ),
                ]
            }
        }

        let terminal = TestTerminal::new(80, 24);
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(MultiLineCursorMarkerComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        runtime.handle_input("\x1b[?1u");
        runtime.render_now();

        let output = runtime.terminal.output.as_str();
        assert!(
            !output.contains(crate::core::cursor::CURSOR_MARKER),
            "cursor marker leaked into output: {output:?}"
        );
        assert!(
            output.contains("topX"),
            "expected top line content in output: {output:?}"
        );
        assert!(
            output.contains("bottomYZ"),
            "expected bottom line content in output: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[7G\x1b[?25l"),
            "unexpected output suffix: {output:?}"
        );
    }

    #[test]
    fn cursor_marker_is_stripped_but_cursor_metadata_wins() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        #[derive(Default)]
        struct CursorMarkerWithMetadataComponent;

        impl Component for CursorMarkerWithMetadataComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec![format!(
                    "hello{}{}",
                    crate::core::cursor::CURSOR_MARKER,
                    crate::core::cursor::CURSOR_MARKER
                )]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 0 })
            }
        }

        let terminal = TestTerminal::new(80, 24);
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(CursorMarkerWithMetadataComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        runtime.handle_input("\x1b[?1u");
        runtime.render_now();

        let output = runtime.terminal.output.as_str();
        assert!(
            !output.contains(crate::core::cursor::CURSOR_MARKER),
            "cursor marker leaked into output: {output:?}"
        );
        assert!(
            output.contains("hello"),
            "expected hello in output: {output:?}"
        );
        assert!(
            output.ends_with("\x1b[1G\x1b[?25l"),
            "unexpected output suffix: {output:?}"
        );
    }

    #[test]
    fn overlay_cursor_is_ignored_when_overlay_line_is_image() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        struct BaseCursorComponent;

        impl Component for BaseCursorComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["one".to_string(), "two".to_string(), "three".to_string()]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 0 })
            }
        }

        struct OverlayImageCursorComponent;

        impl Component for OverlayImageCursorComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["\x1b_Gf=100;data".to_string()]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 4 })
            }
        }

        let terminal = TestTerminal::new(20, 3);
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(BaseCursorComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        let overlay: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(OverlayImageCursorComponent)));
        let options = OverlayOptions {
            width: Some(SizeValue::absolute(10)),
            row: Some(SizeValue::absolute(1)),
            col: Some(SizeValue::absolute(2)),
            ..Default::default()
        };
        runtime.show_overlay(overlay, Some(options));

        runtime.render_now();

        let output = runtime.terminal.output.as_str();
        assert!(
            output.ends_with("\x1b[2A\x1b[1G\x1b[?25l"),
            "unexpected output suffix: {output:?}"
        );
    }

    #[test]
    fn overlay_cursor_metadata_wins_over_base_cursor() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        struct BaseCursorComponent;

        impl Component for BaseCursorComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["one".to_string(), "two".to_string(), "three".to_string()]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 0 })
            }
        }

        struct OverlayCursorComponent;

        impl Component for OverlayCursorComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["overlay".to_string()]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 4 })
            }
        }

        let terminal = TestTerminal::new(20, 3);
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(BaseCursorComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        let overlay: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(OverlayCursorComponent)));
        let options = OverlayOptions {
            width: Some(SizeValue::absolute(10)),
            row: Some(SizeValue::absolute(1)),
            col: Some(SizeValue::absolute(2)),
            ..Default::default()
        };
        runtime.show_overlay(overlay, Some(options));

        runtime.render_now();

        let output = runtime.terminal.output.as_str();
        assert!(
            output.ends_with("\x1b[1A\x1b[7G\x1b[?25l"),
            "unexpected output suffix: {output:?}"
        );
    }

    #[test]
    fn request_full_redraw_rewrites_viewport_without_scrollback_clear() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        struct TwoLineComponent;

        impl Component for TwoLineComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["line-a".to_string(), "line-b".to_string()]
            }
        }

        let terminal = TestTerminal::new(20, 2);
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(TwoLineComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        // First render establishes the renderer baseline.
        runtime.render_now();
        runtime.terminal.output.clear();

        // Rendering identical content should be a no-op diff (no line content rewritten).
        runtime.render_now();
        let output2 = runtime.terminal.output.clone();
        assert!(
            !output2.contains("line-a") && !output2.contains("line-b"),
            "expected no line rewrites on stable diff, got: {output2:?}"
        );
        runtime.terminal.output.clear();

        runtime.request_full_redraw();
        runtime.render_now();

        let output = runtime.terminal.output.as_str();
        assert!(output.contains("line-a"), "expected line-a in output: {output:?}");
        assert!(output.contains("line-b"), "expected line-b in output: {output:?}");
        assert_eq!(
            output.matches("\x1b[2K").count(),
            2,
            "expected exactly 2 full-line clears, got: {output:?}"
        );
        assert!(
            !output.contains("\x1b[3J"),
            "expected no scrollback clear (ESC[3J), got: {output:?}"
        );
        assert!(
            !output.contains("\x1b[2J\x1b[H"),
            "expected no full screen clear (ESC[2J ESC[H), got: {output:?}"
        );
    }

    #[test]
    fn move_by_updates_cursor_model_for_next_cursor_placement() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        struct CursorComponent;

        impl Component for CursorComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["hello".to_string()]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 0 })
            }
        }

        let terminal = TestTerminal::new(20, 2);
        let root: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(CursorComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        runtime.render_now();
        runtime.terminal.output.clear();

        runtime.move_by(1);
        runtime.render_now();

        let output = runtime.terminal.output.as_str();
        assert!(
            output.contains("\x1b[1A"),
            "expected render to move cursor up after out-of-band move down, got: {output:?}"
        );
    }

    #[test]
    fn kitty_protocol_active_true_when_enable_pending_or_enabled() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        let terminal = TestTerminal::default();
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(DummyComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);

        runtime.start().expect("runtime start");
        assert!(!runtime.kitty_protocol_active());

        runtime.handle_input("\x1b[?1u");
        assert!(runtime.kitty_protocol_active(), "expected kitty pending after query response");

        runtime.run_once(); // flush pending enable
        assert!(
            runtime.kitty_protocol_active(),
            "expected kitty enabled after flush"
        );
    }

    #[test]
    fn cell_dimensions_are_runtime_scoped() {
        let terminal_a = TestTerminal::default();
        let terminal_b = TestTerminal::default();

        let root_a: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(DummyComponent)));
        let root_b: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(DummyComponent)));

        let mut runtime_a = TuiRuntime::new(terminal_a, root_a);
        let mut runtime_b = TuiRuntime::new(terminal_b, root_b);

        runtime_a.cell_size_query_pending = true;
        runtime_b.cell_size_query_pending = true;

        runtime_a.handle_input("\x1b[6;20;10t");
        let dims_a = get_cell_dimensions(runtime_a.terminal_image_state.as_ref());
        assert_eq!(dims_a.width_px, 10);
        assert_eq!(dims_a.height_px, 20);

        runtime_b.handle_input("\x1b[6;40;30t");
        let dims_a_again = get_cell_dimensions(runtime_a.terminal_image_state.as_ref());
        let dims_b = get_cell_dimensions(runtime_b.terminal_image_state.as_ref());
        assert_eq!(dims_a_again, dims_a);
        assert_eq!(dims_b.width_px, 30);
        assert_eq!(dims_b.height_px, 40);
    }

    #[test]
    fn overlay_focus_handoff_and_restore() {
        let terminal = TestTerminal::new(80, 24);
        let root_focus = Rc::new(RefCell::new(false));
        let root_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&root_focus),
        );
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(root_component)));
        let mut runtime = TuiRuntime::new(terminal, Rc::clone(&root));

        runtime.start().expect("runtime start");
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
        let root_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&root_focus),
        );
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(root_component)));
        let mut runtime = TuiRuntime::new(terminal, Rc::clone(&root));
        runtime.start().expect("runtime start");
        runtime.set_focus(Rc::clone(&root));

        let overlay_focus = Rc::new(RefCell::new(false));
        let overlay_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&overlay_focus),
        );
        let overlay: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(overlay_component)));
        let options = OverlayOptions {
            visible: Some(Box::new(|w, _| w >= 10)),
            ..Default::default()
        };

        runtime.show_overlay(Rc::clone(&overlay), Some(options));
        runtime.run_once();
        assert!(!*overlay_focus.borrow());

        runtime.terminal.columns = 20;
        runtime.wake.signal_resize();
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

        runtime.start().expect("runtime start");
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

    #[test]
    fn render_handle_wakes_blocking_run() {
        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(component)));
        let mut runtime = TuiRuntime::new(terminal, root);

        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        let baseline = state.borrow().renders;

        let handle = runtime.render_handle();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let join = thread::spawn(move || {
            ready_rx.recv().expect("wait for runtime to block");
            handle.request_render();
        });

        runtime.run_with_before_wait(|| {
            let _ = ready_tx.send(());
        });

        join.join().expect("join render thread");
        assert_eq!(state.borrow().renders, baseline + 1);
    }

    #[test]
    fn render_request_during_render_is_preserved_for_next_tick() {
        struct RenderDuringRender {
            state: Rc<RefCell<RenderState>>,
            handle_slot: Rc<RefCell<Option<RenderHandle>>>,
            requested: bool,
        }

        impl Component for RenderDuringRender {
            fn render(&mut self, _width: usize) -> Vec<String> {
                self.state.borrow_mut().renders += 1;
                if !self.requested {
                    self.requested = true;
                    if let Some(handle) = self.handle_slot.borrow().as_ref() {
                        handle.request_render();
                    }
                }
                Vec::new()
            }
        }

        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let handle_slot: Rc<RefCell<Option<RenderHandle>>> = Rc::new(RefCell::new(None));
        let component = RenderDuringRender {
            state: Rc::clone(&state),
            handle_slot: Rc::clone(&handle_slot),
            requested: false,
        };
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(component)));
        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.set_coalesce_budget_for_tests(CoalesceBudget {
            max_duration: Duration::from_secs(10),
            max_iterations: 1,
        });

        runtime.start().expect("runtime start");
        *handle_slot.borrow_mut() = Some(runtime.render_handle());

        runtime.request_render();
        runtime.run_blocking_once();
        assert_eq!(state.borrow().renders, 1);

        runtime.run_blocking_once();
        assert_eq!(state.borrow().renders, 2);
    }

    #[test]
    fn coalesces_multiple_events_into_single_render() {
        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(component)));
        let mut runtime = TuiRuntime::new(terminal, Rc::clone(&root));
        runtime.set_coalesce_budget_for_tests(CoalesceBudget {
            max_duration: Duration::from_secs(10),
            max_iterations: 4,
        });

        runtime.start().expect("runtime start");
        runtime.set_focus(Rc::clone(&root));
        runtime.render_if_needed();
        let baseline = state.borrow().renders;

        runtime.wake.enqueue_input("a".to_string());
        runtime.wake.enqueue_input("b".to_string());
        runtime.wake.signal_resize();
        runtime.request_render();

        runtime.run_blocking_once();
        assert_eq!(state.borrow().renders, baseline + 1);
    }

    #[test]
    fn title_handle_flushes_without_render() {
        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(component)));
        let mut runtime = TuiRuntime::new(terminal, root);

        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        let baseline = state.borrow().renders;
        runtime.terminal.output.clear();

        let handle = runtime.render_handle();
        let join = thread::spawn(move || {
            handle.set_title("pi");
        });
        join.join().expect("join title thread");

        runtime.run_once();
        assert_eq!(state.borrow().renders, baseline);
        assert_eq!(runtime.terminal.output, "\x1b]0;pi\x07");
    }

    #[test]
    fn title_handle_wakes_blocking_run() {
        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(component)));
        let mut runtime = TuiRuntime::new(terminal, root);

        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        let baseline = state.borrow().renders;
        runtime.terminal.output.clear();

        let handle = runtime.render_handle();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let join = thread::spawn(move || {
            ready_rx.recv().expect("wait for runtime to block");
            handle.set_title("pi");
        });

        runtime.run_with_before_wait(|| {
            let _ = ready_tx.send(());
        });

        join.join().expect("join title thread");
        assert_eq!(state.borrow().renders, baseline);
        assert_eq!(runtime.terminal.output, "\x1b]0;pi\x07");
    }

    #[test]
    fn title_last_wins_coalescing() {
        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(component)));
        let mut runtime = TuiRuntime::new(terminal, root);

        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        let baseline = state.borrow().renders;
        runtime.terminal.output.clear();

        let handle = runtime.render_handle();
        handle.set_title("a");
        handle.set_title("b");

        runtime.run_once();
        assert_eq!(state.borrow().renders, baseline);
        assert_eq!(runtime.terminal.output, "\x1b]0;b\x07");
    }

    #[test]
    fn flush_pending_output_flushes_without_render() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        let terminal = TestTerminal::default();
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(DummyComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        runtime.hide_cursor();
        assert!(
            runtime.terminal.output.is_empty(),
            "expected hide_cursor() to enqueue only (no flush), got: {:?}",
            runtime.terminal.output
        );

        runtime.flush_pending_output();
        assert_eq!(runtime.terminal.output, "\x1b[?25l");
        assert!(
            !runtime.terminal.output.contains("\x1b[?2026h"),
            "expected no render sync start bytes, got: {:?}",
            runtime.terminal.output
        );
    }

    #[test]
    fn flush_pending_output_is_noop_when_stopped() {
        let terminal = TestTerminal::default();
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(DummyComponent)));
        let mut runtime = TuiRuntime::new(terminal, root);

        runtime.output.push(TerminalCmd::HideCursor);
        runtime.flush_pending_output();

        assert!(
            runtime.terminal.output.is_empty(),
            "expected no writes when stopped, got: {:?}",
            runtime.terminal.output
        );
    }

    fn env_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[derive(Default, Debug)]
    struct TrackingState {
        writes: String,
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
        ) -> std::io::Result<()> {
            Ok(())
        }

        fn stop(&mut self) -> std::io::Result<()> {
            let mut state = self.state.lock().expect("tracking state lock poisoned");
            state.stop_calls += 1;
            Ok(())
        }

        fn drain_input(&mut self, max_ms: u64, idle_ms: u64) {
            let mut state = self.state.lock().expect("tracking state lock poisoned");
            state.drain_input_calls += 1;
            state.drain_max_ms = Some(max_ms);
            state.drain_idle_ms = Some(idle_ms);
        }

        fn write(&mut self, data: &str) {
            let mut state = self.state.lock().expect("tracking state lock poisoned");
            state.writes.push_str(data);
        }

        fn columns(&self) -> u16 {
            80
        }

        fn rows(&self) -> u16 {
            24
        }
    }

    #[test]
    fn drop_stops_terminal_when_started() {
        let state = Arc::new(Mutex::new(TrackingState::default()));
        let terminal = TrackingTerminal::new(Arc::clone(&state));
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(DummyComponent)));

        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.start().expect("runtime start");
        drop(runtime);

        TrackingTerminal::with_state(&state, |state| {
            assert!(
                state.writes.contains("\x1b[?25h"),
                "expected show-cursor bytes in output, got: {:?}",
                state.writes
            );
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
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(DummyComponent)));

        let mut runtime = TuiRuntime::new(terminal, root);
        runtime.start().expect("runtime start");
        runtime.stop().expect("runtime stop");
        drop(runtime);

        TrackingTerminal::with_state(&state, |state| {
            assert!(
                state.writes.contains("\x1b[?25h"),
                "expected show-cursor bytes in output, got: {:?}",
                state.writes
            );
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
        let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(DummyComponent)));

        drop(TuiRuntime::new(terminal, root));

        TrackingTerminal::with_state(&state, |state| {
            assert!(
                state.writes.is_empty(),
                "unexpected writes: {:?}",
                state.writes
            );
            assert_eq!(state.drain_input_calls, 0);
            assert_eq!(state.stop_calls, 0);
        });
    }
}
