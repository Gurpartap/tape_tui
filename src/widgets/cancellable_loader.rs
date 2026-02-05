//! Cancellable loader widget (Phase 23).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::core::component::Component;
use crate::core::keybindings::{get_editor_keybindings, EditorAction};
use crate::runtime::tui::RenderHandle;
use crate::widgets::loader::Loader;

#[derive(Clone)]
pub struct AbortSignal {
    aborted: Arc<AtomicBool>,
}

impl AbortSignal {
    pub fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

pub struct CancellableLoader {
    loader: Loader,
    abort_signal: AbortSignal,
    on_abort: Option<Box<dyn FnMut()>>,
}

impl CancellableLoader {
    pub fn new(
        render_handle: RenderHandle,
        spinner_color_fn: Box<dyn Fn(&str) -> String>,
        message_color_fn: Box<dyn Fn(&str) -> String>,
        message: Option<String>,
    ) -> Self {
        let loader = Loader::new(render_handle, spinner_color_fn, message_color_fn, message);
        let aborted = Arc::new(AtomicBool::new(false));
        Self {
            loader,
            abort_signal: AbortSignal { aborted },
            on_abort: None,
        }
    }

    pub fn set_on_abort(&mut self, handler: Option<Box<dyn FnMut()>>) {
        self.on_abort = handler;
    }

    pub fn signal(&self) -> AbortSignal {
        self.abort_signal.clone()
    }

    pub fn aborted(&self) -> bool {
        self.abort_signal.aborted()
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.loader.set_message(message);
    }

    pub fn start(&mut self) {
        self.loader.start();
    }

    pub fn stop(&mut self) {
        self.loader.stop();
    }

    pub fn dispose(&mut self) {
        self.stop();
    }

    #[cfg(test)]
    fn with_requester(
        render_requester: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
        spinner_color_fn: Box<dyn Fn(&str) -> String>,
        message_color_fn: Box<dyn Fn(&str) -> String>,
        message: Option<String>,
    ) -> Self {
        let loader = super::loader::Loader::with_requester(render_requester, spinner_color_fn, message_color_fn, message);
        let aborted = Arc::new(AtomicBool::new(false));
        Self {
            loader,
            abort_signal: AbortSignal { aborted },
            on_abort: None,
        }
    }
}

impl Component for CancellableLoader {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.loader.render(width)
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_editor_keybindings();
        let kb = kb.lock().expect("editor keybindings lock poisoned");
        if kb.matches(data, EditorAction::SelectCancel) {
            if !self.abort_signal.aborted.swap(true, Ordering::SeqCst) {
                if let Some(handler) = self.on_abort.as_mut() {
                    handler();
                }
            }
        }
    }

    fn invalidate(&mut self) {
        self.loader.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::CancellableLoader;
    use crate::core::component::Component;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn cancellable_loader_aborts_on_cancel() {
        let aborted_flag = Arc::new(AtomicBool::new(false));
        let aborted_flag_clone = Arc::clone(&aborted_flag);

        let mut loader = CancellableLoader::with_requester(
            None,
            Box::new(|text| text.to_string()),
            Box::new(|text| text.to_string()),
            Some("Working".to_string()),
        );
        loader.set_on_abort(Some(Box::new(move || {
            aborted_flag_clone.store(true, Ordering::SeqCst);
        })));

        loader.handle_input("\x1b");

        assert!(loader.aborted());
        assert!(aborted_flag.load(Ordering::SeqCst));

        loader.stop();
    }
}
