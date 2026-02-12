//! TUI runtime.

use std::collections::VecDeque;
use std::env;
use std::io;
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
use crate::render::renderer::DiffRenderer;
use crate::render::surface::{composite_surfaces, resolve_surface_layout, RenderedSurface};
use crate::render::Frame;
use crate::runtime::component_registry::{ComponentId, ComponentRegistry};
use crate::runtime::ime::position_hardware_cursor;
use crate::runtime::inline_viewport::InlineViewportState;
use crate::runtime::surface::{
    SurfaceEntry, SurfaceId, SurfaceInputPolicy, SurfaceKind, SurfaceOptions, SurfaceRenderEntry,
    SurfaceState,
};

const STOP_DRAIN_MAX_MS: u64 = 1000;
const STOP_DRAIN_IDLE_MS: u64 = 50;
const COALESCE_MAX_DURATION_MS: u64 = 2;
const COALESCE_MAX_ITERATIONS: usize = 8;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DispatchResult {
    Consumed,
    Ignored,
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
    components: ComponentRegistry,
    root: Vec<ComponentId>,
    focused: Option<ComponentId>,
    renderer: DiffRenderer,
    surfaces: SurfaceState,
    on_debug: Option<Box<dyn FnMut()>>,
    on_diagnostic: Option<Box<dyn FnMut(&str)>>,
    clear_on_shrink: bool,
    show_hardware_cursor: bool,
    stopped: bool,
    wake: Arc<RuntimeWake>,
    coalesce_budget: CoalesceBudget,
    inline_viewport: InlineViewportState,
    input_buffer: String,
    cell_size_query_pending: bool,
    kitty_keyboard_enabled: bool,
    kitty_enable_pending: bool,
    #[cfg(all(unix, not(test)))]
    signal_hook_guard: Option<crate::platform::SignalHookGuard>,
    #[cfg(all(unix, not(test)))]
    panic_hook_guard: Option<crate::platform::PanicHookGuard>,
}

/// Handle used to mutate a shown surface entry.
///
/// Canonical lifecycle:
/// 1. create a component,
/// 2. show it via [`TuiRuntime::show_surface`] (or [`RuntimeHandle::show_surface`] from another
///    thread),
/// 3. mutate visibility/options through this handle,
/// 4. remove it with [`SurfaceHandle::hide`] / [`SurfaceHandle::close`].
pub struct SurfaceHandle {
    id: SurfaceId,
    runtime: RuntimeHandle,
}

/// Explicit escape hatch for *direct raw terminal writes*.
///
/// This type is feature-gated behind `unsafe-terminal-access` because it bypasses the runtime's
/// single write gate (`OutputGate::flush(..)`) and can easily desync the renderer from the actual
/// terminal state.
///
/// # Self-healing contract
/// Any raw write can move the cursor, write arbitrary bytes, or otherwise perturb the terminal.
/// The runtime cannot query terminal state to fully recover, so `Drop` performs the minimal
/// "self-healing" resync currently supported:
/// - request a full viewport redraw next render (no scrollback clear), and
/// - request a render so the next tick repaints the viewport even if content is unchanged.
///
/// Limitations:
/// - The guard does **not** clear the screen or reset the renderer baseline.
/// - Raw terminal access must not scroll, clear, or otherwise leave the cursor/screen state
///   incompatible with subsequent diff renders.
///
/// Importantly, we still preserve the runtime's *deterministic* output ordering: `Drop` only
/// schedules a redraw; the actual bytes still flow through the normal output gate and flush at
/// tick boundaries.
#[cfg(feature = "unsafe-terminal-access")]
pub struct TerminalGuard<'a, T: Terminal> {
    runtime: &'a mut TuiRuntime<T>,
}

#[cfg(feature = "unsafe-terminal-access")]
impl<'a, T: Terminal> TerminalGuard<'a, T> {
    /// Write raw bytes directly to the underlying terminal.
    ///
    /// This bypasses `OutputGate`, so callers are responsible for preserving terminal state
    /// compatibility with the diff renderer contract.
    pub fn write_raw(&mut self, data: &str) {
        self.runtime.terminal.write(data);
    }
}

#[cfg(feature = "unsafe-terminal-access")]
impl<'a, T: Terminal> Drop for TerminalGuard<'a, T> {
    fn drop(&mut self) {
        // Raw terminal access can arbitrarily desync the renderer's cursor/screen bookkeeping.
        // Force a self-healing resync: request a full redraw next tick so the viewport is repainted.
        // Do not flush here; keep flushing at tick boundaries.
        self.runtime.request_full_redraw();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomCommandError {
    MissingComponentId(ComponentId),
    MissingSurfaceId(SurfaceId),
    InvalidState(String),
    Message(String),
}

impl std::fmt::Display for CustomCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingComponentId(component_id) => {
                write!(f, "missing component id {}", component_id.raw())
            }
            Self::MissingSurfaceId(surface_id) => {
                write!(f, "missing surface id {}", surface_id.raw())
            }
            Self::InvalidState(message) => write!(f, "invalid state: {message}"),
            Self::Message(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CustomCommandError {}

fn format_runtime_diagnostic(level: &str, code: &str, message: &str) -> String {
    format!("[tape_tui][{level}][{code}] {message}")
}

pub trait CustomCommand: Send + 'static {
    fn name(&self) -> &'static str;
    fn apply(self: Box<Self>, ctx: &mut CustomCommandCtx) -> Result<(), CustomCommandError>;
}

trait CustomCommandRuntimeOps {
    fn terminal(&mut self, op: TerminalOp) -> bool;
    fn focus_set(&mut self, target: Option<ComponentId>) -> Result<bool, CustomCommandError>;
    fn show_surface(
        &mut self,
        surface_id: SurfaceId,
        component_id: ComponentId,
        options: Option<SurfaceOptions>,
        hidden: bool,
    ) -> Result<bool, CustomCommandError>;
    fn hide_surface(&mut self, surface_id: SurfaceId) -> Result<bool, CustomCommandError>;
    fn set_surface_hidden(
        &mut self,
        surface_id: SurfaceId,
        hidden: bool,
    ) -> Result<bool, CustomCommandError>;
    fn update_surface_options(
        &mut self,
        surface_id: SurfaceId,
        options: Option<SurfaceOptions>,
    ) -> Result<bool, CustomCommandError>;
    fn with_component_mut_dyn(
        &mut self,
        component_id: ComponentId,
        f: &mut dyn FnMut(&mut dyn Component),
    ) -> Result<(), CustomCommandError>;
}

pub struct CustomCommandCtx<'a> {
    runtime: &'a mut dyn CustomCommandRuntimeOps,
    pending_title: &'a mut Option<String>,
    render_requested: &'a mut bool,
}

impl<'a> CustomCommandCtx<'a> {
    fn new(
        runtime: &'a mut dyn CustomCommandRuntimeOps,
        pending_title: &'a mut Option<String>,
        render_requested: &'a mut bool,
    ) -> Self {
        Self {
            runtime,
            pending_title,
            render_requested,
        }
    }

    pub fn terminal(&mut self, op: TerminalOp) {
        if self.runtime.terminal(op) {
            self.request_render();
        }
    }

    pub fn request_render(&mut self) {
        *self.render_requested = true;
    }

    pub fn set_title(&mut self, title: String) {
        *self.pending_title = Some(title);
    }

    pub fn focus_set(&mut self, target: Option<ComponentId>) -> Result<(), CustomCommandError> {
        if self.runtime.focus_set(target)? {
            self.request_render();
        }
        Ok(())
    }

    pub fn show_surface(
        &mut self,
        surface_id: SurfaceId,
        component_id: ComponentId,
        options: Option<SurfaceOptions>,
        hidden: bool,
    ) -> Result<(), CustomCommandError> {
        if self
            .runtime
            .show_surface(surface_id, component_id, options, hidden)?
        {
            self.request_render();
        }
        Ok(())
    }

    pub fn hide_surface(&mut self, surface_id: SurfaceId) -> Result<(), CustomCommandError> {
        if self.runtime.hide_surface(surface_id)? {
            self.request_render();
        }
        Ok(())
    }

    pub fn set_surface_hidden(
        &mut self,
        surface_id: SurfaceId,
        hidden: bool,
    ) -> Result<(), CustomCommandError> {
        if self.runtime.set_surface_hidden(surface_id, hidden)? {
            self.request_render();
        }
        Ok(())
    }

    pub fn update_surface_options(
        &mut self,
        surface_id: SurfaceId,
        options: Option<SurfaceOptions>,
    ) -> Result<(), CustomCommandError> {
        if self.runtime.update_surface_options(surface_id, options)? {
            self.request_render();
        }
        Ok(())
    }

    pub fn with_component_mut<R, F>(
        &mut self,
        component_id: ComponentId,
        f: F,
    ) -> Result<R, CustomCommandError>
    where
        F: FnOnce(&mut dyn Component) -> R,
    {
        let mut f = Some(f);
        let mut result: Option<R> = None;
        self.runtime
            .with_component_mut_dyn(component_id, &mut |component| {
                let f = f
                    .take()
                    .expect("custom command with_component_mut closure already consumed");
                result = Some(f(component));
            })?;
        Ok(result.expect("custom command with_component_mut closure did not run"))
    }
}

pub enum Command {
    RequestRender,
    RequestStop,
    /// Update terminal title without forcing a render.
    SetTitle(String),
    RootSet(Vec<ComponentId>),
    RootPush(ComponentId),
    FocusSet(ComponentId),
    FocusClear,
    ShowSurface {
        surface_id: SurfaceId,
        component: ComponentId,
        options: Option<SurfaceOptions>,
        hidden: bool,
    },
    HideSurface(SurfaceId),
    SetSurfaceHidden {
        surface_id: SurfaceId,
        hidden: bool,
    },
    UpdateSurfaceOptions {
        surface_id: SurfaceId,
        options: Option<SurfaceOptions>,
    },
    Terminal(TerminalOp),
    Custom(Box<dyn CustomCommand>),
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequestRender => write!(f, "RequestRender"),
            Self::RequestStop => write!(f, "RequestStop"),
            Self::SetTitle(title) => f.debug_tuple("SetTitle").field(title).finish(),
            Self::RootSet(components) => f.debug_tuple("RootSet").field(components).finish(),
            Self::RootPush(component_id) => f.debug_tuple("RootPush").field(component_id).finish(),
            Self::FocusSet(component_id) => f.debug_tuple("FocusSet").field(component_id).finish(),
            Self::FocusClear => write!(f, "FocusClear"),
            Self::ShowSurface {
                surface_id,
                component,
                options,
                hidden,
            } => f
                .debug_struct("ShowSurface")
                .field("surface_id", surface_id)
                .field("component", component)
                .field("options", options)
                .field("hidden", hidden)
                .finish(),
            Self::HideSurface(surface_id) => {
                f.debug_tuple("HideSurface").field(surface_id).finish()
            }
            Self::SetSurfaceHidden { surface_id, hidden } => f
                .debug_struct("SetSurfaceHidden")
                .field("surface_id", surface_id)
                .field("hidden", hidden)
                .finish(),
            Self::UpdateSurfaceOptions {
                surface_id,
                options,
            } => f
                .debug_struct("UpdateSurfaceOptions")
                .field("surface_id", surface_id)
                .field("options", options)
                .finish(),
            Self::Terminal(op) => f.debug_tuple("Terminal").field(op).finish(),
            Self::Custom(command) => f.debug_tuple("Custom").field(&command.name()).finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalOp {
    ShowCursor,
    HideCursor,
    ClearLine,
    ClearFromCursor,
    ClearScreen,
    MoveBy(i32),
    /// Request that the next render redraw the full viewport.
    RequestFullRedraw,
}

#[derive(Default)]
struct RuntimeWakeState {
    next_surface_id: u64,
    pending_inputs: Vec<String>,
    pending_resize: bool,
    pending_commands: VecDeque<Command>,
    render_requested: bool,
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
            && state.pending_commands.is_empty()
            && !state.render_requested
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

    fn set_render_requested(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.render_requested = true;
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

    fn enqueue_command(&self, command: Command) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.pending_commands.push_back(command);
        self.cvar.notify_one();
    }

    fn drain_inputs(&self) -> Vec<String> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut state.pending_inputs)
    }

    fn drain_commands(&self) -> VecDeque<Command> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut state.pending_commands)
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
        state.pending_resize
            || !state.pending_inputs.is_empty()
            || !state.pending_commands.is_empty()
    }

    fn reset_for_start(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.stop_requested = false;
        state.pending_resize = false;
        state.pending_inputs.clear();
        state.pending_commands.clear();
        state.render_requested = false;
    }

    fn alloc_surface_id(&self) -> SurfaceId {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let next = state.next_surface_id;
        state.next_surface_id = state
            .next_surface_id
            .checked_add(1)
            .expect("surface id overflowed u64");
        SurfaceId::from_raw(next)
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
            && state.pending_commands.is_empty()
            && !state.render_requested
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
pub struct RuntimeHandle {
    wake: Arc<RuntimeWake>,
}

impl RuntimeHandle {
    pub fn dispatch(&self, command: Command) {
        self.wake.enqueue_command(command);
    }

    pub fn alloc_surface_id(&self) -> SurfaceId {
        self.wake.alloc_surface_id()
    }

    /// Queue showing a surface from a background/context handle.
    ///
    /// Prefer [`TuiRuntime::show_surface`] when you are already on the runtime thread.
    pub fn show_surface(
        &self,
        component_id: ComponentId,
        options: Option<SurfaceOptions>,
        hidden: bool,
    ) -> SurfaceHandle {
        let id = self.alloc_surface_id();
        self.dispatch(Command::ShowSurface {
            surface_id: id,
            component: component_id,
            options,
            hidden,
        });
        SurfaceHandle {
            id,
            runtime: self.clone(),
        }
    }
}

impl SurfaceHandle {
    /// Hide (remove) this surface from the runtime stack.
    pub fn hide(&self) {
        self.runtime.dispatch(Command::HideSurface(self.id));
    }

    /// Alias for hide for API readability in host code.
    pub fn close(&self) {
        self.hide();
    }

    /// Set visibility state without removing the surface from the stack.
    pub fn set_hidden(&self, hidden: bool) {
        self.runtime.dispatch(Command::SetSurfaceHidden {
            surface_id: self.id,
            hidden,
        });
    }

    /// Convenience helper to unhide a previously hidden surface.
    pub fn show(&self) {
        self.set_hidden(false);
    }

    /// Replace this surface's options in-place.
    pub fn update_options(&self, options: Option<SurfaceOptions>) {
        self.runtime.dispatch(Command::UpdateSurfaceOptions {
            surface_id: self.id,
            options,
        });
    }
}

impl<T: Terminal> CustomCommandRuntimeOps for TuiRuntime<T> {
    fn terminal(&mut self, op: TerminalOp) -> bool {
        self.apply_terminal_op(op)
    }

    fn focus_set(&mut self, target: Option<ComponentId>) -> Result<bool, CustomCommandError> {
        if let Some(component_id) = target {
            if self.components.get_mut(component_id).is_none() {
                return Err(CustomCommandError::MissingComponentId(component_id));
            }
        }
        self.set_focused(target);
        Ok(true)
    }

    fn show_surface(
        &mut self,
        surface_id: SurfaceId,
        component_id: ComponentId,
        options: Option<SurfaceOptions>,
        hidden: bool,
    ) -> Result<bool, CustomCommandError> {
        if self.components.get_mut(component_id).is_none() {
            return Err(CustomCommandError::MissingComponentId(component_id));
        }
        if self.apply_show_surface(surface_id, component_id, options, hidden) {
            Ok(true)
        } else {
            Err(CustomCommandError::InvalidState(
                "failed to show surface".to_string(),
            ))
        }
    }

    fn hide_surface(&mut self, surface_id: SurfaceId) -> Result<bool, CustomCommandError> {
        if !self.surfaces.contains(surface_id) {
            return Err(CustomCommandError::MissingSurfaceId(surface_id));
        }
        if self.apply_hide_surface(surface_id) {
            Ok(true)
        } else {
            Err(CustomCommandError::InvalidState(
                "failed to hide surface".to_string(),
            ))
        }
    }

    fn set_surface_hidden(
        &mut self,
        surface_id: SurfaceId,
        hidden: bool,
    ) -> Result<bool, CustomCommandError> {
        if !self.surfaces.contains(surface_id) {
            return Err(CustomCommandError::MissingSurfaceId(surface_id));
        }
        if self.apply_set_surface_hidden(surface_id, hidden) {
            Ok(true)
        } else {
            Err(CustomCommandError::InvalidState(
                "surface hidden state unchanged".to_string(),
            ))
        }
    }

    fn update_surface_options(
        &mut self,
        surface_id: SurfaceId,
        options: Option<SurfaceOptions>,
    ) -> Result<bool, CustomCommandError> {
        if !self.surfaces.contains(surface_id) {
            return Err(CustomCommandError::MissingSurfaceId(surface_id));
        }
        if self.apply_update_surface_options(surface_id, options) {
            Ok(true)
        } else {
            Err(CustomCommandError::InvalidState(
                "failed to update surface options".to_string(),
            ))
        }
    }

    fn with_component_mut_dyn(
        &mut self,
        component_id: ComponentId,
        f: &mut dyn FnMut(&mut dyn Component),
    ) -> Result<(), CustomCommandError> {
        let component = self
            .components
            .get_mut(component_id)
            .ok_or(CustomCommandError::MissingComponentId(component_id))?;
        f(component.as_mut());
        Ok(())
    }
}

impl<T: Terminal> TuiRuntime<T> {
    pub fn new(terminal: T) -> Self {
        let clear_on_shrink = env_flag("TAPE_CLEAR_ON_SHRINK");
        let show_hardware_cursor = env_flag("TAPE_HARDWARE_CURSOR");
        Self {
            terminal,
            output: OutputGate::new(),
            terminal_image_state: Arc::new(TerminalImageState::default()),
            components: ComponentRegistry::new(),
            root: Vec::new(),
            focused: None,
            renderer: DiffRenderer::new(),
            surfaces: SurfaceState::default(),
            on_debug: None,
            on_diagnostic: None,
            clear_on_shrink,
            show_hardware_cursor,
            stopped: true,
            wake: Arc::new(RuntimeWake::default()),
            coalesce_budget: CoalesceBudget::default(),
            inline_viewport: InlineViewportState::default(),
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

    /// Install a diagnostics sink for runtime warnings/errors.
    ///
    /// Diagnostics are always emitted in release builds. If no sink is installed, they are written
    /// to stderr.
    pub fn set_on_diagnostic(&mut self, handler: Option<Box<dyn FnMut(&str)>>) {
        self.on_diagnostic = handler;
    }

    #[cfg(test)]
    fn set_coalesce_budget_for_tests(&mut self, budget: CoalesceBudget) {
        self.coalesce_budget = budget;
    }

    pub fn runtime_handle(&self) -> RuntimeHandle {
        RuntimeHandle {
            wake: Arc::clone(&self.wake),
        }
    }

    /// Feature-gated explicit escape hatch for raw terminal operations.
    ///
    /// This is intended for rare extensions that truly need direct access to the underlying
    /// terminal write path (e.g. to emit an unsupported control sequence).
    ///
    /// # Safety and determinism
    /// - This bypasses the runtime's single write gate, so it is only available when the crate is
    ///   compiled with `--features unsafe-terminal-access`.
    /// - The returned guard is *self-healing*: when dropped it requests a full redraw next render
    ///   and requests a render so the viewport is repainted even if content is unchanged.
    /// - The guard only exposes raw writes (not terminal lifecycle control).
    /// - The guard does not flush the runtime output gate; the requested repaint is emitted on
    ///   the next tick boundary.
    /// - The runtime cannot query terminal state; callers must not scroll/clear/leave cursor state
    ///   incompatible with subsequent diff renders.
    ///
    /// # Panics
    /// Panics if called while the runtime is stopped or if there is pending output in the runtime
    /// output gate (to avoid out-of-order writes).
    #[cfg(feature = "unsafe-terminal-access")]
    pub fn terminal_guard_unsafe(&mut self) -> TerminalGuard<'_, T> {
        assert!(
            !self.stopped,
            "terminal_guard_unsafe() requires a started TuiRuntime; call start() first"
        );
        assert!(
            self.output.is_empty(),
            "terminal_guard_unsafe() requires an empty OutputGate; flush_pending_output()/tick first"
        );
        TerminalGuard { runtime: self }
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
        self.output
            .push(TerminalCmd::Bytes(osc_title_sequence(&title)));
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

    pub fn register_component(&mut self, component: impl Component + 'static) -> ComponentId {
        self.register_component_boxed(Box::new(component))
    }

    pub fn register_component_boxed(&mut self, component: Box<dyn Component>) -> ComponentId {
        self.components.register_boxed(component)
    }

    pub fn set_root(&mut self, components: Vec<ComponentId>) {
        self.root = components;
        self.request_render();
    }

    pub fn push_root(&mut self, component: ComponentId) {
        self.root.push(component);
        self.request_render();
    }

    pub fn set_focus(&mut self, target: ComponentId) {
        self.dispatch_focus_surface_command(Command::FocusSet(target));
    }

    pub fn clear_focus(&mut self) {
        self.dispatch_focus_surface_command(Command::FocusClear);
    }

    /// Show a surface using runtime surface semantics.
    ///
    /// This is the canonical in-thread API for layered UI. Use the returned [`SurfaceHandle`] to
    /// hide/show the surface or update its options without re-registering the component.
    pub fn show_surface(
        &mut self,
        component: ComponentId,
        options: Option<SurfaceOptions>,
    ) -> SurfaceHandle {
        let id = self.wake.alloc_surface_id();
        self.dispatch_focus_surface_command(Command::ShowSurface {
            surface_id: id,
            component,
            options,
            hidden: false,
        });
        SurfaceHandle {
            id,
            runtime: self.runtime_handle(),
        }
    }

    /// Hide (remove) the current topmost surface, if any.
    ///
    /// For targeted lifecycle control of a specific surface, use [`SurfaceHandle::hide`].
    pub fn hide_surface(&mut self) {
        if let Some(surface) = self.surfaces.entries.last().copied() {
            self.dispatch_focus_surface_command(Command::HideSurface(surface.id));
        }
    }

    /// Returns `true` when at least one visible surface is currently active.
    pub fn has_surface(&self) -> bool {
        self.surfaces.has_visible(
            self.terminal.columns() as usize,
            self.terminal.rows() as usize,
        )
    }

    fn dispatch_focus_surface_command(&mut self, command: Command) {
        if self.stopped {
            let mut queue = VecDeque::new();
            queue.push_back(command);
            self.apply_pending_commands(queue);
            return;
        }
        self.wake.enqueue_command(command);
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

            let commands = self.wake.drain_commands();
            if !commands.is_empty() {
                self.apply_pending_commands(commands);
                did_work = true;
            }
            self.reconcile_focus();

            if self.wake.take_pending_resize() {
                self.dispatch_resize_event();
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

        let commands = self.wake.drain_commands();
        if !commands.is_empty() {
            self.apply_pending_commands(commands);
        }
        self.reconcile_focus();

        if self.wake.take_pending_resize() {
            self.dispatch_resize_event();
            self.request_render();
        }

        let inputs = self.wake.drain_inputs();

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

        let (capture_target, fallback_target) = self.input_dispatch_targets();

        let mut dispatch_result = DispatchResult::Ignored;
        for event in events {
            if let InputEvent::Key {
                key_id,
                event_type: KeyEventType::Press,
                ..
            } = &event
            {
                if key_id == "ctrl+shift+d" {
                    if let Some(handler) = self.on_debug.as_mut() {
                        handler();
                    }
                    continue;
                }
            }

            let event_result =
                self.dispatch_event_with_bubbling(&event, capture_target, fallback_target);
            if event_result == DispatchResult::Consumed {
                dispatch_result = DispatchResult::Consumed;
            }
        }

        if dispatch_result == DispatchResult::Consumed {
            self.request_render();
        }
    }

    fn input_dispatch_targets(&self) -> (Option<ComponentId>, Option<ComponentId>) {
        let capture_entry = self.topmost_visible_capture_entry();
        let capture_target = capture_entry.map(|entry| entry.component_id);

        let fallback_target = if let Some(entry) = capture_entry {
            entry
                .pre_focus
                .filter(|target| Some(*target) != capture_target)
                .or_else(|| {
                    self.focused
                        .filter(|target| Some(*target) != capture_target)
                })
                .or_else(|| self.root_input_fallback(capture_target))
        } else {
            self.focused.or_else(|| self.root_input_fallback(None))
        };

        (capture_target, fallback_target)
    }

    fn topmost_visible_capture_entry(&self) -> Option<SurfaceEntry> {
        let columns = self.terminal.columns() as usize;
        let rows = self.terminal.rows() as usize;
        self.surfaces
            .entries
            .iter()
            .rev()
            .find(|entry| {
                entry.is_visible(columns, rows)
                    && entry.input_policy() == SurfaceInputPolicy::Capture
            })
            .copied()
    }

    fn root_input_fallback(&self, excluded: Option<ComponentId>) -> Option<ComponentId> {
        self.root
            .iter()
            .rev()
            .copied()
            .find(|component_id| Some(*component_id) != excluded)
    }

    fn dispatch_event_with_bubbling(
        &mut self,
        event: &InputEvent,
        capture_target: Option<ComponentId>,
        fallback_target: Option<ComponentId>,
    ) -> DispatchResult {
        if let Some(capture_target) = capture_target {
            let capture_result = self.dispatch_event_to_target(capture_target, event);
            if capture_result == DispatchResult::Consumed {
                return DispatchResult::Consumed;
            }
        }

        let Some(fallback_target) = fallback_target else {
            return DispatchResult::Ignored;
        };
        if Some(fallback_target) == capture_target {
            return DispatchResult::Ignored;
        }

        self.dispatch_event_to_target(fallback_target, event)
    }

    fn dispatch_event_to_target(
        &mut self,
        target_id: ComponentId,
        event: &InputEvent,
    ) -> DispatchResult {
        let Some(component) = self.components.get_mut(target_id) else {
            debug_assert!(
                false,
                "input dispatch target component {:?} missing",
                target_id
            );
            if self.focused == Some(target_id) {
                self.focused = None;
            }
            return DispatchResult::Ignored;
        };

        if let InputEvent::Key {
            event_type: KeyEventType::Release,
            ..
        } = event
        {
            if !component.wants_key_release() {
                return DispatchResult::Ignored;
            }
        }

        component.handle_event(event);
        DispatchResult::Consumed
    }

    pub fn request_render(&mut self) {
        self.wake.request_render();
    }

    pub fn render_if_needed(&mut self) {
        if self.stopped {
            return;
        }
        if self.wake.take_render_requested() {
            self.do_render();
        }
        self.flush_output();
    }

    pub fn render_now(&mut self) {
        if self.stopped {
            return;
        }
        let commands = self.wake.drain_commands();
        if !commands.is_empty() {
            self.apply_pending_commands(commands);
        }
        self.reconcile_focus();

        self.wake.clear_render_requested();
        self.do_render();
        self.flush_output();
    }

    fn emit_runtime_diagnostic(&mut self, level: &str, code: &str, message: impl Into<String>) {
        let message = message.into();
        let formatted = format_runtime_diagnostic(level, code, &message);
        if let Some(handler) = self.on_diagnostic.as_mut() {
            handler(&formatted);
            return;
        }
        eprintln!("{formatted}");
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
        let (mut lines, mut cursor_pos) = self.render_root(width, height);

        if self.has_surface() {
            let (composited, surface_cursor) = self.composite_surface_lines(lines, width, height);
            lines = composited;
            if surface_cursor.is_some() {
                cursor_pos = surface_cursor;
            }
        }

        // Components may emit the CURSOR_MARKER APC sequence. Ensure it never
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

        self.inline_viewport.note_terminal_height(height);
        self.inline_viewport.update_total_lines(lines.len());
        cursor_pos = self.inline_viewport.clamp_cursor(cursor_pos);

        // Clamp cursor column to the terminal width to avoid emitting huge `CSI n G` moves.
        if let Some(mut pos) = cursor_pos {
            pos.col = pos.col.min(width.saturating_sub(1));
            cursor_pos = Some(pos);
        }

        let has_surfaces = self.has_surface();
        let frame = Frame::from(lines).with_cursor(cursor_pos);
        let cursor_pos = frame.cursor();
        let total_lines = frame.lines().len();
        let render_cmds =
            self.renderer
                .render(frame, width, height, self.clear_on_shrink, has_surfaces);
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

    fn render_root(&mut self, width: usize, height: usize) -> (Vec<String>, Option<CursorPos>) {
        let root_ids = self.root.clone();
        let mut lines = Vec::new();
        let mut cursor_pos = None;
        for id in root_ids {
            let Some(component) = self.components.get_mut(id) else {
                debug_assert!(false, "root component {:?} missing", id);
                continue;
            };
            component.set_terminal_rows(height);
            let start_row = lines.len();
            let child_lines = component.render(width);
            let child_cursor = component.cursor_pos();
            lines.extend(child_lines);
            if let Some(pos) = child_cursor {
                cursor_pos = Some(CursorPos {
                    row: start_row.saturating_add(pos.row),
                    col: pos.col,
                });
            }
        }
        (lines, cursor_pos)
    }

    fn invalidate_root_components(&mut self) {
        let root_ids = self.root.clone();
        for id in root_ids {
            let Some(component) = self.components.get_mut(id) else {
                debug_assert!(false, "root component {:?} missing", id);
                continue;
            };
            component.invalidate();
        }
    }

    fn apply_pending_commands(&mut self, commands: VecDeque<Command>) {
        // Commands are applied at a single, explicit stage in the tick to preserve deterministic
        // ordering relative to input handling and render decisions.
        let mut pending_title: Option<String> = None;
        let mut render_requested = false;

        for command in commands {
            match command {
                Command::RequestRender => {
                    render_requested = true;
                }
                Command::RequestStop => {
                    self.wake.request_stop();
                }
                Command::SetTitle(title) => {
                    pending_title = Some(title);
                }
                Command::RootSet(components) => {
                    let mut resolved = Vec::with_capacity(components.len());
                    let mut had_missing = false;
                    for component_id in components {
                        if self.components.get_mut(component_id).is_none() {
                            self.emit_runtime_diagnostic(
                                "error",
                                "command.root_set.missing_component_id",
                                format!(
                                    "root set references missing component id {}",
                                    component_id.raw()
                                ),
                            );
                            had_missing = true;
                            continue;
                        }
                        resolved.push(component_id);
                    }
                    if had_missing {
                        continue;
                    }
                    self.root = resolved;
                    render_requested = true;
                }
                Command::RootPush(component) => {
                    if self.components.get_mut(component).is_none() {
                        self.emit_runtime_diagnostic(
                            "error",
                            "command.root_push.missing_component_id",
                            format!(
                                "root push references missing component id {}",
                                component.raw()
                            ),
                        );
                    } else {
                        self.root.push(component);
                        render_requested = true;
                    }
                }
                Command::FocusSet(component_id) => {
                    self.set_focused(Some(component_id));
                    render_requested = true;
                }
                Command::FocusClear => {
                    self.set_focused(None);
                    render_requested = true;
                }
                Command::ShowSurface {
                    surface_id,
                    component,
                    options,
                    hidden,
                } => {
                    if self.apply_show_surface(surface_id, component, options, hidden) {
                        render_requested = true;
                    }
                }
                Command::HideSurface(surface_id) => {
                    if self.apply_hide_surface(surface_id) {
                        render_requested = true;
                    }
                }
                Command::SetSurfaceHidden { surface_id, hidden } => {
                    if self.apply_set_surface_hidden(surface_id, hidden) {
                        render_requested = true;
                    }
                }
                Command::UpdateSurfaceOptions {
                    surface_id,
                    options,
                } => {
                    if self.apply_update_surface_options(surface_id, options) {
                        render_requested = true;
                    }
                }
                Command::Terminal(op) => {
                    if self.apply_terminal_op(op) {
                        render_requested = true;
                    }
                }
                Command::Custom(custom_command) => {
                    let command_name = custom_command.name();
                    let mut ctx =
                        CustomCommandCtx::new(self, &mut pending_title, &mut render_requested);
                    if let Err(error) = custom_command.apply(&mut ctx) {
                        let diagnostic = format!("custom command {command_name} failed: {error}");
                        self.emit_runtime_diagnostic(
                            "error",
                            "command.custom.failed",
                            diagnostic.clone(),
                        );
                        debug_assert!(false, "{diagnostic}");
                    }
                }
            }
        }

        if let Some(title) = pending_title {
            self.output
                .push(TerminalCmd::Bytes(osc_title_sequence(&title)));
        }

        if render_requested {
            self.wake.set_render_requested();
        }
    }

    fn apply_terminal_op(&mut self, op: TerminalOp) -> bool {
        match op {
            TerminalOp::ShowCursor => {
                self.output.push(TerminalCmd::ShowCursor);
                false
            }
            TerminalOp::HideCursor => {
                self.output.push(TerminalCmd::HideCursor);
                false
            }
            TerminalOp::ClearLine => {
                self.output.push(TerminalCmd::ClearLine);
                self.renderer.request_full_redraw_next();
                true
            }
            TerminalOp::ClearFromCursor => {
                self.output.push(TerminalCmd::ClearFromCursor);
                self.renderer.request_full_redraw_next();
                true
            }
            TerminalOp::ClearScreen => {
                self.output.push(TerminalCmd::ClearScreen);
                self.renderer.reset_for_external_clear_screen();
                true
            }
            TerminalOp::MoveBy(lines) => {
                if lines == 0 {
                    return false;
                }
                if lines > 0 {
                    self.output.push(TerminalCmd::MoveDown(lines as usize));
                } else {
                    self.output.push(TerminalCmd::MoveUp((-lines) as usize));
                }
                self.renderer
                    .apply_out_of_band_move_by(lines, self.terminal.rows() as usize);
                false
            }
            TerminalOp::RequestFullRedraw => {
                self.renderer.request_full_redraw_next();
                true
            }
        }
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
                self.invalidate_root_components();
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

    fn apply_show_surface(
        &mut self,
        surface_id: SurfaceId,
        component: ComponentId,
        options: Option<SurfaceOptions>,
        hidden: bool,
    ) -> bool {
        self.apply_show_surface_internal(
            surface_id,
            component,
            options,
            hidden,
            "command.show_surface.missing_component_id",
            "show surface",
        )
    }

    fn apply_show_surface_internal(
        &mut self,
        surface_id: SurfaceId,
        component: ComponentId,
        options: Option<SurfaceOptions>,
        hidden: bool,
        missing_component_code: &'static str,
        action_label: &'static str,
    ) -> bool {
        if self.components.get_mut(component).is_none() {
            self.emit_runtime_diagnostic(
                "error",
                missing_component_code,
                format!(
                    "{action_label} references missing component id {}",
                    component.raw()
                ),
            );
            return false;
        }

        if let Some(existing_index) = self.surfaces.index_of(surface_id) {
            let replaced = self.surfaces.entries.remove(existing_index);
            if replaced.input_policy() == SurfaceInputPolicy::Capture
                && self.focused == Some(replaced.component_id)
            {
                self.restore_focus_after_surface_loss(replaced.pre_focus);
            }
        }

        let pre_focus = self.focused.filter(|focused| *focused != component);
        self.surfaces.entries.push(SurfaceEntry {
            id: surface_id,
            component_id: component,
            options,
            pre_focus,
            hidden,
        });

        let columns = self.terminal.columns() as usize;
        let rows = self.terminal.rows() as usize;
        if let Some(entry) = self.surfaces.entries.last().copied() {
            let is_capture = entry.input_policy() == SurfaceInputPolicy::Capture;
            if !hidden && is_capture && entry.is_visible(columns, rows) {
                self.set_focused(Some(component));
            }
        }

        true
    }

    fn apply_hide_surface(&mut self, surface_id: SurfaceId) -> bool {
        self.apply_hide_surface_internal(
            surface_id,
            "command.hide_surface.missing_surface_id",
            "hide surface",
        )
    }

    fn apply_hide_surface_internal(
        &mut self,
        surface_id: SurfaceId,
        missing_id_code: &'static str,
        action_label: &'static str,
    ) -> bool {
        let Some(index) = self.surfaces.index_of(surface_id) else {
            self.emit_runtime_diagnostic(
                "error",
                missing_id_code,
                format!(
                    "{action_label} references missing surface id {}",
                    surface_id.raw()
                ),
            );
            return false;
        };

        let removed = self.surfaces.entries.remove(index);
        if removed.input_policy() == SurfaceInputPolicy::Capture
            && self.focused == Some(removed.component_id)
        {
            self.restore_focus_after_surface_loss(removed.pre_focus);
        }
        true
    }

    fn apply_set_surface_hidden(&mut self, surface_id: SurfaceId, hidden: bool) -> bool {
        self.apply_set_surface_hidden_internal(
            surface_id,
            hidden,
            "command.set_surface_hidden.missing_surface_id",
            "set surface hidden",
        )
    }

    fn apply_set_surface_hidden_internal(
        &mut self,
        surface_id: SurfaceId,
        hidden: bool,
        missing_id_code: &'static str,
        action_label: &'static str,
    ) -> bool {
        let Some(index) = self.surfaces.index_of(surface_id) else {
            self.emit_runtime_diagnostic(
                "error",
                missing_id_code,
                format!(
                    "{action_label} references missing surface id {}",
                    surface_id.raw()
                ),
            );
            return false;
        };

        if hidden {
            let (component_id, pre_focus, was_capture) = {
                let entry = &mut self.surfaces.entries[index];
                if entry.hidden {
                    return false;
                }
                entry.hidden = true;
                (
                    entry.component_id,
                    entry.pre_focus,
                    entry.input_policy() == SurfaceInputPolicy::Capture,
                )
            };
            if was_capture && self.focused == Some(component_id) {
                self.restore_focus_after_surface_loss(pre_focus);
            }
            return true;
        }

        if !self.surfaces.entries[index].hidden {
            return false;
        }

        let current_focus = self.focused;
        {
            let entry = &mut self.surfaces.entries[index];
            entry.hidden = false;
            if current_focus != Some(entry.component_id) {
                entry.pre_focus = current_focus;
            }
        }

        // Unhiding should make this surface topmost for deterministic focus handoff.
        let entry = self.surfaces.entries.remove(index);
        let component_id = entry.component_id;
        let is_capture = entry.input_policy() == SurfaceInputPolicy::Capture;
        self.surfaces.entries.push(entry);

        let columns = self.terminal.columns() as usize;
        let rows = self.terminal.rows() as usize;
        if is_capture
            && self
                .surfaces
                .entries
                .last()
                .is_some_and(|entry| entry.is_visible(columns, rows))
        {
            self.set_focused(Some(component_id));
        }

        true
    }

    fn apply_update_surface_options(
        &mut self,
        surface_id: SurfaceId,
        options: Option<SurfaceOptions>,
    ) -> bool {
        let Some(index) = self.surfaces.index_of(surface_id) else {
            self.emit_runtime_diagnostic(
                "error",
                "command.update_surface_options.missing_surface_id",
                format!(
                    "update surface options references missing surface id {}",
                    surface_id.raw()
                ),
            );
            return false;
        };

        self.surfaces.entries[index].options = options;
        true
    }

    fn set_focused(&mut self, target: Option<ComponentId>) {
        if self.focused == target {
            return;
        }

        if let Some(previous) = self.focused.take() {
            let Some(component) = self.components.get_mut(previous) else {
                self.emit_runtime_diagnostic(
                    "error",
                    "focus.missing_previous_component_id",
                    format!("focused component id {} is missing", previous.raw()),
                );
                return;
            };
            if let Some(focusable) = component.as_focusable() {
                focusable.set_focused(false);
            }
        }

        let Some(next) = target else {
            return;
        };

        let Some(component) = self.components.get_mut(next) else {
            self.emit_runtime_diagnostic(
                "error",
                "focus.missing_target_component_id",
                format!("focus target component id {} is missing", next.raw()),
            );
            return;
        };
        if let Some(focusable) = component.as_focusable() {
            focusable.set_focused(true);
        }
        self.focused = Some(next);
    }

    fn restore_focus_after_surface_loss(&mut self, pre_focus: Option<ComponentId>) {
        if let Some(pre_focus) = pre_focus {
            if self.components.get_mut(pre_focus).is_some() {
                self.set_focused(Some(pre_focus));
                return;
            }
        }

        if let Some(next_surface) = self.topmost_visible_capture_surface() {
            self.set_focused(Some(next_surface));
            return;
        }

        self.set_focused(None);
    }

    fn visible_surface_snapshot(&self) -> Vec<SurfaceRenderEntry> {
        self.surfaces.visible_snapshot(
            self.terminal.columns() as usize,
            self.terminal.rows() as usize,
        )
    }

    fn composite_surface_lines(
        &mut self,
        lines: Vec<String>,
        width: usize,
        height: usize,
    ) -> (Vec<String>, Option<CursorPos>) {
        let surface_entries = self.visible_surface_snapshot();
        let mut rendered: Vec<(RenderedSurface, Option<CursorPos>)> = Vec::new();

        let mut reserved_top = 0usize;
        let mut reserved_bottom = 0usize;

        for entry in surface_entries {
            let surface_options = entry.options.unwrap_or_default();
            let layout_options =
                surface_options.with_lane_reservations(reserved_top, reserved_bottom);
            let render_options = Some(crate::render::surface::SurfaceOptions::from(
                &layout_options,
            ));

            let layout = resolve_surface_layout(render_options.as_ref(), 0, width, height);
            let Some(component) = self.components.get_mut(entry.component_id) else {
                debug_assert!(
                    false,
                    "surface component {:?} missing during render",
                    entry.component_id
                );
                continue;
            };

            component.set_terminal_rows(height);
            let viewport_rows = layout.max_height.unwrap_or(height);
            component.set_viewport_size(layout.width, viewport_rows);
            let mut surface_lines = component.render(layout.width);
            let mut cursor_pos = component.cursor_pos();
            if let Some(max_height) = layout.max_height {
                if surface_lines.len() > max_height {
                    surface_lines.truncate(max_height);
                }
            }
            if let Some(pos) = cursor_pos {
                if pos.row >= surface_lines.len() {
                    cursor_pos = None;
                }
            }
            let final_layout =
                resolve_surface_layout(render_options.as_ref(), surface_lines.len(), width, height);

            let lane_height = surface_lines.len();
            apply_lane_reservations(
                surface_options.kind,
                lane_height,
                height,
                &mut reserved_top,
                &mut reserved_bottom,
            );

            rendered.push((
                RenderedSurface {
                    lines: surface_lines,
                    row: final_layout.row,
                    col: final_layout.col,
                    width: final_layout.width,
                },
                cursor_pos,
            ));
        }

        let mut min_lines_needed = lines.len();
        for (surface, _) in rendered.iter() {
            min_lines_needed =
                min_lines_needed.max(surface.row.saturating_add(surface.lines.len()));
        }
        let working_height = self.renderer.max_lines_rendered().max(min_lines_needed);
        let viewport_start = working_height.saturating_sub(height);

        let mut surface_cursor: Option<CursorPos> = None;
        for (surface, cursor_pos) in rendered.iter() {
            let Some(cursor_pos) = cursor_pos else {
                continue;
            };
            if cursor_pos.row >= surface.lines.len() || cursor_pos.col >= surface.width {
                continue;
            }
            if is_image_line(&surface.lines[cursor_pos.row]) {
                continue;
            }
            let abs_row = viewport_start
                .saturating_add(surface.row)
                .saturating_add(cursor_pos.row);
            if abs_row < lines.len() && is_image_line(&lines[abs_row]) {
                continue;
            }
            surface_cursor = Some(CursorPos {
                row: abs_row,
                col: surface.col.saturating_add(cursor_pos.col),
            });
        }

        let surfaces = rendered
            .into_iter()
            .map(|(surface, _)| surface)
            .collect::<Vec<_>>();
        let composited = composite_surfaces(
            lines,
            &surfaces,
            width,
            height,
            self.renderer.max_lines_rendered(),
            is_image_line,
        );

        (composited, surface_cursor)
    }

    fn topmost_visible_capture_surface(&self) -> Option<ComponentId> {
        self.surfaces.topmost_visible_component(
            self.terminal.columns() as usize,
            self.terminal.rows() as usize,
            true,
        )
    }

    fn dispatch_resize_event(&mut self) {
        let rows = self.terminal.rows();
        self.inline_viewport.note_terminal_height(rows as usize);

        let event = InputEvent::Resize {
            columns: self.terminal.columns(),
            rows,
        };
        let (capture_target, fallback_target) = self.input_dispatch_targets();
        let _ = self.dispatch_event_with_bubbling(&event, capture_target, fallback_target);
    }

    fn reconcile_focus(&mut self) {
        if let Some(topmost) = self.topmost_visible_capture_surface() {
            self.set_focused(Some(topmost));
            return;
        }

        let Some(focused) = self.focused else {
            return;
        };

        if let Some(entry) = self
            .surfaces
            .entries
            .iter()
            .rev()
            .find(|entry| entry.component_id == focused)
            .copied()
        {
            self.restore_focus_after_surface_loss(entry.pre_focus);
            return;
        }

        if self.components.get_mut(focused).is_none() {
            debug_assert!(false, "focused component {:?} missing", focused);
            self.focused = None;
        }
    }

    fn has_pending_non_render(&self) -> bool {
        self.wake.has_pending_non_render()
    }
}

fn apply_lane_reservations(
    kind: SurfaceKind,
    lane_height: usize,
    viewport_height: usize,
    reserved_top: &mut usize,
    reserved_bottom: &mut usize,
) {
    let bounded_lane_height = lane_height.min(viewport_height);
    match kind {
        SurfaceKind::Toast => {
            *reserved_top = reserved_top
                .saturating_add(bounded_lane_height)
                .min(viewport_height);
        }
        SurfaceKind::AttachmentRow | SurfaceKind::Drawer => {
            *reserved_bottom = reserved_bottom
                .saturating_add(bounded_lane_height)
                .min(viewport_height);
        }
        SurfaceKind::Modal | SurfaceKind::Corner => {}
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
        apply_lane_reservations, find_cell_size_response, CoalesceBudget, Command, ComponentId,
        CrashCleanup, CustomCommand, CustomCommandCtx, CustomCommandError, RuntimeHandle,
        TerminalOp, TuiRuntime,
    };
    use crate::core::component::Component;
    use crate::core::cursor::CursorPos;
    use crate::core::output::TerminalCmd;
    use crate::core::terminal::Terminal;
    use crate::core::terminal_image::get_cell_dimensions;
    use crate::runtime::surface::{
        SurfaceAnchor, SurfaceId, SurfaceInputPolicy, SurfaceKind, SurfaceLayoutOptions,
        SurfaceMargin, SurfaceOptions, SurfaceSizeValue, SurfaceVisibility,
    };
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

    fn runtime_with_root<T: Terminal, C: Component + 'static>(
        terminal: T,
        component: C,
    ) -> (TuiRuntime<T>, ComponentId) {
        let mut runtime = TuiRuntime::new(terminal);
        let root_id = runtime.register_component(component);
        runtime.set_root(vec![root_id]);
        (runtime, root_id)
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

    struct MutableTextComponent {
        text: Rc<RefCell<String>>,
        renders: Rc<RefCell<usize>>,
    }

    impl MutableTextComponent {
        fn new(text: Rc<RefCell<String>>, renders: Rc<RefCell<usize>>) -> Self {
            Self { text, renders }
        }
    }

    impl Component for MutableTextComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            *self.renders.borrow_mut() += 1;
            vec![self.text.borrow().clone()]
        }

        fn handle_event(&mut self, event: &crate::core::input_event::InputEvent) {
            if let crate::core::input_event::InputEvent::Text { text, .. } = event {
                *self.text.borrow_mut() = text.clone();
            }
        }
    }

    struct MutateComponentCustomCommand {
        component_id: ComponentId,
        next_text: String,
    }

    impl CustomCommand for MutateComponentCustomCommand {
        fn name(&self) -> &'static str {
            "mutate_component"
        }

        fn apply(self: Box<Self>, ctx: &mut CustomCommandCtx) -> Result<(), CustomCommandError> {
            let component_id = self.component_id;
            let next_text = self.next_text;
            ctx.with_component_mut(component_id, move |component| {
                let event = crate::core::input_event::InputEvent::Text {
                    raw: next_text.clone(),
                    text: next_text.clone(),
                    event_type: crate::core::input::KeyEventType::Press,
                };
                component.handle_event(&event);
            })?;
            ctx.request_render();
            Ok(())
        }
    }

    struct HideCursorCustomCommand;

    impl CustomCommand for HideCursorCustomCommand {
        fn name(&self) -> &'static str {
            "hide_cursor_terminal_op"
        }

        fn apply(self: Box<Self>, ctx: &mut CustomCommandCtx) -> Result<(), CustomCommandError> {
            ctx.terminal(TerminalOp::HideCursor);
            Ok(())
        }
    }

    struct FailingCustomCommand;

    impl CustomCommand for FailingCustomCommand {
        fn name(&self) -> &'static str {
            "failing_custom_command"
        }

        fn apply(self: Box<Self>, _ctx: &mut CustomCommandCtx) -> Result<(), CustomCommandError> {
            Err(CustomCommandError::Message(
                "intentional custom command failure".to_string(),
            ))
        }
    }

    enum SurfaceMutationAction {
        Show {
            surface_id: SurfaceId,
            component_id: ComponentId,
            options: Option<SurfaceOptions>,
            hidden: bool,
        },
        Hide {
            surface_id: SurfaceId,
        },
        SetHidden {
            surface_id: SurfaceId,
            hidden: bool,
        },
        UpdateOptions {
            surface_id: SurfaceId,
            options: Option<SurfaceOptions>,
        },
    }

    struct SurfaceMutationCustomCommand {
        action: SurfaceMutationAction,
    }

    impl CustomCommand for SurfaceMutationCustomCommand {
        fn name(&self) -> &'static str {
            "surface_mutation_custom_command"
        }

        fn apply(self: Box<Self>, ctx: &mut CustomCommandCtx) -> Result<(), CustomCommandError> {
            match self.action {
                SurfaceMutationAction::Show {
                    surface_id,
                    component_id,
                    options,
                    hidden,
                } => ctx.show_surface(surface_id, component_id, options, hidden),
                SurfaceMutationAction::Hide { surface_id } => ctx.hide_surface(surface_id),
                SurfaceMutationAction::SetHidden { surface_id, hidden } => {
                    ctx.set_surface_hidden(surface_id, hidden)
                }
                SurfaceMutationAction::UpdateOptions {
                    surface_id,
                    options,
                } => ctx.update_surface_options(surface_id, options),
            }
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

    struct StaticLinesComponent {
        lines: Vec<String>,
        cursor: Option<CursorPos>,
    }

    impl Component for StaticLinesComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.lines.clone()
        }

        fn cursor_pos(&self) -> Option<CursorPos> {
            self.cursor
        }
    }

    #[test]
    fn root_stack_concatenates_children_and_offsets_cursor() {
        let terminal = TestTerminal::default();
        let mut runtime = TuiRuntime::new(terminal);
        let first = StaticLinesComponent {
            lines: vec!["one".to_string()],
            cursor: Some(CursorPos { row: 0, col: 0 }),
        };
        let second = StaticLinesComponent {
            lines: vec!["two".to_string(), "three".to_string()],
            cursor: Some(CursorPos { row: 1, col: 2 }),
        };
        let first_id = runtime.register_component(first);
        let second_id = runtime.register_component(second);
        runtime.set_root(vec![first_id, second_id]);

        let (lines, cursor) = runtime.render_root(10, 24);
        assert_eq!(lines, vec!["one", "two", "three"]);
        assert_eq!(cursor, Some(CursorPos { row: 2, col: 2 }));
    }

    #[test]
    fn custom_command_mutates_component_and_requests_single_render() {
        let terminal = TestTerminal::new(20, 5);
        let text = Rc::new(RefCell::new("before".to_string()));
        let renders = Rc::new(RefCell::new(0usize));
        let component = MutableTextComponent::new(Rc::clone(&text), Rc::clone(&renders));
        let (mut runtime, component_id) = runtime_with_root(terminal, component);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        let baseline_renders = *renders.borrow();
        runtime.terminal.output.clear();

        let handle = runtime.runtime_handle();
        handle.dispatch(Command::Custom(Box::new(MutateComponentCustomCommand {
            component_id,
            next_text: "after".to_string(),
        })));

        runtime.run_once();

        assert_eq!(text.borrow().as_str(), "after");
        assert_eq!(*renders.borrow(), baseline_renders + 1);
        assert!(
            runtime.terminal.output.contains("after"),
            "expected updated render output, got: {:?}",
            runtime.terminal.output
        );
    }

    #[test]
    fn custom_command_terminal_ops_flush_only_at_tick_boundary() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        let terminal = TestTerminal::default();
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());
        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        runtime.terminal.output.clear();

        let mut commands = std::collections::VecDeque::new();
        commands.push_back(Command::Custom(Box::new(HideCursorCustomCommand)));
        runtime.apply_pending_commands(commands);

        assert!(
            runtime.terminal.output.is_empty(),
            "expected no direct terminal writes during command apply, got: {:?}",
            runtime.terminal.output
        );
        assert!(
            !runtime.output.is_empty(),
            "expected terminal op to be queued into OutputGate"
        );

        runtime.run_once();
        assert_eq!(runtime.terminal.output, "\x1b[?25l");
    }

    #[test]
    fn custom_command_failure_emits_runtime_diagnostic() {
        let terminal = TestTerminal::default();
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());
        let diagnostics = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = Rc::clone(&diagnostics);
        runtime.set_on_diagnostic(Some(Box::new(move |message| {
            sink.borrow_mut().push(message.to_string());
        })));

        runtime.start().expect("runtime start");
        runtime.render_if_needed();

        let handle = runtime.runtime_handle();
        handle.dispatch(Command::Custom(Box::new(FailingCustomCommand)));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.run_once();
        }));
        if cfg!(debug_assertions) {
            assert!(
                result.is_err(),
                "expected debug_assert panic for failing custom command in debug builds"
            );
        } else {
            assert!(
                result.is_ok(),
                "expected no panic when debug assertions are disabled"
            );
        }

        let diagnostics = diagnostics.borrow();
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert!(
            diagnostic.contains("[tape_tui][error][command.custom.failed]"),
            "expected custom command failure diagnostic, got: {diagnostic:?}"
        );
        assert!(
            diagnostic.contains("failing_custom_command"),
            "expected command name in diagnostic, got: {diagnostic:?}"
        );
    }

    #[test]
    fn custom_command_surface_mutation_lifecycle_supports_visibility_and_option_updates() {
        let terminal = TestTerminal::new(80, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();
        assert!(*root_focus.borrow());

        let surface_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component =
            TestComponent::new(false, Rc::clone(&surface_inputs), Rc::clone(&surface_focus));
        let surface_component_id = runtime.register_component(surface_component);
        let surface_id = SurfaceId::from_raw(777);

        let handle = runtime.runtime_handle();
        handle.dispatch(Command::Custom(Box::new(SurfaceMutationCustomCommand {
            action: SurfaceMutationAction::Show {
                surface_id,
                component_id: surface_component_id,
                options: Some(SurfaceOptions {
                    input_policy: SurfaceInputPolicy::Capture,
                    kind: SurfaceKind::Modal,
                    ..Default::default()
                }),
                hidden: false,
            },
        })));
        runtime.run_once();

        assert_eq!(runtime.surfaces.entries.len(), 1);
        assert!(!runtime.surfaces.entries[0].hidden);
        assert!(*surface_focus.borrow());
        assert!(!*root_focus.borrow());

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();
        handle.dispatch(Command::Custom(Box::new(SurfaceMutationCustomCommand {
            action: SurfaceMutationAction::UpdateOptions {
                surface_id,
                options: Some(SurfaceOptions {
                    input_policy: SurfaceInputPolicy::Passthrough,
                    kind: SurfaceKind::Corner,
                    ..Default::default()
                }),
            },
        })));
        runtime.run_once();

        runtime.handle_input("a");
        assert_eq!(root_inputs.borrow().as_slice(), &["a".to_string()]);
        assert!(surface_inputs.borrow().is_empty());

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();
        handle.dispatch(Command::Custom(Box::new(SurfaceMutationCustomCommand {
            action: SurfaceMutationAction::UpdateOptions {
                surface_id,
                options: Some(SurfaceOptions {
                    input_policy: SurfaceInputPolicy::Capture,
                    kind: SurfaceKind::Modal,
                    ..Default::default()
                }),
            },
        })));
        runtime.run_once();

        runtime.handle_input("b");
        assert!(root_inputs.borrow().is_empty());
        assert_eq!(surface_inputs.borrow().as_slice(), &["b".to_string()]);

        handle.dispatch(Command::Custom(Box::new(SurfaceMutationCustomCommand {
            action: SurfaceMutationAction::SetHidden {
                surface_id,
                hidden: true,
            },
        })));
        runtime.run_once();

        assert!(runtime.surfaces.entries[0].hidden);
        assert!(!*surface_focus.borrow());
        assert!(*root_focus.borrow());

        handle.dispatch(Command::Custom(Box::new(SurfaceMutationCustomCommand {
            action: SurfaceMutationAction::SetHidden {
                surface_id,
                hidden: false,
            },
        })));
        runtime.run_once();

        assert!(!runtime.surfaces.entries[0].hidden);
        assert!(*surface_focus.borrow());
        assert!(!*root_focus.borrow());

        handle.dispatch(Command::Custom(Box::new(SurfaceMutationCustomCommand {
            action: SurfaceMutationAction::Hide { surface_id },
        })));
        runtime.run_once();

        assert!(runtime.surfaces.entries.is_empty());
        assert!(!*surface_focus.borrow());
        assert!(*root_focus.borrow());
    }

    #[test]
    fn custom_command_surface_mutation_missing_id_emits_runtime_diagnostic() {
        let terminal = TestTerminal::default();
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());
        let diagnostics = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = Rc::clone(&diagnostics);
        runtime.set_on_diagnostic(Some(Box::new(move |message| {
            sink.borrow_mut().push(message.to_string());
        })));

        runtime.start().expect("runtime start");
        runtime.render_if_needed();

        let missing_surface_id = SurfaceId::from_raw(4040);
        let handle = runtime.runtime_handle();
        handle.dispatch(Command::Custom(Box::new(SurfaceMutationCustomCommand {
            action: SurfaceMutationAction::Hide {
                surface_id: missing_surface_id,
            },
        })));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.run_once();
        }));
        if cfg!(debug_assertions) {
            assert!(
                result.is_err(),
                "expected debug_assert panic for failing custom command in debug builds"
            );
        } else {
            assert!(
                result.is_ok(),
                "expected no panic when debug assertions are disabled"
            );
        }

        let diagnostics = diagnostics.borrow();
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert!(
            diagnostic.contains("[tape_tui][error][command.custom.failed]"),
            "expected custom command failure diagnostic, got: {diagnostic:?}"
        );
        assert!(
            diagnostic.contains("missing surface id 4040"),
            "expected missing surface id details in diagnostic, got: {diagnostic:?}"
        );
    }

    #[test]
    fn raw_command_invalid_ids_emit_runtime_diagnostics_without_panicking() {
        let mut id_source_runtime = TuiRuntime::new(TestTerminal::default());
        let _ = id_source_runtime.register_component(DummyComponent::default());
        let missing_component_id = id_source_runtime.register_component(DummyComponent::default());

        let terminal = TestTerminal::default();
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());
        let diagnostics = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = Rc::clone(&diagnostics);
        runtime.set_on_diagnostic(Some(Box::new(move |message| {
            sink.borrow_mut().push(message.to_string());
        })));
        runtime.start().expect("runtime start");
        runtime.render_if_needed();

        let handle = runtime.runtime_handle();
        handle.dispatch(Command::RootSet(vec![missing_component_id]));
        handle.dispatch(Command::RootPush(missing_component_id));
        handle.dispatch(Command::FocusSet(missing_component_id));
        handle.dispatch(Command::ShowSurface {
            surface_id: SurfaceId::from_raw(77),
            component: missing_component_id,
            options: None,
            hidden: false,
        });
        handle.dispatch(Command::HideSurface(SurfaceId::from_raw(42)));
        handle.dispatch(Command::SetSurfaceHidden {
            surface_id: SurfaceId::from_raw(55),
            hidden: true,
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.run_once();
        }));
        assert!(
            result.is_ok(),
            "invalid raw command ids must not panic in any build"
        );

        let diagnostics = diagnostics.borrow().join("\n");
        assert!(
            diagnostics.contains("command.root_set.missing_component_id"),
            "expected root-set missing component diagnostic, got: {diagnostics:?}"
        );
        assert!(
            diagnostics.contains("command.root_push.missing_component_id"),
            "expected root-push missing component diagnostic, got: {diagnostics:?}"
        );
        assert!(
            diagnostics.contains("focus.missing_target_component_id"),
            "expected focus target missing component diagnostic, got: {diagnostics:?}"
        );
        assert!(
            diagnostics.contains("command.show_surface.missing_component_id"),
            "expected show-surface missing component diagnostic, got: {diagnostics:?}"
        );
        assert!(
            diagnostics.contains("command.hide_surface.missing_surface_id"),
            "expected hide-surface missing id diagnostic, got: {diagnostics:?}"
        );
        assert!(
            diagnostics.contains("command.set_surface_hidden.missing_surface_id"),
            "expected set-surface-hidden missing id diagnostic, got: {diagnostics:?}"
        );

        assert_eq!(
            runtime.root.len(),
            1,
            "invalid root ids must not mutate root stack"
        );
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());

        let inputs = Rc::new(RefCell::new(Vec::new()));
        let focused = Rc::new(RefCell::new(false));
        let component = TestComponent::new(false, Rc::clone(&inputs), focused);
        let component_id = runtime.register_component(component);
        runtime.set_focus(component_id);
        runtime.handle_input("\x1b[32;1:3u");
        assert!(inputs.borrow().is_empty());

        let inputs_release = Rc::new(RefCell::new(Vec::new()));
        let focused_release = Rc::new(RefCell::new(false));
        let component_release =
            TestComponent::new(true, Rc::clone(&inputs_release), focused_release);
        let component_release_id = runtime.register_component(component_release);
        runtime.set_focus(component_release_id);
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, component);

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
        let (mut runtime, _root_id) = runtime_with_root(terminal, CursorPosComponent);
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, WideCursorComponent);
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, CursorMarkerComponent);
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, MultiLineCursorMarkerComponent);
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
        let (mut runtime, _root_id) =
            runtime_with_root(terminal, CursorMarkerWithMetadataComponent);
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
    fn surface_cursor_is_ignored_when_surface_line_is_image() {
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

        struct SurfaceImageCursorComponent;

        impl Component for SurfaceImageCursorComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["\x1b_Gf=100;data".to_string()]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 4 })
            }
        }

        let terminal = TestTerminal::new(20, 3);
        let (mut runtime, _root_id) = runtime_with_root(terminal, BaseCursorComponent);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        let surface_id = runtime.register_component(SurfaceImageCursorComponent);
        let options = SurfaceLayoutOptions {
            width: Some(SurfaceSizeValue::absolute(10)),
            row: Some(SurfaceSizeValue::absolute(1)),
            col: Some(SurfaceSizeValue::absolute(2)),
            ..Default::default()
        };
        runtime.show_surface(surface_id, Some(SurfaceOptions::from(options)));

        runtime.run_once();

        let output = runtime.terminal.output.as_str();
        assert!(
            output.ends_with("\x1b[2A\x1b[1G\x1b[?25l"),
            "unexpected output suffix: {output:?}"
        );
    }

    #[test]
    fn surface_cursor_metadata_wins_over_base_cursor() {
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

        struct SurfaceCursorComponent;

        impl Component for SurfaceCursorComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["surface".to_string()]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 4 })
            }
        }

        let terminal = TestTerminal::new(20, 3);
        let (mut runtime, _root_id) = runtime_with_root(terminal, BaseCursorComponent);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        let surface_id = runtime.register_component(SurfaceCursorComponent);
        let options = SurfaceLayoutOptions {
            width: Some(SurfaceSizeValue::absolute(10)),
            row: Some(SurfaceSizeValue::absolute(1)),
            col: Some(SurfaceSizeValue::absolute(2)),
            ..Default::default()
        };
        runtime.show_surface(surface_id, Some(SurfaceOptions::from(options)));

        runtime.run_once();

        let output = runtime.terminal.output.as_str();
        assert!(
            output.ends_with("\x1b[1A\x1b[7G\x1b[?25l"),
            "unexpected output suffix: {output:?}"
        );
    }

    struct ViewportRecordingComponent {
        last: Rc<RefCell<Option<(usize, usize)>>>,
    }

    impl ViewportRecordingComponent {
        fn new(last: Rc<RefCell<Option<(usize, usize)>>>) -> Self {
            Self { last }
        }
    }

    impl Component for ViewportRecordingComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }

        fn set_viewport_size(&mut self, cols: usize, rows: usize) {
            *self.last.borrow_mut() = Some((cols, rows));
        }
    }

    #[test]
    fn surface_sets_viewport_size_from_layout_budget() {
        let terminal = TestTerminal::new(20, 10);
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        let last = Rc::new(RefCell::new(None));
        let surface_id =
            runtime.register_component(ViewportRecordingComponent::new(Rc::clone(&last)));
        let options = SurfaceLayoutOptions {
            width: Some(SurfaceSizeValue::absolute(10)),
            max_height: Some(SurfaceSizeValue::absolute(3)),
            ..Default::default()
        };

        runtime.show_surface(surface_id, Some(SurfaceOptions::from(options)));
        runtime.run_once();

        assert_eq!(*last.borrow(), Some((10, 3)));
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, TwoLineComponent);
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
        assert!(
            output.contains("line-a"),
            "expected line-a in output: {output:?}"
        );
        assert!(
            output.contains("line-b"),
            "expected line-b in output: {output:?}"
        );
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, CursorComponent);
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());

        runtime.start().expect("runtime start");
        assert!(!runtime.kitty_protocol_active());

        runtime.handle_input("\x1b[?1u");
        assert!(
            runtime.kitty_protocol_active(),
            "expected kitty pending after query response"
        );

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

        let (mut runtime_a, _root_a) = runtime_with_root(terminal_a, DummyComponent::default());
        let (mut runtime_b, _root_b) = runtime_with_root(terminal_b, DummyComponent::default());

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
    fn surface_focus_handoff_and_restore() {
        let terminal = TestTerminal::new(80, 24);
        let root_focus = Rc::new(RefCell::new(false));
        let root_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&root_focus),
        );
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);

        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();
        assert!(*root_focus.borrow());

        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&surface_focus),
        );
        let surface_id = runtime.register_component(surface_component);
        let handle = runtime.show_surface(surface_id, None);
        runtime.run_once();
        assert!(*surface_focus.borrow());

        handle.hide();
        runtime.run_once();
        assert!(*root_focus.borrow());
    }

    #[test]
    fn surface_visibility_callback_on_resize() {
        let terminal = TestTerminal::new(5, 10);
        let root_focus = Rc::new(RefCell::new(false));
        let root_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&root_focus),
        );
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);

        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&surface_focus),
        );
        let surface_id = runtime.register_component(surface_component);
        let options = SurfaceLayoutOptions {
            visibility: SurfaceVisibility::MinCols(10),
            ..Default::default()
        };

        runtime.show_surface(surface_id, Some(SurfaceOptions::from(options)));
        runtime.run_once();
        assert!(!*surface_focus.borrow());

        runtime.terminal.columns = 20;
        runtime.wake.signal_resize();
        runtime.run_once();
        assert!(*surface_focus.borrow());
    }

    #[test]
    fn surface_set_hidden_unhide_focuses_surface_and_routes_input() {
        let terminal = TestTerminal::new(80, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);

        let surface_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component =
            TestComponent::new(false, Rc::clone(&surface_inputs), Rc::clone(&surface_focus));
        let surface_id = runtime.register_component(surface_component);

        let handle = runtime.show_surface(surface_id, None);
        runtime.run_once();

        handle.set_hidden(true);
        runtime.run_once();
        assert!(*root_focus.borrow());
        assert!(!*surface_focus.borrow());

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();

        handle.set_hidden(false);
        runtime.run_once();
        assert!(*surface_focus.borrow());

        runtime.handle_input("x");
        assert_eq!(surface_inputs.borrow().as_slice(), &["x".to_string()]);
        assert!(root_inputs.borrow().is_empty());
    }

    #[test]
    fn surface_set_hidden_hides_focused_surface_and_restores_previous_focus() {
        let terminal = TestTerminal::new(80, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);

        let surface_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component =
            TestComponent::new(false, Rc::clone(&surface_inputs), Rc::clone(&surface_focus));
        let surface_id = runtime.register_component(surface_component);

        let handle = runtime.show_surface(surface_id, None);
        runtime.run_once();
        assert!(*surface_focus.borrow());

        handle.set_hidden(true);
        runtime.run_once();
        assert!(*root_focus.borrow());

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();

        runtime.handle_input("y");
        assert_eq!(root_inputs.borrow().as_slice(), &["y".to_string()]);
        assert!(surface_inputs.borrow().is_empty());
    }

    #[test]
    fn surface_set_hidden_unhide_moves_focus_even_when_another_surface_is_focused() {
        let terminal = TestTerminal::new(80, 24);

        let root_focus = Rc::new(RefCell::new(false));
        let root_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&root_focus),
        );
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);

        let surface_a_focus = Rc::new(RefCell::new(false));
        let surface_a_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&surface_a_focus),
        );
        let surface_a_id = runtime.register_component(surface_a_component);
        runtime.show_surface(surface_a_id, None);
        runtime.run_once();
        assert!(*surface_a_focus.borrow());

        let surface_b_focus = Rc::new(RefCell::new(false));
        let surface_b_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&surface_b_focus),
        );
        let surface_b_id = runtime.register_component(surface_b_component);
        let surface_b_handle = runtime.show_surface(surface_b_id, None);
        runtime.run_once();
        assert!(*surface_b_focus.borrow());

        surface_b_handle.set_hidden(true);
        runtime.run_once();
        assert!(*surface_a_focus.borrow());
        assert!(!*surface_b_focus.borrow());

        surface_b_handle.set_hidden(false);
        runtime.run_once();
        assert!(*surface_b_focus.borrow());
    }

    #[test]
    fn surface_capture_receives_input_before_root() {
        let terminal = TestTerminal::new(80, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();

        let surface_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component =
            TestComponent::new(false, Rc::clone(&surface_inputs), Rc::clone(&surface_focus));
        let surface_id = runtime.register_component(surface_component);

        runtime.show_surface(
            surface_id,
            Some(SurfaceOptions {
                input_policy: SurfaceInputPolicy::Capture,
                kind: SurfaceKind::Modal,
                ..Default::default()
            }),
        );
        runtime.run_once();

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();
        runtime.handle_input("x");

        assert_eq!(surface_inputs.borrow().as_slice(), &["x".to_string()]);
        assert!(root_inputs.borrow().is_empty());
        assert!(*surface_focus.borrow());
    }

    #[test]
    fn input_routing_precedence_tracks_topmost_visible_capture_surface() {
        let terminal = TestTerminal::new(80, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();

        let surface_a_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_a_focus = Rc::new(RefCell::new(false));
        let surface_a_component = TestComponent::new(
            false,
            Rc::clone(&surface_a_inputs),
            Rc::clone(&surface_a_focus),
        );
        let surface_a_id = runtime.register_component(surface_a_component);
        let surface_a = runtime.show_surface(
            surface_a_id,
            Some(SurfaceOptions {
                input_policy: SurfaceInputPolicy::Capture,
                kind: SurfaceKind::Modal,
                ..Default::default()
            }),
        );
        runtime.run_once();

        let surface_b_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_b_focus = Rc::new(RefCell::new(false));
        let surface_b_component = TestComponent::new(
            false,
            Rc::clone(&surface_b_inputs),
            Rc::clone(&surface_b_focus),
        );
        let surface_b_id = runtime.register_component(surface_b_component);
        let surface_b = runtime.show_surface(
            surface_b_id,
            Some(SurfaceOptions {
                input_policy: SurfaceInputPolicy::Capture,
                kind: SurfaceKind::Modal,
                ..Default::default()
            }),
        );
        runtime.run_once();

        root_inputs.borrow_mut().clear();
        surface_a_inputs.borrow_mut().clear();
        surface_b_inputs.borrow_mut().clear();
        runtime.handle_input("1");

        assert_eq!(surface_b_inputs.borrow().as_slice(), &["1".to_string()]);
        assert!(surface_a_inputs.borrow().is_empty());
        assert!(root_inputs.borrow().is_empty());
        assert!(*surface_b_focus.borrow());

        surface_b.set_hidden(true);
        runtime.run_once();

        root_inputs.borrow_mut().clear();
        surface_a_inputs.borrow_mut().clear();
        surface_b_inputs.borrow_mut().clear();
        runtime.handle_input("2");

        assert_eq!(surface_a_inputs.borrow().as_slice(), &["2".to_string()]);
        assert!(surface_b_inputs.borrow().is_empty());
        assert!(root_inputs.borrow().is_empty());
        assert!(*surface_a_focus.borrow());

        surface_a.update_options(Some(SurfaceOptions {
            input_policy: SurfaceInputPolicy::Capture,
            kind: SurfaceKind::Modal,
            layout: SurfaceLayoutOptions {
                visibility: SurfaceVisibility::MinCols(120),
                ..Default::default()
            },
        }));
        runtime.run_once();

        root_inputs.borrow_mut().clear();
        surface_a_inputs.borrow_mut().clear();
        surface_b_inputs.borrow_mut().clear();
        runtime.handle_input("3");

        assert_eq!(root_inputs.borrow().as_slice(), &["3".to_string()]);
        assert!(surface_a_inputs.borrow().is_empty());
        assert!(surface_b_inputs.borrow().is_empty());
        assert!(*root_focus.borrow());
    }

    #[test]
    fn capture_release_ignored_by_surface_bubbles_to_previous_focus() {
        let terminal = TestTerminal::new(80, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(true, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();

        let surface_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component =
            TestComponent::new(false, Rc::clone(&surface_inputs), Rc::clone(&surface_focus));
        let surface_id = runtime.register_component(surface_component);
        runtime.show_surface(
            surface_id,
            Some(SurfaceOptions {
                input_policy: SurfaceInputPolicy::Capture,
                kind: SurfaceKind::Modal,
                ..Default::default()
            }),
        );
        runtime.run_once();

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();

        runtime.handle_input("\x1b[32;1:3u");

        assert_eq!(
            root_inputs.borrow().as_slice(),
            &["\x1b[32;1:3u".to_string()]
        );
        assert!(surface_inputs.borrow().is_empty());
        assert!(*surface_focus.borrow());
    }

    #[test]
    fn root_fallback_handles_input_when_focus_is_none() {
        let terminal = TestTerminal::new(80, 24);

        let root_a_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_a_focus = Rc::new(RefCell::new(false));
        let root_a_component =
            TestComponent::new(false, Rc::clone(&root_a_inputs), Rc::clone(&root_a_focus));
        let (mut runtime, root_a_id) = runtime_with_root(terminal, root_a_component);

        let root_b_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_b_focus = Rc::new(RefCell::new(false));
        let root_b_component =
            TestComponent::new(false, Rc::clone(&root_b_inputs), Rc::clone(&root_b_focus));
        let root_b_id = runtime.register_component(root_b_component);
        runtime.set_root(vec![root_a_id, root_b_id]);

        runtime.start().expect("runtime start");
        runtime.run_once();

        root_a_inputs.borrow_mut().clear();
        root_b_inputs.borrow_mut().clear();

        runtime.handle_input("z");

        assert!(root_a_inputs.borrow().is_empty());
        assert_eq!(root_b_inputs.borrow().as_slice(), &["z".to_string()]);
    }

    #[test]
    fn visibility_transition_preserves_deterministic_capture_routing() {
        let terminal = TestTerminal::new(20, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();

        let surface_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component =
            TestComponent::new(false, Rc::clone(&surface_inputs), Rc::clone(&surface_focus));
        let surface_id = runtime.register_component(surface_component);
        runtime.show_surface(
            surface_id,
            Some(SurfaceOptions {
                input_policy: SurfaceInputPolicy::Capture,
                kind: SurfaceKind::Modal,
                layout: SurfaceLayoutOptions {
                    visibility: SurfaceVisibility::MinCols(10),
                    ..Default::default()
                },
            }),
        );
        runtime.run_once();

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();
        runtime.handle_input("a");
        assert_eq!(surface_inputs.borrow().as_slice(), &["a".to_string()]);
        assert!(root_inputs.borrow().is_empty());

        runtime.terminal.columns = 5;
        runtime.wake.signal_resize();
        runtime.run_once();

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();
        runtime.handle_input("b");
        assert_eq!(root_inputs.borrow().as_slice(), &["b".to_string()]);
        assert!(surface_inputs.borrow().is_empty());
        assert!(*root_focus.borrow());

        runtime.terminal.columns = 20;
        runtime.wake.signal_resize();
        runtime.run_once();

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();
        runtime.handle_input("c");
        assert_eq!(surface_inputs.borrow().as_slice(), &["c".to_string()]);
        assert!(root_inputs.borrow().is_empty());
        assert!(*surface_focus.borrow());
    }

    #[test]
    fn surface_passthrough_does_not_steal_input_from_root() {
        let terminal = TestTerminal::new(80, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();

        let surface_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component =
            TestComponent::new(false, Rc::clone(&surface_inputs), Rc::clone(&surface_focus));
        let surface_id = runtime.register_component(surface_component);

        runtime.show_surface(
            surface_id,
            Some(SurfaceOptions {
                input_policy: SurfaceInputPolicy::Passthrough,
                kind: SurfaceKind::Corner,
                ..Default::default()
            }),
        );
        runtime.run_once();

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();
        runtime.handle_input("y");

        assert_eq!(root_inputs.borrow().as_slice(), &["y".to_string()]);
        assert!(surface_inputs.borrow().is_empty());
        assert!(*root_focus.borrow());
        assert!(!*surface_focus.borrow());
    }

    #[test]
    fn surface_handle_update_options_switches_input_policy() {
        let terminal = TestTerminal::new(80, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();

        let surface_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component =
            TestComponent::new(false, Rc::clone(&surface_inputs), Rc::clone(&surface_focus));
        let surface_id = runtime.register_component(surface_component);

        let handle = runtime.show_surface(
            surface_id,
            Some(SurfaceOptions {
                input_policy: SurfaceInputPolicy::Passthrough,
                kind: SurfaceKind::Corner,
                ..Default::default()
            }),
        );
        runtime.run_once();

        runtime.handle_input("a");
        assert_eq!(root_inputs.borrow().as_slice(), &["a".to_string()]);
        assert!(surface_inputs.borrow().is_empty());

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();
        handle.update_options(Some(SurfaceOptions {
            input_policy: SurfaceInputPolicy::Capture,
            kind: SurfaceKind::Modal,
            ..Default::default()
        }));
        runtime.run_once();
        runtime.handle_input("b");

        assert_eq!(surface_inputs.borrow().as_slice(), &["b".to_string()]);
        assert!(root_inputs.borrow().is_empty());
        assert!(*surface_focus.borrow());
    }

    #[test]
    fn surface_handle_show_and_close_follow_visibility_lifecycle() {
        let terminal = TestTerminal::new(80, 24);

        let root_focus = Rc::new(RefCell::new(false));
        let root_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&root_focus),
        );
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();

        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&surface_focus),
        );
        let surface_component_id = runtime.register_component(surface_component);
        let handle = runtime.show_surface(
            surface_component_id,
            Some(SurfaceOptions {
                input_policy: SurfaceInputPolicy::Capture,
                kind: SurfaceKind::Modal,
                ..Default::default()
            }),
        );
        runtime.run_once();

        assert_eq!(runtime.surfaces.entries.len(), 1);
        assert!(!runtime.surfaces.entries[0].hidden);
        assert!(*surface_focus.borrow());
        assert!(!*root_focus.borrow());

        handle.set_hidden(true);
        assert!(!runtime.surfaces.entries[0].hidden);
        runtime.run_once();

        assert!(runtime.surfaces.entries[0].hidden);
        assert!(!*surface_focus.borrow());
        assert!(*root_focus.borrow());

        handle.show();
        runtime.run_once();

        assert!(!runtime.surfaces.entries[0].hidden);
        assert!(*surface_focus.borrow());
        assert!(!*root_focus.borrow());

        handle.close();
        runtime.run_once();

        assert!(runtime.surfaces.entries.is_empty());
        assert!(*root_focus.borrow());
    }

    #[test]
    fn runtime_handle_surface_commands_mutate_surface_stack_from_background_path() {
        let terminal = TestTerminal::new(80, 24);

        let root_focus = Rc::new(RefCell::new(false));
        let root_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&root_focus),
        );
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();

        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&surface_focus),
        );
        let surface_component_id = runtime.register_component(surface_component);

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
        runtime.run_once();

        assert_eq!(runtime.surfaces.entries.len(), 1);
        assert_eq!(runtime.surfaces.entries[0].id, surface_id);
        assert!(!runtime.surfaces.entries[0].hidden);
        assert!(*surface_focus.borrow());
        assert!(!*root_focus.borrow());

        handle.dispatch(Command::SetSurfaceHidden {
            surface_id,
            hidden: true,
        });
        runtime.run_once();

        assert!(runtime.surfaces.entries[0].hidden);
        assert!(!*surface_focus.borrow());
        assert!(*root_focus.borrow());

        handle.dispatch(Command::SetSurfaceHidden {
            surface_id,
            hidden: false,
        });
        runtime.run_once();

        assert!(!runtime.surfaces.entries[0].hidden);
        assert!(*surface_focus.borrow());
        assert!(!*root_focus.borrow());

        handle.dispatch(Command::HideSurface(surface_id));
        runtime.run_once();

        assert!(runtime.surfaces.entries.is_empty());
        assert!(*root_focus.borrow());
    }

    #[test]
    fn show_surface_with_reused_id_replaces_previous_entry_deterministically() {
        let terminal = TestTerminal::new(80, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();

        let surface_a_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_a_focus = Rc::new(RefCell::new(false));
        let surface_a_component = TestComponent::new(
            false,
            Rc::clone(&surface_a_inputs),
            Rc::clone(&surface_a_focus),
        );
        let surface_a_id = runtime.register_component(surface_a_component);

        let surface_b_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_b_focus = Rc::new(RefCell::new(false));
        let surface_b_component = TestComponent::new(
            false,
            Rc::clone(&surface_b_inputs),
            Rc::clone(&surface_b_focus),
        );
        let surface_b_id = runtime.register_component(surface_b_component);

        let handle = runtime.runtime_handle();
        let surface_id = handle.alloc_surface_id();
        let capture_options = Some(SurfaceOptions {
            input_policy: SurfaceInputPolicy::Capture,
            kind: SurfaceKind::Modal,
            ..Default::default()
        });

        handle.dispatch(Command::ShowSurface {
            surface_id,
            component: surface_a_id,
            options: capture_options,
            hidden: false,
        });
        handle.dispatch(Command::ShowSurface {
            surface_id,
            component: surface_b_id,
            options: capture_options,
            hidden: false,
        });
        runtime.run_once();

        assert_eq!(runtime.surfaces.entries.len(), 1);
        assert_eq!(runtime.surfaces.entries[0].id, surface_id);
        assert_eq!(runtime.surfaces.entries[0].component_id, surface_b_id);
        assert!(!*surface_a_focus.borrow());
        assert!(*surface_b_focus.borrow());
        assert!(!*root_focus.borrow());

        root_inputs.borrow_mut().clear();
        surface_a_inputs.borrow_mut().clear();
        surface_b_inputs.borrow_mut().clear();
        runtime.handle_input("x");

        assert!(root_inputs.borrow().is_empty());
        assert!(surface_a_inputs.borrow().is_empty());
        assert_eq!(surface_b_inputs.borrow().as_slice(), &["x".to_string()]);

        handle.dispatch(Command::HideSurface(surface_id));
        runtime.run_once();

        assert!(runtime.surfaces.entries.is_empty());
        assert!(*root_focus.borrow());

        root_inputs.borrow_mut().clear();
        surface_b_inputs.borrow_mut().clear();
        runtime.handle_input("y");

        assert_eq!(root_inputs.borrow().as_slice(), &["y".to_string()]);
        assert!(surface_b_inputs.borrow().is_empty());
    }

    #[test]
    fn surface_visibility_command_applies_before_input_in_same_tick() {
        let terminal = TestTerminal::new(80, 24);

        let root_inputs = Rc::new(RefCell::new(Vec::new()));
        let root_focus = Rc::new(RefCell::new(false));
        let root_component =
            TestComponent::new(false, Rc::clone(&root_inputs), Rc::clone(&root_focus));
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();

        let surface_inputs = Rc::new(RefCell::new(Vec::new()));
        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component =
            TestComponent::new(false, Rc::clone(&surface_inputs), Rc::clone(&surface_focus));
        let surface_id = runtime.register_component(surface_component);

        runtime.show_surface(
            surface_id,
            Some(SurfaceOptions {
                input_policy: SurfaceInputPolicy::Capture,
                kind: SurfaceKind::Modal,
                ..Default::default()
            }),
        );
        runtime.run_once();
        assert!(*surface_focus.borrow());

        root_inputs.borrow_mut().clear();
        surface_inputs.borrow_mut().clear();

        let shown_surface_id = runtime.surfaces.entries[0].id;
        let handle = runtime.runtime_handle();
        handle.dispatch(Command::SetSurfaceHidden {
            surface_id: shown_surface_id,
            hidden: true,
        });
        runtime.wake.enqueue_input("z".to_string());

        runtime.run_once();

        assert_eq!(root_inputs.borrow().as_slice(), &["z".to_string()]);
        assert!(surface_inputs.borrow().is_empty());
        assert!(*root_focus.borrow());
        assert!(!*surface_focus.borrow());
    }

    #[test]
    fn topmost_surface_without_cursor_keeps_lower_surface_cursor_winner() {
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

        struct LowerSurfaceCursorComponent;

        impl Component for LowerSurfaceCursorComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["lower".to_string()]
            }

            fn cursor_pos(&self) -> Option<CursorPos> {
                Some(CursorPos { row: 0, col: 1 })
            }
        }

        struct TopSurfaceNoCursorComponent;

        impl Component for TopSurfaceNoCursorComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["top".to_string()]
            }
        }

        let terminal = TestTerminal::new(20, 3);
        let (mut runtime, _root_id) = runtime_with_root(terminal, BaseCursorComponent);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        let lower_surface_id = runtime.register_component(LowerSurfaceCursorComponent);
        runtime.show_surface(
            lower_surface_id,
            Some(SurfaceOptions::from(SurfaceLayoutOptions {
                width: Some(SurfaceSizeValue::absolute(10)),
                row: Some(SurfaceSizeValue::absolute(1)),
                col: Some(SurfaceSizeValue::absolute(2)),
                ..Default::default()
            })),
        );

        let top_surface_id = runtime.register_component(TopSurfaceNoCursorComponent);
        runtime.show_surface(
            top_surface_id,
            Some(SurfaceOptions::from(SurfaceLayoutOptions {
                width: Some(SurfaceSizeValue::absolute(10)),
                row: Some(SurfaceSizeValue::absolute(0)),
                col: Some(SurfaceSizeValue::absolute(0)),
                ..Default::default()
            })),
        );

        runtime.run_once();

        let output = runtime.terminal.output.as_str();
        assert!(
            output.ends_with("\x1b[1A\x1b[4G\x1b[?25l"),
            "expected lower surface cursor to remain winner when top surface has no cursor, got: {output:?}"
        );
    }

    #[test]
    fn lane_reservation_helper_bounds_to_viewport_height() {
        let mut reserved_top = 0usize;
        let mut reserved_bottom = 0usize;

        apply_lane_reservations(
            SurfaceKind::Toast,
            9,
            2,
            &mut reserved_top,
            &mut reserved_bottom,
        );
        assert_eq!(reserved_top, 2);
        assert_eq!(reserved_bottom, 0);

        apply_lane_reservations(
            SurfaceKind::AttachmentRow,
            7,
            2,
            &mut reserved_top,
            &mut reserved_bottom,
        );
        assert_eq!(reserved_top, 2);
        assert_eq!(reserved_bottom, 2);

        apply_lane_reservations(
            SurfaceKind::Drawer,
            5,
            2,
            &mut reserved_top,
            &mut reserved_bottom,
        );
        assert_eq!(reserved_top, 2);
        assert_eq!(reserved_bottom, 2);

        apply_lane_reservations(
            SurfaceKind::Modal,
            99,
            2,
            &mut reserved_top,
            &mut reserved_bottom,
        );
        assert_eq!(reserved_top, 2);
        assert_eq!(reserved_bottom, 2);
    }

    #[test]
    fn tiny_terminal_toast_lane_compositing_preserves_transcript_prefix_order() {
        let terminal = TestTerminal::new(12, 2);
        let root_component = StaticLinesComponent {
            lines: vec![
                "root-0".to_string(),
                "root-1".to_string(),
                "root-2".to_string(),
                "root-3".to_string(),
            ],
            cursor: None,
        };
        let (mut runtime, _root_id) = runtime_with_root(terminal, root_component);

        let toast_a_id = runtime.register_component(StaticLinesComponent {
            lines: vec!["toast-a".to_string()],
            cursor: None,
        });
        let toast_b_id = runtime.register_component(StaticLinesComponent {
            lines: vec!["toast-b".to_string()],
            cursor: None,
        });

        let toast_options = SurfaceOptions {
            input_policy: SurfaceInputPolicy::Passthrough,
            kind: SurfaceKind::Toast,
            layout: SurfaceLayoutOptions {
                width: Some(SurfaceSizeValue::absolute(7)),
                ..Default::default()
            },
        };

        runtime.show_surface(toast_a_id, Some(toast_options));
        runtime.show_surface(toast_b_id, Some(toast_options));

        let (lines, _cursor) = runtime.render_root(12, 2);
        let (composited, _surface_cursor) = runtime.composite_surface_lines(lines, 12, 2);

        assert_eq!(composited.len(), 4);
        assert_eq!(composited[0], "root-0");
        assert_eq!(composited[1], "root-1");
        assert!(composited[2].contains("toast-a"), "{composited:?}");
        assert!(composited[3].contains("toast-b"), "{composited:?}");
    }

    #[test]
    fn surface_modal_geometry_matches_layout_resolution() {
        let layout_options = SurfaceLayoutOptions {
            width: Some(SurfaceSizeValue::percent(55.0)),
            min_width: Some(22),
            max_height: Some(SurfaceSizeValue::percent(60.0)),
            anchor: Some(SurfaceAnchor::BottomRight),
            offset_x: Some(-3),
            offset_y: Some(2),
            margin: Some(SurfaceMargin {
                top: Some(2),
                right: Some(4),
                bottom: Some(3),
                left: Some(1),
            }),
            ..Default::default()
        };

        let layout_render_options = crate::render::surface::SurfaceOptions::from(&layout_options);
        let expected_layout = crate::render::surface::resolve_surface_layout(
            Some(&layout_render_options),
            11,
            132,
            48,
        );

        let surface_options = SurfaceOptions {
            layout: layout_options,
            kind: SurfaceKind::Modal,
            input_policy: SurfaceInputPolicy::Capture,
        };
        let lane_adjusted_layout = surface_options.with_lane_reservations(0, 0);
        let surface_render_options =
            crate::render::surface::SurfaceOptions::from(&lane_adjusted_layout);
        let actual_layout = crate::render::surface::resolve_surface_layout(
            Some(&surface_render_options),
            11,
            132,
            48,
        );

        assert_eq!(actual_layout, expected_layout);
    }

    #[test]
    fn surface_visibility_and_row_col_semantics_match_layout_options() {
        let layout_options = SurfaceLayoutOptions {
            row: Some(SurfaceSizeValue::percent(40.0)),
            col: Some(SurfaceSizeValue::absolute(9)),
            visibility: SurfaceVisibility::MinSize { cols: 90, rows: 30 },
            ..Default::default()
        };

        let surface_options = SurfaceOptions::from(layout_options);

        let visibility_matrix = [
            (120usize, 40usize, true),
            (89usize, 40usize, false),
            (120usize, 29usize, false),
        ];
        for (cols, rows, expected) in visibility_matrix {
            assert_eq!(layout_options.is_visible(cols, rows), expected);
            assert_eq!(surface_options.is_visible(cols, rows), expected);
        }

        let layout_render_options = crate::render::surface::SurfaceOptions::from(&layout_options);
        let expected_layout = crate::render::surface::resolve_surface_layout(
            Some(&layout_render_options),
            7,
            120,
            40,
        );

        let lane_adjusted_layout = surface_options.with_lane_reservations(0, 0);
        let surface_render_options =
            crate::render::surface::SurfaceOptions::from(&lane_adjusted_layout);
        let actual_layout = crate::render::surface::resolve_surface_layout(
            Some(&surface_render_options),
            7,
            120,
            40,
        );

        assert_eq!(actual_layout, expected_layout);
    }

    #[test]
    fn surface_toast_lane_stacks_from_top_without_mutating_transcript_order() {
        let terminal = TestTerminal::new(40, 6);
        let root_component = StaticLinesComponent {
            lines: vec![
                "root-0".to_string(),
                "root-1".to_string(),
                "root-2".to_string(),
            ],
            cursor: None,
        };
        let (mut runtime, _root_id) = runtime_with_root(terminal, root_component);

        let toast_a_id = runtime.register_component(StaticLinesComponent {
            lines: vec!["toast-a".to_string()],
            cursor: None,
        });
        let toast_b_id = runtime.register_component(StaticLinesComponent {
            lines: vec!["toast-b".to_string()],
            cursor: None,
        });

        let toast_options = SurfaceOptions {
            input_policy: SurfaceInputPolicy::Passthrough,
            kind: SurfaceKind::Toast,
            layout: SurfaceLayoutOptions {
                width: Some(SurfaceSizeValue::absolute(10)),
                ..Default::default()
            },
        };

        runtime.show_surface(toast_a_id, Some(toast_options));
        runtime.show_surface(toast_b_id, Some(toast_options));

        let (lines, _cursor) = runtime.render_root(40, 6);
        let (composited, _surface_cursor) = runtime.composite_surface_lines(lines, 40, 6);

        assert!(
            composited[0].contains("toast-a"),
            "expected first toast on first viewport row, got: {:?}",
            composited[0]
        );
        assert!(
            composited[1].contains("toast-b"),
            "expected second toast stacked below first, got: {:?}",
            composited[1]
        );
        assert!(
            composited[2].contains("root-2") || composited[2].contains("root-0"),
            "expected transcript content to remain present after toast compositing, got: {:?}",
            composited
        );
    }

    #[test]
    fn surface_handle_mutations_apply_only_when_commands_are_drained() {
        let terminal = TestTerminal::new(80, 24);

        let root_focus = Rc::new(RefCell::new(false));
        let root_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&root_focus),
        );
        let (mut runtime, root_id) = runtime_with_root(terminal, root_component);
        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
        runtime.run_once();
        assert!(*root_focus.borrow());

        let surface_focus = Rc::new(RefCell::new(false));
        let surface_component = TestComponent::new(
            false,
            Rc::new(RefCell::new(Vec::new())),
            Rc::clone(&surface_focus),
        );
        let surface_component_id = runtime.register_component(surface_component);
        let handle = runtime.show_surface(surface_component_id, None);
        runtime.run_once();
        assert_eq!(runtime.surfaces.entries.len(), 1);
        assert!(*surface_focus.borrow());
        assert!(!*root_focus.borrow());

        handle.set_hidden(true);

        // SurfaceHandle must enqueue commands instead of mutating runtime state inline.
        assert_eq!(runtime.surfaces.entries.len(), 1);
        assert!(!runtime.surfaces.entries[0].hidden);
        assert!(*surface_focus.borrow());
        assert!(!*root_focus.borrow());

        runtime.run_once();
        assert!(runtime.surfaces.entries[0].hidden);
        assert!(!*surface_focus.borrow());
        assert!(*root_focus.borrow());

        handle.hide();
        assert_eq!(runtime.surfaces.entries.len(), 1);

        runtime.run_once();
        assert!(runtime.surfaces.entries.is_empty());
    }

    #[test]
    fn command_show_surface_uses_runtime_surface_options_type() {
        let terminal = TestTerminal::default();
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());
        let surface_component_id = runtime.register_component(DummyComponent::default());
        let surface_id = SurfaceId::from_raw(99);
        let options = SurfaceOptions {
            layout: SurfaceLayoutOptions {
                width: Some(SurfaceSizeValue::absolute(12)),
                ..Default::default()
            },
            ..Default::default()
        };

        let command = Command::ShowSurface {
            surface_id,
            component: surface_component_id,
            options: Some(options),
            hidden: false,
        };

        match command {
            Command::ShowSurface {
                surface_id: seen_id,
                options: Some(seen_options),
                ..
            } => {
                assert_eq!(seen_id, surface_id);
                assert_eq!(seen_options, options);
            }
            _ => panic!("expected show-surface command"),
        }
    }

    #[test]
    fn runtime_handle_triggers_render_from_background_task() {
        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let (mut runtime, _root_id) = runtime_with_root(terminal, component);

        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        let baseline = state.borrow().renders;

        let handle = runtime.runtime_handle();
        let join = thread::spawn(move || {
            handle.dispatch(Command::RequestRender);
        });
        join.join().expect("join render thread");

        runtime.run_once();
        assert_eq!(state.borrow().renders, baseline + 1);
    }

    #[test]
    fn runtime_handle_wakes_blocking_run() {
        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let (mut runtime, _root_id) = runtime_with_root(terminal, component);

        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        let baseline = state.borrow().renders;

        let handle = runtime.runtime_handle();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let join = thread::spawn(move || {
            ready_rx.recv().expect("wait for runtime to block");
            handle.dispatch(Command::RequestRender);
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
            handle_slot: Rc<RefCell<Option<RuntimeHandle>>>,
            requested: bool,
        }

        impl Component for RenderDuringRender {
            fn render(&mut self, _width: usize) -> Vec<String> {
                self.state.borrow_mut().renders += 1;
                if !self.requested {
                    self.requested = true;
                    if let Some(handle) = self.handle_slot.borrow().as_ref() {
                        handle.dispatch(Command::RequestRender);
                    }
                }
                Vec::new()
            }
        }

        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let handle_slot: Rc<RefCell<Option<RuntimeHandle>>> = Rc::new(RefCell::new(None));
        let component = RenderDuringRender {
            state: Rc::clone(&state),
            handle_slot: Rc::clone(&handle_slot),
            requested: false,
        };
        let (mut runtime, _root_id) = runtime_with_root(terminal, component);
        runtime.set_coalesce_budget_for_tests(CoalesceBudget {
            max_duration: Duration::from_secs(10),
            max_iterations: 1,
        });

        runtime.start().expect("runtime start");
        *handle_slot.borrow_mut() = Some(runtime.runtime_handle());

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
        let (mut runtime, root_id) = runtime_with_root(terminal, component);
        runtime.set_coalesce_budget_for_tests(CoalesceBudget {
            max_duration: Duration::from_secs(10),
            max_iterations: 4,
        });

        runtime.start().expect("runtime start");
        runtime.set_focus(root_id);
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, component);

        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        let baseline = state.borrow().renders;
        runtime.terminal.output.clear();

        let handle = runtime.runtime_handle();
        let join = thread::spawn(move || {
            handle.dispatch(Command::SetTitle("tape".to_string()));
        });
        join.join().expect("join title thread");

        runtime.run_once();
        assert_eq!(state.borrow().renders, baseline);
        assert_eq!(runtime.terminal.output, "\x1b]0;tape\x07");
    }

    #[test]
    fn title_handle_wakes_blocking_run() {
        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let (mut runtime, _root_id) = runtime_with_root(terminal, component);

        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        let baseline = state.borrow().renders;
        runtime.terminal.output.clear();

        let handle = runtime.runtime_handle();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let join = thread::spawn(move || {
            ready_rx.recv().expect("wait for runtime to block");
            handle.dispatch(Command::SetTitle("tape".to_string()));
        });

        runtime.run_with_before_wait(|| {
            let _ = ready_tx.send(());
        });

        join.join().expect("join title thread");
        assert_eq!(state.borrow().renders, baseline);
        assert_eq!(runtime.terminal.output, "\x1b]0;tape\x07");
    }

    #[test]
    fn title_last_wins_coalescing() {
        let terminal = TestTerminal::default();
        let state = Rc::new(RefCell::new(RenderState::default()));
        let component = CountingComponent {
            state: Rc::clone(&state),
        };
        let (mut runtime, _root_id) = runtime_with_root(terminal, component);

        runtime.start().expect("runtime start");
        runtime.render_if_needed();
        let baseline = state.borrow().renders;
        runtime.terminal.output.clear();

        let handle = runtime.runtime_handle();
        handle.dispatch(Command::SetTitle("a".to_string()));
        handle.dispatch(Command::SetTitle("b".to_string()));

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
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());

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
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());

        runtime.output.push(TerminalCmd::HideCursor);
        runtime.flush_pending_output();

        assert!(
            runtime.terminal.output.is_empty(),
            "expected no writes when stopped, got: {:?}",
            runtime.terminal.output
        );
    }

    #[test]
    fn render_if_needed_is_noop_when_stopped() {
        struct LabelComponent;

        impl Component for LabelComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["stopped".to_string()]
            }
        }

        let terminal = TestTerminal::new(20, 4);
        let (mut runtime, _root_id) = runtime_with_root(terminal, LabelComponent);
        runtime.request_render();
        runtime.render_if_needed();

        assert!(
            runtime.terminal.output.is_empty(),
            "expected no writes when stopped, got: {:?}",
            runtime.terminal.output
        );
    }

    #[test]
    fn render_now_is_noop_when_stopped() {
        struct LabelComponent;

        impl Component for LabelComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["stopped".to_string()]
            }
        }

        let terminal = TestTerminal::new(20, 4);
        let (mut runtime, _root_id) = runtime_with_root(terminal, LabelComponent);
        runtime.render_now();

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

    #[test]
    fn runtime_handle_hide_cursor_wakes_and_flushes_without_render() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        let terminal = TestTerminal::default();
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());

        runtime.start().expect("runtime start");
        runtime.render_if_needed(); // clear initial render request
        runtime.terminal.output.clear();

        let handle = runtime.runtime_handle();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let join = thread::spawn(move || {
            ready_rx.recv().expect("wait for runtime to block");
            handle.dispatch(Command::Terminal(TerminalOp::HideCursor));
        });

        runtime.run_with_before_wait(|| {
            let _ = ready_tx.send(());
        });

        join.join().expect("join hide cursor thread");

        assert!(
            runtime.terminal.output.contains("\x1b[?25l"),
            "expected hide cursor bytes, got: {:?}",
            runtime.terminal.output
        );
        assert!(
            !runtime.terminal.output.contains("\x1b[?2026h"),
            "expected no render sync start bytes, got: {:?}",
            runtime.terminal.output
        );
    }

    #[test]
    fn commands_apply_before_input_in_same_tick() {
        let terminal = TestTerminal::default();
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());

        let first_inputs = Rc::new(RefCell::new(Vec::new()));
        let first_focus = Rc::new(RefCell::new(false));
        let first_id = runtime.register_component(TestComponent::new(
            false,
            Rc::clone(&first_inputs),
            Rc::clone(&first_focus),
        ));

        let second_inputs = Rc::new(RefCell::new(Vec::new()));
        let second_focus = Rc::new(RefCell::new(false));
        let second_id = runtime.register_component(TestComponent::new(
            false,
            Rc::clone(&second_inputs),
            Rc::clone(&second_focus),
        ));

        runtime.start().expect("runtime start");
        runtime.render_if_needed(); // clear initial render request
        runtime.set_focus(first_id);
        runtime.run_once();
        assert!(*first_focus.borrow());

        let handle = runtime.runtime_handle();
        handle.dispatch(Command::FocusSet(second_id));
        runtime.wake.enqueue_input("x".to_string());

        runtime.run_once();

        assert_eq!(second_inputs.borrow().as_slice(), &["x".to_string()]);
        assert!(first_inputs.borrow().is_empty());
        assert!(*second_focus.borrow());
        assert!(!*first_focus.borrow());
    }

    #[test]
    fn render_now_applies_queued_commands_before_render() {
        struct LabelComponent(&'static str);

        impl Component for LabelComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec![self.0.to_string()]
            }
        }

        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        let terminal = TestTerminal::new(40, 4);
        let (mut runtime, root_a_id) = runtime_with_root(terminal, LabelComponent("root-a"));
        let root_b_id = runtime.register_component(LabelComponent("root-b"));
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.render_now();
        runtime.terminal.output.clear();

        let handle = runtime.runtime_handle();
        handle.dispatch(Command::RootSet(vec![root_b_id]));

        runtime.render_now();

        assert_eq!(runtime.root, vec![root_b_id]);
        assert!(
            runtime.terminal.output.contains("root-b"),
            "expected render_now to apply queued RootSet before rendering, got: {:?}",
            runtime.terminal.output
        );
        assert_ne!(runtime.root, vec![root_a_id]);
    }

    #[test]
    fn render_handle_clear_screen_triggers_redraw() {
        let _guard = env_test_lock().lock().expect("test lock poisoned");
        std::env::remove_var("TERM_PROGRAM");
        std::env::remove_var("KITTY_WINDOW_ID");

        struct HelloComponent;

        impl Component for HelloComponent {
            fn render(&mut self, _width: usize) -> Vec<String> {
                vec!["hello".to_string()]
            }
        }

        let terminal = TestTerminal::new(80, 24);
        let (mut runtime, _root_id) = runtime_with_root(terminal, HelloComponent);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.render_if_needed(); // establish baseline
        runtime.terminal.output.clear();

        let handle = runtime.runtime_handle();
        handle.dispatch(Command::Terminal(TerminalOp::ClearScreen));

        runtime.run_once();

        let output = runtime.terminal.output.as_str();
        let clear_idx = output
            .find("\x1b[2J\x1b[H")
            .expect("expected clear screen bytes (ESC[2J ESC[H)");
        let hello_idx = output
            .find("hello")
            .expect("expected frame content after clear");
        assert!(
            clear_idx < hello_idx,
            "expected clear screen bytes before frame content, got: {output:?}"
        );
        assert!(
            !output.contains("\x1b[3J"),
            "expected no scrollback clear (ESC[3J), got: {output:?}"
        );
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());
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
        let (runtime, _root_id) = runtime_with_root(terminal, DummyComponent::default());
        drop(runtime);

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

    #[cfg(feature = "unsafe-terminal-access")]
    #[test]
    fn unsafe_terminal_guard_resyncs_renderer_and_forces_full_repaint() {
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
        let (mut runtime, _root_id) = runtime_with_root(terminal, TwoLineComponent);
        runtime.show_hardware_cursor = false;

        runtime.start().expect("runtime start");
        runtime.terminal.output.clear();

        // Establish diff renderer baseline.
        runtime.render_now();
        runtime.terminal.output.clear();

        // Stable diff should not rewrite line content.
        runtime.render_now();
        let stable_output = runtime.terminal.output.clone();
        assert!(
            !stable_output.contains("line-a") && !stable_output.contains("line-b"),
            "expected stable diff to avoid line rewrites, got: {stable_output:?}"
        );
        runtime.terminal.output.clear();

        {
            let mut guard = runtime.terminal_guard_unsafe();
            guard.write_raw("\x1b[?25l");
        }

        runtime.terminal.output.clear();
        runtime.render_if_needed();

        let output = runtime.terminal.output.as_str();
        let line_a_idx = output
            .find("line-a")
            .expect("expected frame repaint after guard drop");
        let line_b_idx = output
            .find("line-b")
            .expect("expected frame repaint after guard drop");
        assert!(
            line_a_idx < line_b_idx,
            "expected line-a before line-b, got: {output:?}"
        );
        assert_eq!(
            output.matches("\x1b[2K").count(),
            2,
            "expected full redraw to clear each line, got: {output:?}"
        );
        assert!(
            !output.contains("\x1b[2J\x1b[H"),
            "expected no full screen clear (ESC[2J ESC[H), got: {output:?}"
        );
        assert!(
            !output.contains("\x1b[3J"),
            "expected no scrollback clear (ESC[3J), got: {output:?}"
        );
    }
}
