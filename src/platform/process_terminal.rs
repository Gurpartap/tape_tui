//! Process-based terminal implementation (Phase 1).

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::core::terminal::Terminal;
use crate::platform::stdin_buffer::{StdinBuffer, StdinEvent};

#[cfg(unix)]
use libc::{self, c_int};
#[cfg(unix)]
use signal_hook::iterator::Signals;

#[derive(Default)]
struct InputState {
    handler: Option<Box<dyn FnMut(String) + Send>>,
}

#[cfg(unix)]
type ResizeHandlerFn = dyn FnMut() + Send;

#[cfg(unix)]
type ResizeHandler = Arc<Mutex<Option<Box<ResizeHandlerFn>>>>;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as u64
}

#[cfg(unix)]
fn wait_writable(fd: c_int) -> std::io::Result<()> {
    let mut fds = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut fds, 1, -1) };
        if result < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if result == 0 {
            // Infinite timeout should not return 0, but avoid a tight loop if it does.
            continue;
        }
        if (fds.revents & libc::POLLOUT) != 0 {
            return Ok(());
        }

        return Err(std::io::Error::other(format!(
            "poll(POLLOUT) returned revents=0x{:x}",
            fds.revents
        )));
    }
}

#[cfg(unix)]
fn write_all_fd_with<FWrite, FWait>(
    fd: c_int,
    bytes: &[u8],
    mut write_once: FWrite,
    mut wait_writable: FWait,
) -> std::io::Result<()>
where
    FWrite: FnMut(c_int, &[u8]) -> std::io::Result<usize>,
    FWait: FnMut(c_int) -> std::io::Result<()>,
{
    let mut written = 0;
    while written < bytes.len() {
        match write_once(fd, &bytes[written..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "write returned 0",
                ));
            }
            Ok(count) => {
                let remaining = bytes.len() - written;
                if count > remaining {
                    return Err(std::io::Error::other(
                        "write returned more bytes than requested",
                    ));
                }
                written += count;
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                continue;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                wait_writable(fd)?;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_fd(fd: c_int, data: &str) {
    if data.is_empty() {
        return;
    }

    let bytes = data.as_bytes();
    let result = write_all_fd_with(
        fd,
        bytes,
        |fd, buf| {
            let result = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
            if result < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(result as usize)
            }
        },
        wait_writable,
    );
    if let Err(err) = result {
        panic!("failed to write to terminal: {err}");
    }
}

#[cfg(unix)]
fn read_winsize(fd: c_int) -> Option<(u16, u16)> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let result = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) };
    if result == 0 && size.ws_col > 0 && size.ws_row > 0 {
        Some((size.ws_col, size.ws_row))
    } else {
        None
    }
}

#[cfg(unix)]
fn poll_readable(fd: c_int, timeout_ms: i32) -> bool {
    let mut fds = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut fds, 1, timeout_ms) };
    result > 0 && (fds.revents & libc::POLLIN) != 0
}

#[cfg(unix)]
fn get_termios(fd: c_int) -> std::io::Result<libc::termios> {
    let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
    let result = unsafe { libc::tcgetattr(fd, &mut termios) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(termios)
}

#[cfg(unix)]
fn set_termios(fd: c_int, termios: &libc::termios) -> std::io::Result<()> {
    let result = unsafe { libc::tcsetattr(fd, libc::TCSANOW, termios) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
struct StopTestHooks {
    before_flush_ready: std::sync::mpsc::Sender<()>,
    before_flush_go: std::sync::mpsc::Receiver<()>,
    after_flush_ready: std::sync::mpsc::Sender<()>,
    after_flush_go: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
impl StopTestHooks {
    fn before_flush(&self) {
        let _ = self.before_flush_ready.send(());
        let _ = self.before_flush_go.recv();
    }

    fn after_flush(&self) {
        let _ = self.after_flush_ready.send(());
        let _ = self.after_flush_go.recv();
    }
}

#[cfg(unix)]
pub struct ProcessTerminal {
    stdin_fd: c_int,
    stdout_fd: c_int,
    original_termios: Option<libc::termios>,
    input_state: Arc<Mutex<InputState>>,
    resize_handler: ResizeHandler,
    input_thread: Option<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    drain_mode: Arc<AtomicBool>,
    last_input_time: Arc<AtomicU64>,
    write_log_path: Option<PathBuf>,
    write_log_failed: bool,
    resize_signal_handle: Option<signal_hook::iterator::Handle>,
    resize_thread: Option<JoinHandle<()>>,
    #[cfg(test)]
    stop_test_hooks: Option<StopTestHooks>,
}

#[cfg(unix)]
impl ProcessTerminal {
    pub fn new() -> Self {
        let write_log_path = match env::var_os("PI_TUI_WRITE_LOG") {
            Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
            _ => None,
        };

        Self {
            stdin_fd: libc::STDIN_FILENO,
            stdout_fd: libc::STDOUT_FILENO,
            original_termios: None,
            input_state: Arc::new(Mutex::new(InputState::default())),
            resize_handler: Arc::new(Mutex::new(None)),
            input_thread: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
            drain_mode: Arc::new(AtomicBool::new(false)),
            last_input_time: Arc::new(AtomicU64::new(now_ms())),
            write_log_path,
            write_log_failed: false,
            resize_signal_handle: None,
            resize_thread: None,
            #[cfg(test)]
            stop_test_hooks: None,
        }
    }

    fn enable_raw_mode(&mut self) -> std::io::Result<()> {
        if self.original_termios.is_none() {
            self.original_termios = Some(get_termios(self.stdin_fd)?);
        }
        let mut raw = *self
            .original_termios
            .as_ref()
            .expect("original termios missing");
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        set_termios(self.stdin_fd, &raw)
    }

    fn restore_raw_mode(&mut self) -> std::io::Result<()> {
        if let Some(original) = self.original_termios.as_ref() {
            set_termios(self.stdin_fd, original)?;
        }
        Ok(())
    }

    fn start_input_thread(&mut self) {
        let stdin_fd = self.stdin_fd;
        let input_state = Arc::clone(&self.input_state);
        let stop_flag = Arc::clone(&self.stop_flag);
        let drain_mode = Arc::clone(&self.drain_mode);
        let last_input_time = Arc::clone(&self.last_input_time);

        self.input_thread = Some(thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            let mut stdin_buffer = StdinBuffer::new(10);

            while !stop_flag.load(Ordering::SeqCst) {
                let now = Instant::now();
                let timeout_ms = stdin_buffer.next_timeout_ms(now, 50);
                let readable = poll_readable(stdin_fd, timeout_ms);
                let events = if readable {
                    let read_len = unsafe {
                        libc::read(stdin_fd, buffer.as_mut_ptr() as *mut _, buffer.len())
                    };
                    if read_len <= 0 {
                        Vec::new()
                    } else {
                        last_input_time.store(now_ms(), Ordering::SeqCst);
                        stdin_buffer.process(&buffer[..read_len as usize])
                    }
                } else {
                    stdin_buffer.flush_due(now)
                };

                if events.is_empty() {
                    continue;
                }

                for event in events {
                    match event {
                        StdinEvent::Data(sequence) => {
                            if drain_mode.load(Ordering::SeqCst) {
                                continue;
                            }

                            let mut state =
                                input_state.lock().expect("input handler lock poisoned");
                            if let Some(handler) = state.handler.as_mut() {
                                handler(sequence);
                            }
                        }
                        StdinEvent::Paste(content) => {
                            if drain_mode.load(Ordering::SeqCst) {
                                continue;
                            }
                            let mut state =
                                input_state.lock().expect("input handler lock poisoned");
                            if let Some(handler) = state.handler.as_mut() {
                                handler(format!("\x1b[200~{}\x1b[201~", content));
                            }
                        }
                    }
                }
            }
        }));
    }

    fn stop_input_thread(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.input_thread.take() {
            let _ = handle.join();
        }
    }

    fn start_resize_thread(&mut self) {
        let mut signals = Signals::new([libc::SIGWINCH]).expect("failed to register SIGWINCH");
        let handle = signals.handle();
        let resize_handler = Arc::clone(&self.resize_handler);

        let thread = thread::spawn(move || {
            for _ in signals.forever() {
                let mut handler = resize_handler.lock().expect("resize handler lock poisoned");
                if let Some(handler) = handler.as_mut() {
                    handler();
                }
            }
        });

        self.resize_signal_handle = Some(handle);
        self.resize_thread = Some(thread);
    }

    fn stop_resize_thread(&mut self) {
        if let Some(handle) = self.resize_signal_handle.take() {
            handle.close();
        }
        if let Some(thread) = self.resize_thread.take() {
            let _ = thread.join();
        }
    }

    fn write_control(&self, data: &str) {
        write_fd(self.stdout_fd, data);
    }
}

#[cfg(unix)]
impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl Terminal for ProcessTerminal {
    fn start(
        &mut self,
        on_input: Box<dyn FnMut(String) + Send>,
        on_resize: Box<dyn FnMut() + Send>,
    ) -> std::io::Result<()> {
        {
            let mut state = self
                .input_state
                .lock()
                .expect("input handler lock poisoned");
            state.handler = Some(on_input);
        }
        {
            let mut handler = self
                .resize_handler
                .lock()
                .expect("resize handler lock poisoned");
            *handler = Some(on_resize);
        }

        self.stop_flag.store(false, Ordering::SeqCst);
        self.drain_mode.store(false, Ordering::SeqCst);
        self.last_input_time.store(now_ms(), Ordering::SeqCst);

        if let Err(err) = self.enable_raw_mode() {
            {
                let mut state = self
                    .input_state
                    .lock()
                    .expect("input handler lock poisoned");
                state.handler = None;
            }
            {
                let mut handler = self
                    .resize_handler
                    .lock()
                    .expect("resize handler lock poisoned");
                *handler = None;
            }
            return Err(err);
        }

        self.start_resize_thread();
        unsafe {
            libc::raise(libc::SIGWINCH);
        }

        self.start_input_thread();

        Ok(())
    }

    fn stop(&mut self) -> std::io::Result<()> {
        self.stop_input_thread();
        self.stop_resize_thread();

        {
            let mut state = self
                .input_state
                .lock()
                .expect("input handler lock poisoned");
            state.handler = None;
        }
        {
            let mut handler = self
                .resize_handler
                .lock()
                .expect("resize handler lock poisoned");
            *handler = None;
        }

        #[cfg(test)]
        if let Some(hooks) = self.stop_test_hooks.as_ref() {
            hooks.before_flush();
        }

        // Flush input before leaving raw mode to avoid buffered bytes leaking to the shell.
        let _ = unsafe { libc::tcflush(self.stdin_fd, libc::TCIFLUSH) };

        #[cfg(test)]
        if let Some(hooks) = self.stop_test_hooks.as_ref() {
            hooks.after_flush();
        }

        self.restore_raw_mode()
    }

    fn drain_input(&mut self, max_ms: u64, idle_ms: u64) {
        self.drain_mode.store(true, Ordering::SeqCst);
        self.last_input_time.store(now_ms(), Ordering::SeqCst);

        let end_time = now_ms().saturating_add(max_ms);
        loop {
            let now = now_ms();
            if now >= end_time {
                break;
            }
            let last_input = self.last_input_time.load(Ordering::SeqCst);
            if now.saturating_sub(last_input) >= idle_ms {
                break;
            }

            let remaining = end_time.saturating_sub(now);
            let sleep_for = idle_ms.min(remaining).max(1);
            thread::sleep(Duration::from_millis(sleep_for));
        }

        self.drain_mode.store(false, Ordering::SeqCst);
    }

    fn write(&mut self, data: &str) {
        self.write_control(data);
        if self.write_log_failed {
            return;
        }
        if let Some(path) = self.write_log_path.as_ref() {
            let result = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| file.write_all(data.as_bytes()));
            if result.is_err() {
                self.write_log_failed = true;
            }
        }
    }

    fn columns(&self) -> u16 {
        read_winsize(self.stdout_fd)
            .map(|(cols, _)| cols)
            .unwrap_or(80)
    }

    fn rows(&self) -> u16 {
        read_winsize(self.stdout_fd)
            .map(|(_, rows)| rows)
            .unwrap_or(24)
    }
}

/// Signal handler guard for cleanup hooks.
#[cfg(unix)]
pub struct SignalHookGuard {
    handle: signal_hook::iterator::Handle,
    thread: Option<JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for SignalHookGuard {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Panic hook guard for cleanup hooks.
#[cfg(unix)]
type PanicHookFn = dyn Fn(&std::panic::PanicHookInfo) + Send + Sync + 'static;

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PanicHookId {
    data: usize,
    vtable: usize,
}

#[cfg(unix)]
fn panic_hook_id(hook: &PanicHookFn) -> PanicHookId {
    // Compare fat pointers (data + vtable) to detect whether the currently
    // installed hook is the one this guard installed.
    let raw = hook as *const PanicHookFn;
    let (data, vtable): (*const (), *const ()) = unsafe { std::mem::transmute(raw) };
    PanicHookId {
        data: data as usize,
        vtable: vtable as usize,
    }
}

#[cfg(unix)]
struct PanicCleanupNode {
    cleanup: Arc<dyn Fn() + Send + Sync + 'static>,
    ran: AtomicBool,
    active: AtomicBool,
    next: AtomicPtr<PanicCleanupNode>,
}

#[cfg(unix)]
impl PanicCleanupNode {
    fn new(cleanup: Arc<dyn Fn() + Send + Sync + 'static>) -> Self {
        Self {
            cleanup,
            ran: AtomicBool::new(false),
            active: AtomicBool::new(true),
            next: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

#[cfg(unix)]
fn run_cleanup_once<F>(cleanup: &Arc<F>, ran: &AtomicBool)
where
    F: Fn() + Send + Sync + 'static + ?Sized,
{
    if !ran.swap(true, Ordering::SeqCst) {
        cleanup();
    }
}

/// Global registry of cleanup nodes for the process panic hook.
///
/// Nodes are intentionally leaked; guards mark nodes inactive on drop.
#[cfg(unix)]
static PANIC_CLEANUP_HEAD: AtomicPtr<PanicCleanupNode> = AtomicPtr::new(std::ptr::null_mut());

#[cfg(unix)]
static PANIC_HOOK_ACTIVE_GUARDS: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
#[derive(Default)]
struct PanicHookWrapperState {
    installed: Option<PanicHookId>,
    previous: Option<Arc<Box<PanicHookFn>>>,
}

#[cfg(unix)]
static PANIC_HOOK_WRAPPER_STATE: Mutex<PanicHookWrapperState> = Mutex::new(PanicHookWrapperState {
    installed: None,
    previous: None,
});

#[cfg(unix)]
fn register_panic_cleanup(cleanup: Arc<dyn Fn() + Send + Sync + 'static>) -> *mut PanicCleanupNode {
    let node = Box::new(PanicCleanupNode::new(cleanup));
    let node_ptr = Box::into_raw(node);

    loop {
        let head = PANIC_CLEANUP_HEAD.load(Ordering::Acquire);
        // SAFETY: `node_ptr` is a fresh allocation; no other thread can observe it
        // until we publish it by swapping PANIC_CLEANUP_HEAD.
        unsafe {
            (*node_ptr).next.store(head, Ordering::Relaxed);
        }

        if PANIC_CLEANUP_HEAD
            .compare_exchange(head, node_ptr, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            break;
        }
    }

    node_ptr
}

#[cfg(unix)]
fn run_all_panic_cleanups() {
    let mut node_ptr = PANIC_CLEANUP_HEAD.load(Ordering::Acquire);
    while !node_ptr.is_null() {
        // SAFETY: nodes are leaked for the program lifetime.
        let node = unsafe { &*node_ptr };
        if node.active.load(Ordering::Acquire) {
            run_cleanup_once(&node.cleanup, &node.ran);
        }
        node_ptr = node.next.load(Ordering::Acquire);
    }
}

#[cfg(unix)]
fn sync_panic_hook_state() {
    let mut state = match PANIC_HOOK_WRAPPER_STATE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };

    // Converge to the desired state (wrapper installed iff there is at least one guard).
    loop {
        let want_wrapper = PANIC_HOOK_ACTIVE_GUARDS.load(Ordering::SeqCst) != 0;

        let current = std::panic::take_hook();
        let current_id = panic_hook_id(current.as_ref());

        match (want_wrapper, state.installed) {
            (true, Some(installed)) if current_id == installed => {
                // Wrapper already installed and still current.
                std::panic::set_hook(current);
            }
            (true, _) => {
                // Wrapper missing or replaced: (re)install it around the current hook.
                state.installed = None;
                state.previous = None;

                let previous = Arc::new(current);
                let previous_for_hook = Arc::clone(&previous);
                let hook: Box<PanicHookFn> = Box::new(move |info| {
                    run_all_panic_cleanups();
                    (previous_for_hook)(info);
                });
                let installed = panic_hook_id(hook.as_ref());
                std::panic::set_hook(hook);

                state.installed = Some(installed);
                state.previous = Some(previous);
            }
            (false, Some(installed)) if current_id == installed => {
                // Uninstall: restore the hook that was active before we installed the wrapper.
                drop(current);

                let previous = state.previous.take();
                state.installed = None;

                let Some(previous) = previous else {
                    // `take_hook()` installed the default hook; keep it.
                    break;
                };

                match Arc::try_unwrap(previous) {
                    Ok(previous) => std::panic::set_hook(previous),
                    Err(previous) => std::panic::set_hook(Box::new(move |info| {
                        (previous)(info);
                    })),
                }
            }
            (false, Some(_)) => {
                // Another part of the program installed a newer hook after ours.
                // Do not clobber it when removing the last guard.
                std::panic::set_hook(current);
                state.installed = None;
                state.previous = None;
            }
            (false, None) => {
                std::panic::set_hook(current);
            }
        }

        let want_wrapper_now = PANIC_HOOK_ACTIVE_GUARDS.load(Ordering::SeqCst) != 0;
        let wrapper_installed_now = state.installed.is_some();
        if want_wrapper_now == wrapper_installed_now {
            break;
        }
    }
}

#[cfg(unix)]
pub struct PanicHookGuard {
    node: *mut PanicCleanupNode,
}

#[cfg(unix)]
impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        // SAFETY: nodes are leaked and remain valid for the program lifetime.
        unsafe {
            (*self.node).active.store(false, Ordering::Release);
        }

        let previous = PANIC_HOOK_ACTIVE_GUARDS.fetch_sub(1, Ordering::SeqCst);
        if previous == 1 {
            sync_panic_hook_state();
        }
    }
}

/// Install SIGINT/SIGTERM cleanup hook.
#[cfg(unix)]
pub fn install_signal_handlers<F>(cleanup: F) -> SignalHookGuard
where
    F: Fn() + Send + Sync + 'static,
{
    let cleanup = Arc::new(cleanup);
    let ran = Arc::new(AtomicBool::new(false));
    let mut signals =
        Signals::new([libc::SIGINT, libc::SIGTERM]).expect("failed to register signal handlers");
    let handle = signals.handle();
    let cleanup_clone = Arc::clone(&cleanup);
    let ran_clone = Arc::clone(&ran);

    let thread = thread::spawn(move || {
        for _ in signals.forever() {
            run_cleanup_once(&cleanup_clone, &ran_clone);
        }
    });

    SignalHookGuard {
        handle,
        thread: Some(thread),
    }
}

/// Install panic hook that runs cleanup once, then delegates to the previous hook.
#[cfg(unix)]
pub fn install_panic_hook<F>(cleanup: F) -> PanicHookGuard
where
    F: Fn() + Send + Sync + 'static,
{
    let node = register_panic_cleanup(Arc::new(cleanup));

    let previous = PANIC_HOOK_ACTIVE_GUARDS.fetch_add(1, Ordering::SeqCst);
    if previous == 0 {
        sync_panic_hook_state();
    }

    PanicHookGuard { node }
}

/// Minimal terminal writer for panic/signal cleanup.
///
/// This is intentionally best-effort:
/// - never panics
/// - never blocks indefinitely
/// - does not touch termios / raw mode
#[cfg(unix)]
pub(crate) struct HookTerminal {
    fd: c_int,
    owns_fd: bool,
}

#[cfg(unix)]
impl HookTerminal {
    pub(crate) fn new() -> Self {
        // Prefer the controlling TTY (works even if stdout is redirected).
        // Open in non-blocking mode so crash cleanup can never hang.
        let flags = libc::O_WRONLY | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC;
        let fd = unsafe { libc::open(c"/dev/tty".as_ptr(), flags) };
        if fd >= 0 {
            Self { fd, owns_fd: true }
        } else {
            // No controlling TTY (or not accessible). Disable output rather than
            // risking a blocking write to stdout/stderr (which may be a full pipe).
            Self {
                fd: -1,
                owns_fd: false,
            }
        }
    }

    fn write_best_effort(&self, data: &str) {
        if self.fd < 0 || data.is_empty() {
            return;
        }

        let bytes = data.as_bytes();
        let mut written = 0;
        while written < bytes.len() {
            let remaining = &bytes[written..];
            let result = unsafe {
                libc::write(
                    self.fd,
                    remaining.as_ptr() as *const libc::c_void,
                    remaining.len(),
                )
            };
            if result > 0 {
                written = written.saturating_add(result as usize);
                continue;
            }
            if result == 0 {
                break;
            }

            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }

            // Best-effort crash cleanup: do not block or spin forever.
            // - WouldBlock/EAGAIN: drop remaining output.
            // - Any other error: drop remaining output.
            break;
        }
    }
}

#[cfg(unix)]
impl Default for HookTerminal {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl Drop for HookTerminal {
    fn drop(&mut self) {
        if self.owns_fd {
            unsafe {
                libc::close(self.fd);
            }
        }
    }
}

#[cfg(unix)]
impl Terminal for HookTerminal {
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
        self.write_best_effort(data);
    }

    fn columns(&self) -> u16 {
        80
    }

    fn rows(&self) -> u16 {
        24
    }
}

#[cfg(not(unix))]
pub struct ProcessTerminal;

#[cfg(not(unix))]
impl ProcessTerminal {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(unix))]
impl Terminal for ProcessTerminal {
    fn start(
        &mut self,
        _on_input: Box<dyn FnMut(String) + Send>,
        _on_resize: Box<dyn FnMut() + Send>,
    ) -> std::io::Result<()> {
        panic!("ProcessTerminal is only supported on Unix platforms");
    }

    fn stop(&mut self) -> std::io::Result<()> {
        panic!("ProcessTerminal is only supported on Unix platforms");
    }

    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {
        panic!("ProcessTerminal is only supported on Unix platforms");
    }

    fn write(&mut self, _data: &str) {
        panic!("ProcessTerminal is only supported on Unix platforms");
    }

    fn columns(&self) -> u16 {
        80
    }

    fn rows(&self) -> u16 {
        24
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    };
    use std::time::{Duration, Instant};

    use super::{
        get_termios, install_panic_hook, poll_readable, write_all_fd_with, HookTerminal,
        ProcessTerminal, StopTestHooks,
    };
    use crate::core::terminal::Terminal;

    #[cfg(unix)]
    use libc::{self, c_int};

    struct Pty {
        master: c_int,
        slave: c_int,
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.master);
                libc::close(self.slave);
            }
        }
    }

    fn open_pty() -> Pty {
        let mut master: c_int = 0;
        let mut slave: c_int = 0;
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, 0, "openpty failed");
        Pty { master, slave }
    }

    fn set_nonblocking(fd: c_int, enabled: bool) {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0, "fcntl(F_GETFL) failed");
        let new_flags = if enabled {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        let result = unsafe { libc::fcntl(fd, libc::F_SETFL, new_flags) };
        assert!(result >= 0, "fcntl(F_SETFL) failed");
    }

    fn read_available(fd: c_int, timeout: Duration) -> Vec<u8> {
        let end = Instant::now() + timeout;
        let mut out = Vec::new();
        while Instant::now() < end {
            let now = Instant::now();
            let remaining = end.saturating_duration_since(now);
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            if timeout_ms == 0 || !poll_readable(fd, timeout_ms) {
                break;
            }
            let mut buf = [0u8; 1024];
            let read_len = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if read_len <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..read_len as usize]);
        }
        out
    }

    fn read_nonblocking(fd: c_int) -> io::Result<Vec<u8>> {
        set_nonblocking(fd, true);
        let mut buf = [0u8; 64];
        let read_len = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
        let result = if read_len < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(buf[..read_len as usize].to_vec())
        };
        set_nonblocking(fd, false);
        result
    }

    fn panic_hook_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn panic_hook_guard_drop_does_not_clobber_later_hooks() {
        let _guard = panic_hook_test_lock()
            .lock()
            .expect("panic hook test lock poisoned");

        let original = std::panic::take_hook();
        // Keep this test quiet: install a no-op base hook so the default hook doesn't
        // print to stderr when we trigger a panic.
        std::panic::set_hook(Box::new(|_| {}));

        struct RestoreOriginal {
            hook: Option<Box<super::PanicHookFn>>,
        }

        impl Drop for RestoreOriginal {
            fn drop(&mut self) {
                if let Some(hook) = self.hook.take() {
                    std::panic::set_hook(hook);
                }
            }
        }

        let _restore = RestoreOriginal {
            hook: Some(original),
        };

        let cleanup_a = Arc::new(AtomicUsize::new(0));
        let cleanup_b = Arc::new(AtomicUsize::new(0));

        let guard_a = install_panic_hook({
            let cleanup_a = Arc::clone(&cleanup_a);
            move || {
                cleanup_a.fetch_add(1, Ordering::SeqCst);
            }
        });

        let guard_b = install_panic_hook({
            let cleanup_b = Arc::clone(&cleanup_b);
            move || {
                cleanup_b.fetch_add(1, Ordering::SeqCst);
            }
        });

        // Dropping the older guard must not clobber the newer hook (guard_b).
        drop(guard_a);

        let _ = std::panic::catch_unwind(|| {
            panic!("boom");
        });

        assert_eq!(
            cleanup_b.load(Ordering::SeqCst),
            1,
            "expected newer hook cleanup to run"
        );

        drop(guard_b);
    }

    #[test]
    fn panic_hook_guards_restore_base_hook_when_dropped_out_of_order() {
        let _guard = panic_hook_test_lock()
            .lock()
            .expect("panic hook test lock poisoned");

        let original = std::panic::take_hook();

        struct RestoreOriginal {
            hook: Option<Box<super::PanicHookFn>>,
        }

        impl Drop for RestoreOriginal {
            fn drop(&mut self) {
                if let Some(hook) = self.hook.take() {
                    std::panic::set_hook(hook);
                }
            }
        }

        let _restore = RestoreOriginal {
            hook: Some(original),
        };

        fn base_hook(_: &std::panic::PanicHookInfo) {}

        let base_hook: Box<super::PanicHookFn> = Box::new(base_hook);
        let base_hook_id = super::panic_hook_id(base_hook.as_ref());
        std::panic::set_hook(base_hook);

        let guard_a = install_panic_hook(|| {});
        let guard_b = install_panic_hook(|| {});

        // Drop guards out of LIFO order. When all guards are gone, the base hook must be restored.
        drop(guard_a);
        drop(guard_b);

        let current = std::panic::take_hook();
        let current_id = super::panic_hook_id(current.as_ref());
        std::panic::set_hook(current);

        assert_eq!(current_id, base_hook_id, "base hook not restored");
    }

    #[test]
    fn hook_terminal_write_best_effort_returns_on_would_block() {
        let mut fds = [0 as c_int; 2];
        let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(result, 0, "pipe failed");

        let read_fd = fds[0];
        let write_fd = fds[1];

        // Make the write end non-blocking and fill the pipe until it would block.
        set_nonblocking(write_fd, true);

        let buf = [b'x'; 4096];
        loop {
            let written =
                unsafe { libc::write(write_fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
            if written > 0 {
                continue;
            }
            if written == 0 {
                break;
            }

            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == io::ErrorKind::WouldBlock {
                break;
            }
            panic!("unexpected error filling pipe: {err:?}");
        }

        let terminal = HookTerminal {
            fd: write_fd,
            owns_fd: false,
        };
        terminal.write_best_effort("cleanup");

        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

    #[test]
    fn pty_start_stop_do_not_write_output() {
        let pty = open_pty();

        let mut terminal = ProcessTerminal::new();
        terminal.stdin_fd = pty.slave;
        terminal.stdout_fd = pty.slave;

        terminal
            .start(Box::new(|_| {}), Box::new(|| {}))
            .expect("terminal start");
        let output = read_available(pty.master, Duration::from_millis(200));
        assert!(
            output.is_empty(),
            "expected start() to write no output, got: {:?}",
            String::from_utf8_lossy(&output)
        );

        terminal.stop().expect("terminal stop");
        let output = read_available(pty.master, Duration::from_millis(200));
        assert!(
            output.is_empty(),
            "expected stop() to write no output, got: {:?}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn drain_input_returns_within_limits() {
        let pty = open_pty();

        let mut terminal = ProcessTerminal::new();
        terminal.stdin_fd = pty.slave;
        terminal.stdout_fd = pty.slave;

        terminal
            .start(Box::new(|_| {}), Box::new(|| {}))
            .expect("terminal start");

        let start = Instant::now();
        terminal.drain_input(200, 50);
        let elapsed = start.elapsed();
        assert!(
            elapsed <= Duration::from_millis(300),
            "drain_input exceeded max window: {elapsed:?}"
        );

        terminal.stop().expect("terminal stop");
    }

    #[test]
    fn tcflush_runs_before_raw_mode_restore() {
        let pty = open_pty();
        let original = get_termios(pty.slave).expect("get termios");

        let (before_ready_tx, before_ready_rx) = mpsc::channel();
        let (before_go_tx, before_go_rx) = mpsc::channel();
        let (after_ready_tx, after_ready_rx) = mpsc::channel();
        let (after_go_tx, after_go_rx) = mpsc::channel();

        let mut terminal = ProcessTerminal::new();
        terminal.stdin_fd = pty.slave;
        terminal.stdout_fd = pty.slave;
        terminal.stop_test_hooks = Some(StopTestHooks {
            before_flush_ready: before_ready_tx,
            before_flush_go: before_go_rx,
            after_flush_ready: after_ready_tx,
            after_flush_go: after_go_rx,
        });

        terminal
            .start(Box::new(|_| {}), Box::new(|| {}))
            .expect("terminal start");

        struct StopRelease {
            before_go: Option<mpsc::Sender<()>>,
            after_go: Option<mpsc::Sender<()>>,
        }

        impl Drop for StopRelease {
            fn drop(&mut self) {
                if let Some(tx) = self.before_go.take() {
                    let _ = tx.send(());
                }
                if let Some(tx) = self.after_go.take() {
                    let _ = tx.send(());
                }
            }
        }

        let mut stop_release = StopRelease {
            before_go: Some(before_go_tx),
            after_go: Some(after_go_tx),
        };

        let stop_thread = std::thread::spawn(move || {
            terminal.stop().expect("terminal stop");
        });

        before_ready_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("stop did not reach before_flush hook");

        let _ = unsafe { libc::write(pty.master, b"abc".as_ptr() as *const libc::c_void, 3) };

        stop_release
            .before_go
            .take()
            .expect("before_go already used")
            .send(())
            .expect("before_go send failed");

        after_ready_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("stop did not reach after_flush hook");

        let raw = get_termios(pty.slave).expect("get termios");
        assert_eq!(raw.c_lflag & libc::ICANON, 0, "raw mode restored too early");

        match read_nonblocking(pty.slave) {
            Ok(data) => assert!(data.is_empty(), "input not flushed: {data:?}"),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => panic!("unexpected read error: {err:?}"),
        }

        stop_release
            .after_go
            .take()
            .expect("after_go already used")
            .send(())
            .expect("after_go send failed");

        stop_thread.join().expect("stop thread panicked");

        let restored = get_termios(pty.slave).expect("get termios");
        assert_eq!(
            restored.c_lflag & libc::ICANON,
            original.c_lflag & libc::ICANON,
            "raw mode not restored"
        );
    }

    #[test]
    fn bracketed_paste_is_rewrapped_for_input_handler() {
        let pty = open_pty();

        let (tx, rx) = mpsc::channel();
        let mut terminal = ProcessTerminal::new();
        terminal.stdin_fd = pty.slave;
        terminal.stdout_fd = pty.slave;

        terminal
            .start(
                Box::new(move |data| {
                    let _ = tx.send(data);
                }),
                Box::new(|| {}),
            )
            .expect("terminal start");

        let payload = b"\x1b[200~hello\x1b[201~";
        let _ = unsafe {
            libc::write(
                pty.master,
                payload.as_ptr() as *const libc::c_void,
                payload.len(),
            )
        };

        let received = rx
            .recv_timeout(Duration::from_millis(200))
            .expect("missing paste event");
        assert_eq!(received, "\x1b[200~hello\x1b[201~");

        terminal.stop().expect("terminal stop");
    }

    #[test]
    fn start_returns_err_on_tcgetattr_failure() {
        let mut terminal = ProcessTerminal::new();
        terminal.stdin_fd = -1;
        terminal.stdout_fd = -1;

        let result = terminal.start(Box::new(|_| {}), Box::new(|| {}));
        let err = result.expect_err("expected start to fail");
        assert_eq!(
            err.raw_os_error(),
            Some(libc::EBADF),
            "expected EBADF, got: {err:?}"
        );
    }

    #[test]
    fn write_all_fd_with_retries_on_eintr_and_writes_all_bytes() {
        let data = b"hello";
        let mut out = Vec::new();
        let mut calls = 0;
        write_all_fd_with(
            1,
            data,
            |_, buf| {
                calls += 1;
                match calls {
                    1 => Err(io::Error::from(io::ErrorKind::Interrupted)),
                    2 => {
                        out.extend_from_slice(&buf[..2]);
                        Ok(2)
                    }
                    _ => {
                        out.extend_from_slice(buf);
                        Ok(buf.len())
                    }
                }
            },
            |_| unreachable!("wait_writable should not be called for EINTR"),
        )
        .expect("write_all_fd_with failed");

        assert_eq!(out, data);
    }

    #[test]
    fn write_all_fd_with_handles_partial_writes() {
        let data = b"abcdefg";
        let mut out = Vec::new();
        let mut calls = 0;
        write_all_fd_with(
            1,
            data,
            |_, buf| {
                calls += 1;
                let count = buf.len().min(2);
                out.extend_from_slice(&buf[..count]);
                Ok(count)
            },
            |_| unreachable!("wait_writable should not be called for partial writes"),
        )
        .expect("write_all_fd_with failed");

        assert_eq!(out, data);
        assert!(calls > 1, "expected multiple writes, got {calls}");
    }

    #[test]
    fn write_all_fd_with_waits_for_writable_on_would_block_and_retries() {
        let data = b"xyz";
        let mut out = Vec::new();
        let mut calls = 0;
        let events = std::cell::RefCell::new(Vec::new());
        write_all_fd_with(
            1,
            data,
            |_, buf| {
                events.borrow_mut().push("write");
                calls += 1;
                if calls == 1 {
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                out.extend_from_slice(buf);
                Ok(buf.len())
            },
            |_| {
                events.borrow_mut().push("wait");
                Ok(())
            },
        )
        .expect("write_all_fd_with failed");

        assert_eq!(out, data);
        assert_eq!(events.into_inner(), vec!["write", "wait", "write"]);
    }
}
