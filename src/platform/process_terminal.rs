//! Process-based terminal implementation (Phase 1).

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as u64
}

#[cfg(unix)]
fn write_fd(fd: c_int, data: &str) {
    let bytes = data.as_bytes();
    let mut written = 0;
    while written < bytes.len() {
        let result = unsafe {
            libc::write(
                fd,
                bytes[written..].as_ptr() as *const libc::c_void,
                bytes.len() - written,
            )
        };
        if result <= 0 {
            return;
        }
        written += result as usize;
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
fn get_termios(fd: c_int) -> libc::termios {
    let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
    let result = unsafe { libc::tcgetattr(fd, &mut termios) };
    if result != 0 {
        panic!("failed to read terminal attributes");
    }
    termios
}

#[cfg(unix)]
fn set_termios(fd: c_int, termios: &libc::termios) {
    let result = unsafe { libc::tcsetattr(fd, libc::TCSANOW, termios) };
    if result != 0 {
        panic!("failed to set terminal attributes");
    }
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
    resize_handler: Arc<Mutex<Option<Box<dyn FnMut() + Send>>>>,
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

    fn enable_raw_mode(&mut self) {
        if self.original_termios.is_none() {
            self.original_termios = Some(get_termios(self.stdin_fd));
        }
        let mut raw = self
            .original_termios
            .as_ref()
            .expect("original termios missing")
            .clone();
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        set_termios(self.stdin_fd, &raw);
    }

    fn restore_raw_mode(&mut self) {
        if let Some(original) = self.original_termios.as_ref() {
            set_termios(self.stdin_fd, original);
        }
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
    ) {
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

        self.enable_raw_mode();

        self.start_resize_thread();
        unsafe {
            libc::raise(libc::SIGWINCH);
        }

        self.start_input_thread();
    }

    fn stop(&mut self) {
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

        self.restore_raw_mode();
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
pub struct PanicHookGuard {
    previous: Arc<Box<dyn Fn(&std::panic::PanicHookInfo) + Send + Sync + 'static>>,
}

#[cfg(unix)]
impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        let previous = Arc::clone(&self.previous);
        std::panic::set_hook(Box::new(move |info| {
            (previous)(info);
        }));
    }
}

#[cfg(unix)]
fn run_cleanup_once<F>(cleanup: &Arc<F>, ran: &AtomicBool)
where
    F: Fn() + Send + Sync + 'static,
{
    if !ran.swap(true, Ordering::SeqCst) {
        cleanup();
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
    let cleanup = Arc::new(cleanup);
    let ran = Arc::new(AtomicBool::new(false));
    let previous = Arc::new(std::panic::take_hook());
    let previous_for_hook = Arc::clone(&previous);
    let cleanup_for_hook = Arc::clone(&cleanup);
    let ran_for_hook = Arc::clone(&ran);

    std::panic::set_hook(Box::new(move |info| {
        run_cleanup_once(&cleanup_for_hook, &ran_for_hook);
        (previous_for_hook)(info);
    }));

    PanicHookGuard { previous }
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
    ) {
        panic!("ProcessTerminal is only supported on Unix platforms");
    }

    fn stop(&mut self) {
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
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::{get_termios, poll_readable, ProcessTerminal, StopTestHooks};
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

    #[test]
    fn pty_start_stop_do_not_write_output() {
        let pty = open_pty();

        let mut terminal = ProcessTerminal::new();
        terminal.stdin_fd = pty.slave;
        terminal.stdout_fd = pty.slave;

        terminal.start(Box::new(|_| {}), Box::new(|| {}));
        let output = read_available(pty.master, Duration::from_millis(200));
        assert!(
            output.is_empty(),
            "expected start() to write no output, got: {:?}",
            String::from_utf8_lossy(&output)
        );

        terminal.stop();
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

        terminal.start(Box::new(|_| {}), Box::new(|| {}));

        let start = Instant::now();
        terminal.drain_input(200, 50);
        let elapsed = start.elapsed();
        assert!(
            elapsed <= Duration::from_millis(300),
            "drain_input exceeded max window: {elapsed:?}"
        );

        terminal.stop();
    }

    #[test]
    fn tcflush_runs_before_raw_mode_restore() {
        let pty = open_pty();
        let original = get_termios(pty.slave);

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

        terminal.start(Box::new(|_| {}), Box::new(|| {}));

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
            terminal.stop();
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

        let raw = get_termios(pty.slave);
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

        let restored = get_termios(pty.slave);
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

        terminal.start(
            Box::new(move |data| {
                let _ = tx.send(data);
            }),
            Box::new(|| {}),
        );

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

        terminal.stop();
    }
}
