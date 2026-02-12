use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tape_tui::core::component::Focusable;
use tape_tui::core::cursor::CursorPos;
use tape_tui::core::input::KeyEventType;
use tape_tui::{
    default_editor_keybindings_handle, Component, Editor, EditorOptions, EditorTheme, InputEvent,
    SelectListTheme,
};

use crate::app::{App, HostOps, Message, Mode, Role};
use crate::runtime::RuntimeController;

fn ansi_wrap(text: &str, prefix: &str, suffix: &str) -> String {
    format!("{prefix}{text}{suffix}")
}

fn dim(text: &str) -> String {
    ansi_wrap(text, "\x1b[2m", "\x1b[22m")
}

fn bold(text: &str) -> String {
    ansi_wrap(text, "\x1b[1m", "\x1b[22m")
}

fn blue(text: &str) -> String {
    ansi_wrap(text, "\x1b[34m", "\x1b[39m")
}

fn cyan(text: &str) -> String {
    ansi_wrap(text, "\x1b[36m", "\x1b[39m")
}

fn yellow(text: &str) -> String {
    ansi_wrap(text, "\x1b[33m", "\x1b[39m")
}

fn red(text: &str) -> String {
    ansi_wrap(text, "\x1b[31m", "\x1b[39m")
}

fn green(text: &str) -> String {
    ansi_wrap(text, "\x1b[32m", "\x1b[39m")
}

fn magenta(text: &str) -> String {
    ansi_wrap(text, "\x1b[35m", "\x1b[39m")
}

fn yellow_dim(text: &str) -> String {
    ansi_wrap(text, "\x1b[33m\x1b[2m", "\x1b[22m\x1b[39m")
}

fn editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Box::new(dim),
        select_list: SelectListTheme {
            selected_prefix: std::sync::Arc::new(blue),
            selected_text: std::sync::Arc::new(bold),
            description: std::sync::Arc::new(dim),
            scroll_info: std::sync::Arc::new(dim),
            no_match: std::sync::Arc::new(dim),
        },
    }
}

pub struct AppComponent {
    app: Arc<Mutex<App>>,
    host: Arc<RuntimeController>,
    editor: Editor,
    cursor_pos: Option<CursorPos>,
}

impl AppComponent {
    pub fn new(app: Arc<Mutex<App>>, host: Arc<RuntimeController>) -> Self {
        let app_for_change = Arc::clone(&app);
        let app_for_submit = Arc::clone(&app);
        let host_for_submit = Arc::clone(&host);

        let mut editor = Editor::new(
            editor_theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.set_on_change(Some(Box::new(move |value| {
            lock_unpoisoned(&app_for_change).on_input_replace(value);
        })));
        editor.set_on_submit(Some(Box::new(move |value| {
            let mut app = lock_unpoisoned(&app_for_submit);
            app.on_input_replace(value);

            let mut host = Arc::clone(&host_for_submit);
            app.on_submit(&mut host);

            if matches!(app.mode, Mode::Running { .. }) {
                let app_for_spinner = Arc::clone(&app_for_submit);
                let host_for_spinner = Arc::clone(&host_for_submit);
                thread::spawn(move || {
                    loop {
                        thread::sleep(Duration::from_millis(120));

                        let running = {
                            let app = lock_unpoisoned(&app_for_spinner);
                            matches!(app.mode, Mode::Running { .. })
                        };
                        if !running {
                            break;
                        }

                        let mut host = host_for_spinner.clone();
                        host.request_render();
                    }
                });
            }
        })));

        Self {
            app,
            host,
            editor,
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
    fn render(&mut self, width: usize) -> Vec<String> {
        let snapshot = self.snapshot();
        let mut lines = Vec::new();

        append_wrapped_text(&mut lines, width, &render_header(), "", "");
        for message in &snapshot.transcript {
            render_message_lines(message, width, &mut lines);
            lines.push(separator_line(width));
        }

        append_wrapped_text(
            &mut lines,
            width,
            &render_status_line(&snapshot.mode),
            "",
            "",
        );
        let editor_start_row = lines.len();
        lines.extend(self.editor.render(width));
        append_wrapped_text(
            &mut lines,
            width,
            &render_command_guide(),
            "",
            "",
        );
        append_wrapped_text(
            &mut lines,
            width,
            &render_working_directory(),
            "",
            "",
        );

        self.cursor_pos = self
            .editor
            .cursor_pos()
            .map(|position| CursorPos {
                row: position.row + editor_start_row,
                col: position.col,
            });

        lines
    }

    fn cursor_pos(&self) -> Option<CursorPos> {
        self.cursor_pos
    }

    fn set_terminal_rows(&mut self, rows: usize) {
        self.editor.set_terminal_rows(rows);
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        self.editor.as_focusable()
    }

    fn handle_event(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Key {
                key_id,
                event_type: KeyEventType::Press,
                ..
            } => match key_id.as_str() {
                "escape" => {
                    self.with_app_mut(|app, host| app.on_cancel(host));
                }
                "ctrl+c" => {
                    self.with_app_mut(|app, host| app.on_quit(host));
                }
                _ => {
                    self.editor.handle_event(event);
                }
            },
            _ => {
                self.editor.handle_event(event);
            }
        }
    }
}

fn render_status_line(mode: &Mode) -> String {
    match mode {
        Mode::Idle => {
            format!(
                "{} {}",
                cyan("*"),
                dim("Ready - awaiting your input")
            )
        }
        Mode::Running { run_id } => {
            format!(
                "{} {} {}",
                spinner_glyph(),
                yellow_dim("Working"),
                green(&format!("run_id={run_id}"))
            )
        }
        Mode::Error(error) => format!("{} {} {}", red("!"), red("Error:"), dim(error)),
        Mode::Exiting => {
            format!("{} {}", yellow_dim("Shutting down"), yellow("..."))
        }
    }
}

fn render_header() -> String {
    format!(
        "{} {}",
        bold("Coding Agent"),
        dim("local coding workflow runner")
    )
}

fn render_working_directory() -> String {
    match std::env::current_dir() {
        Ok(path) => {
            let cwd = path.display().to_string();
            let branch = current_git_branch().unwrap_or_else(|| "unknown".to_string());
            format!("{} {} {}", dim("cwd"), cyan(&cwd), dim(&format!("({branch})")))
        }
        Err(_) => format!(
            "{} {}",
            dim("cwd"),
            red("<unable to read current working directory>")
        ),
    }
}

fn current_git_branch() -> Option<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8(output.stdout).ok()?;
    let branch = branch.trim();
    if branch.is_empty() {
        None
    } else {
        Some(branch.to_string())
    }
}

fn render_command_guide() -> String {
    let commands = ["/help", "/clear", "/cancel", "/quit"];
    format!(
        "{} {}",
        dim("Commands:"),
        commands
            .iter()
            .map(|command| cyan(command))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn separator_line(width: usize) -> String {
    let max = width.max(10);
    dim(&"─".repeat(max.saturating_sub(1)))
}

fn spinner_glyph() -> String {
    const FRAMES: [&str; 4] = ["|", "/", "-", "\\"];
    let index = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| since_epoch.subsec_millis().try_into().ok())
        .unwrap_or(0);
    FRAMES[(index / 120 % 4) as usize].to_string()
}

fn render_message_lines(message: &Message, width: usize, lines: &mut Vec<String>) {
    let role_prefix = message_role_prefix(message);

    if message.content.is_empty() {
        append_wrapped_text(lines, width, "", &format!("{role_prefix}: "), "  ");
        return;
    }

    for (index, line) in message.content.split('\n').enumerate() {
        let prefix = if index == 0 {
            format!("{role_prefix}: ")
        } else {
            "  ".to_string()
        };
        append_wrapped_text(lines, width, line, &prefix, "  ");
    }
}

fn message_role_prefix(message: &Message) -> String {
    let (role, role_label) = match message.role {
        Role::User => (cyan("[user]"), "you"),
        Role::Assistant => (blue("[asst]"), "assistant"),
        Role::System => (green_dim("[sys]"), "system"),
        Role::Tool => (magenta("[tool]"), "tool"),
    };

    format!("{role} {role_label}")
}

fn append_wrapped_text(lines: &mut Vec<String>, width: usize, text: &str, first_prefix: &str, continuation_prefix: &str) {
    if width == 0 {
        lines.push(format!("{first_prefix}{text}"));
        return;
    }

    let width = width.max(1);
    let mut current_prefix = first_prefix.to_string();
    let mut line = current_prefix.clone();
    let mut visible_len = visible_text_width(&line);
    let mut line_capacity = width;

    if text.is_empty() {
        lines.push(line);
        return;
    }

    let mut index = 0;
    let bytes = text.as_bytes();
    while index < bytes.len() {
        if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'[' {
            let start = index;
            index += 2;
            while index < bytes.len() {
                let byte = bytes[index];
                index += 1;
                if (b'@'..=b'~').contains(&byte) {
                    break;
                }
            }
            line.push_str(std::str::from_utf8(&bytes[start..index]).unwrap_or_default());
            continue;
        }

    let ch = match std::str::from_utf8(&bytes[index..]).ok().and_then(|rest| rest.chars().next()) {
        Some(ch) => ch,
        None => break,
    };
    index += ch.len_utf8();

        if ch == '\n' {
            lines.push(line);
            current_prefix = continuation_prefix.to_string();
            line = current_prefix.clone();
            visible_len = visible_text_width(&line);
            line_capacity = width;
            continue;
        }

        if visible_len >= line_capacity {
            lines.push(line);
            line = continuation_prefix.to_string();
            visible_len = visible_text_width(&line);
            line_capacity = width;
        }

        line.push(ch);
        visible_len += 1;
    }

    lines.push(line);
}

fn green_dim(text: &str) -> String {
    ansi_wrap(text, "\x1b[32m\x1b[2m", "\x1b[22m\x1b[39m")
}

fn visible_text_width(text: &str) -> usize {
    strip_ansi(text).chars().count()
}

fn strip_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'[' {
            index += 2;
            while index < bytes.len() {
                let byte = bytes[index];
                index += 1;
                if (b'@'..=b'~').contains(&byte) {
                    break;
                }
            }
            continue;
        }

        output.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(output).unwrap_or_default()
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
