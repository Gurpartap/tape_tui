//! Loader widget (Phase 23).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::core::component::Component;
use crate::runtime::tui::RenderHandle;
use crate::widgets::text::Text;

type RenderRequester = Arc<dyn Fn() + Send + Sync>;

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct Loader {
    spinner_color_fn: Box<dyn Fn(&str) -> String>,
    message_color_fn: Box<dyn Fn(&str) -> String>,
    message: String,
    text: Text,
    render_requester: Option<RenderRequester>,
    current_frame: Arc<AtomicUsize>,
    stop_flag: Arc<AtomicBool>,
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
        let mut loader = Self {
            spinner_color_fn,
            message_color_fn,
            message: message.unwrap_or_else(|| "Loading...".to_string()),
            text: Text::with_padding("", 1, 0),
            render_requester,
            current_frame: Arc::new(AtomicUsize::new(0)),
            stop_flag: Arc::new(AtomicBool::new(false)),
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

        self.thread = Some(thread::spawn(move || {
            while !stop_flag.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(80));
                current_frame.fetch_add(1, Ordering::SeqCst);
                if let Some(request) = render_requester.as_ref() {
                    request();
                }
            }
        }));
    }

    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
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
    use super::Loader;
    use crate::core::component::Component;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn loader_ticks_and_requests_render() {
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_clone = Arc::clone(&requests);
        let render_requester = Arc::new(move || {
            requests_clone.fetch_add(1, Ordering::SeqCst);
        });

        let mut loader = Loader::with_requester(
            Some(render_requester),
            Box::new(|text| text.to_string()),
            Box::new(|text| text.to_string()),
            Some("Working".to_string()),
        );

        let before = loader.render(20);
        thread::sleep(Duration::from_millis(120));
        let after = loader.render(20);

        assert!(requests.load(Ordering::SeqCst) >= 1);
        assert_ne!(before.get(1), after.get(1));

        loader.stop();
    }
}
