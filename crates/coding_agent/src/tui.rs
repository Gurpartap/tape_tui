use std::sync::{Arc, Mutex, MutexGuard};

use tape_tui::core::cursor::CursorPos;
use tape_tui::core::input::KeyEventType;
use tape_tui::{Component, InputEvent};

use crate::app::{App, HostOps, Message, Mode, Role};
use crate::runtime::RuntimeController;

pub struct AppComponent {
    app: Arc<Mutex<App>>,
    host: Arc<RuntimeController>,
    cursor_pos: Option<CursorPos>,
}

impl AppComponent {
    pub fn new(app: Arc<Mutex<App>>, host: Arc<RuntimeController>) -> Self {
        Self {
            app,
            host,
            cursor_pos: None,
        }
    }

    fn with_app_mut(&self, mut f: impl FnMut(&mut App, &mut dyn HostOps)) {
        let mut app = lock_unpoisoned(&self.app);
        let mut host = Arc::clone(&self.host);
        f(&mut app, &mut host);
    }

    fn snapshot(&self) -> App {
        lock_unpoisoned(&self.app).clone()
    }
}

impl Component for AppComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        let snapshot = self.snapshot();
        let mut lines = Vec::new();
        lines.push(render_status_line(&snapshot.mode));

        for message in &snapshot.transcript {
            render_message_lines(message, &mut lines);
        }

        let prompt_prefix = "> ";
        let cursor_row = lines.len();
        let cursor_col = prompt_prefix.chars().count() + snapshot.input.chars().count();
        lines.push(format!("{prompt_prefix}{}", snapshot.input));

        self.cursor_pos = Some(CursorPos {
            row: cursor_row,
            col: cursor_col,
        });

        lines
    }

    fn cursor_pos(&self) -> Option<CursorPos> {
        self.cursor_pos
    }

    fn handle_event(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Key {
                key_id,
                event_type: KeyEventType::Press,
                ..
            } => match key_id.as_str() {
                "enter" => {
                    self.with_app_mut(|app, host| app.on_submit(host));
                }
                "escape" => {
                    self.with_app_mut(|app, host| app.on_cancel(host));
                }
                "ctrl+c" => {
                    self.with_app_mut(|app, host| app.on_quit(host));
                }
                "backspace" => {
                    self.with_app_mut(|app, host| {
                        if app.input.pop().is_some() {
                            host.request_render();
                        }
                    });
                }
                _ => {}
            },
            InputEvent::Text {
                text,
                event_type: KeyEventType::Press,
                ..
            } => {
                if text.is_empty() {
                    return;
                }

                self.with_app_mut(|app, host| {
                    let mut updated = std::mem::take(&mut app.input);
                    updated.push_str(text);
                    app.on_input_replace(updated);
                    host.request_render();
                });
            }
            InputEvent::Paste { text, .. } => {
                if text.is_empty() {
                    return;
                }

                self.with_app_mut(|app, host| {
                    let mut updated = std::mem::take(&mut app.input);
                    updated.push_str(text);
                    app.on_input_replace(updated);
                    host.request_render();
                });
            }
            _ => {}
        }
    }
}

fn render_status_line(mode: &Mode) -> String {
    match mode {
        Mode::Idle => "Status: Idle".to_string(),
        Mode::Running { run_id } => format!("Status: Running (run_id={run_id})"),
        Mode::Error(error) => format!("Status: Error ({error})"),
        Mode::Exiting => "Status: Exiting".to_string(),
    }
}

fn render_message_lines(message: &Message, lines: &mut Vec<String>) {
    let role = match message.role {
        Role::User => "user",
        Role::Assistant if message.streaming => "assistant(streaming)",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    };

    if message.content.is_empty() {
        lines.push(format!("{role}:"));
        return;
    }

    for (index, line) in message.content.split('\n').enumerate() {
        if index == 0 {
            lines.push(format!("{role}: {line}"));
        } else {
            lines.push(format!("  {line}"));
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
