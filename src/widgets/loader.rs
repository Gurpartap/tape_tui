//! Loader widget (Phase 23).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::core::component::Component;
use crate::runtime::tui::RenderHandle;
use crate::widgets::text::Text;

type RenderRequester = Arc<dyn Fn() + Send + Sync>;

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SPINNER_INTERVAL_MS: u64 = 80;

trait Sleeper: Send + Sync {
    fn sleep(&self, duration: Duration);
    fn wake(&self);
}

#[derive(Debug, Default)]
struct SleepState {
    wake_tokens: usize,
}

/// Default, wall-clock sleeper used in production.
///
/// This is intentionally wakeable so `Loader::stop()` can unblock a currently
/// blocked sleep without waiting for the next tick.
#[derive(Debug, Default)]
struct RealSleeper {
    state: Mutex<SleepState>,
    cvar: Condvar,
}

impl Sleeper for RealSleeper {
    fn sleep(&self, duration: Duration) {
        let state = self.state.lock().expect("real sleeper state poisoned");
        let (mut state, _) = self
            .cvar
            .wait_timeout_while(state, duration, |state| state.wake_tokens == 0)
            .expect("real sleeper state poisoned");

        if state.wake_tokens > 0 {
            state.wake_tokens -= 1;
        }
    }

    fn wake(&self) {
        let mut state = self.state.lock().expect("real sleeper state poisoned");
        state.wake_tokens = state.wake_tokens.saturating_add(1);
        self.cvar.notify_all();
    }
}

pub struct Loader {
    spinner_color_fn: Box<dyn Fn(&str) -> String>,
    message_color_fn: Box<dyn Fn(&str) -> String>,
    message: String,
    text: Text,
    render_requester: Option<RenderRequester>,
    current_frame: Arc<AtomicUsize>,
    stop_flag: Arc<AtomicBool>,
    sleeper: Arc<dyn Sleeper>,
    thread: Option<JoinHandle<()>>,
}

impl Loader {
    pub fn new(
        render_handle: RenderHandle,
        spinner_color_fn: Box<dyn Fn(&str) -> String>,
        message_color_fn: Box<dyn Fn(&str) -> String>,
        message: Option<String>,
    ) -> Self {
        let requester = Arc::new(move || {
            render_handle.request_render();
        });
        Self::with_requester(Some(requester), spinner_color_fn, message_color_fn, message)
    }

    pub(crate) fn with_requester(
        render_requester: Option<RenderRequester>,
        spinner_color_fn: Box<dyn Fn(&str) -> String>,
        message_color_fn: Box<dyn Fn(&str) -> String>,
        message: Option<String>,
    ) -> Self {
        Self::with_requester_and_sleeper(
            render_requester,
            Arc::new(RealSleeper::default()),
            spinner_color_fn,
            message_color_fn,
            message,
        )
    }

    fn with_requester_and_sleeper(
        render_requester: Option<RenderRequester>,
        sleeper: Arc<dyn Sleeper>,
        spinner_color_fn: Box<dyn Fn(&str) -> String>,
        message_color_fn: Box<dyn Fn(&str) -> String>,
        message: Option<String>,
    ) -> Self {
        let mut loader = Self {
            spinner_color_fn,
            message_color_fn,
            message: message.unwrap_or_else(|| "Loading...".to_string()),
            text: Text::with_padding("", 1, 0),
            render_requester,
            current_frame: Arc::new(AtomicUsize::new(0)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            sleeper,
            thread: None,
        };
        loader.start();
        loader
    }

    pub fn start(&mut self) {
        if self.thread.is_some() {
            return;
        }

        self.stop_flag.store(false, Ordering::SeqCst);
        self.current_frame.store(0, Ordering::SeqCst);
        self.update_text();
        self.request_render();

        let stop_flag = Arc::clone(&self.stop_flag);
        let current_frame = Arc::clone(&self.current_frame);
        let render_requester = self.render_requester.clone();
        let sleeper = Arc::clone(&self.sleeper);

        self.thread = Some(thread::spawn(move || loop {
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }

            sleeper.sleep(Duration::from_millis(SPINNER_INTERVAL_MS));

            if stop_flag.load(Ordering::SeqCst) {
                break;
            }

            current_frame.fetch_add(1, Ordering::SeqCst);
            if let Some(request) = render_requester.as_ref() {
                request();
            }
        }));
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            self.sleeper.wake();
            if handle.thread().id() == thread::current().id() {
                return;
            }
            let _ = handle.join();
        }
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
        self.update_text();
        self.request_render();
    }

    fn update_text(&mut self) {
        let idx = self.current_frame.load(Ordering::SeqCst) % SPINNER_FRAMES.len();
        let frame = SPINNER_FRAMES[idx];
        let spinner = (self.spinner_color_fn)(frame);
        let message = (self.message_color_fn)(&self.message);
        self.text.set_text(format!("{spinner} {message}"));
    }

    fn request_render(&self) {
        if let Some(requester) = self.render_requester.as_ref() {
            requester();
        }
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Component for Loader {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.update_text();
        let mut lines = Vec::new();
        lines.push(String::new());
        lines.extend(self.text.render(width));
        lines
    }

    fn invalidate(&mut self) {
        self.text.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::{Loader, Sleeper};
    use crate::core::component::Component;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct TestSleepState {
        wake_tokens: usize,
    }

    #[derive(Debug, Default)]
    struct TestSleeper {
        state: Mutex<TestSleepState>,
        cvar: Condvar,
    }

    impl Sleeper for TestSleeper {
        fn sleep(&self, _duration: Duration) {
            let mut state = self.state.lock().expect("test sleeper state poisoned");
            while state.wake_tokens == 0 {
                state = self.cvar.wait(state).expect("test sleeper state poisoned");
            }
            state.wake_tokens -= 1;
        }

        fn wake(&self) {
            let mut state = self.state.lock().expect("test sleeper state poisoned");
            state.wake_tokens = state.wake_tokens.saturating_add(1);
            self.cvar.notify_all();
        }
    }

    #[test]
    fn loader_ticks_and_requests_render() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_clone = Arc::clone(&requests);
        let render_requester = Arc::new(move || {
            requests_clone.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(());
        });

        let sleeper: Arc<dyn Sleeper> = Arc::new(TestSleeper::default());
        let mut loader = Loader::with_requester_and_sleeper(
            Some(render_requester),
            Arc::clone(&sleeper),
            Box::new(|text| text.to_string()),
            Box::new(|text| text.to_string()),
            Some("Working".to_string()),
        );

        // `Loader::start()` requests an initial render. Drain any pre-existing
        // requests so we can deterministically observe the tick-triggered one.
        for _ in rx.try_iter() {}
        let baseline_requests = requests.load(Ordering::SeqCst);

        let before = loader.render(20);

        sleeper.wake();
        rx.recv_timeout(Duration::from_secs(1))
            .expect("tick render request not observed");
        let after = loader.render(20);

        assert!(requests.load(Ordering::SeqCst) > baseline_requests);
        assert_ne!(before.get(1), after.get(1));

        loader.stop();
    }
}
