use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pi_tui::core::autocomplete::CommandEntry;
use pi_tui::core::component::{Component, Focusable};
use pi_tui::widgets::select_list::SelectListTheme;
use pi_tui::{
    default_editor_keybindings_handle, CombinedAutocompleteProvider, Editor, EditorOptions,
    EditorTheme, InputEvent, Loader, Markdown, MarkdownTheme, ProcessTerminal, SlashCommand, Text,
    TUI,
};

const WELCOME_TEXT: &str =
    "Welcome to Simple Chat!\n\nType your messages below. Type '/' for commands. Press Ctrl+C to exit.";

const RESPONSES: [&str; 8] = [
    "That's interesting! Tell me more.",
    "I see what you mean.",
    "Fascinating perspective!",
    "Could you elaborate on that?",
    "That makes sense to me.",
    "I hadn't thought of it that way.",
    "Great point!",
    "Thanks for sharing that.",
];

fn ansi_wrap(text: &str, prefix: &str, suffix: &str) -> String {
    format!("{prefix}{text}{suffix}")
}

fn dim(text: &str) -> String {
    ansi_wrap(text, "\x1b[2m", "\x1b[22m")
}

fn bold(text: &str) -> String {
    ansi_wrap(text, "\x1b[1m", "\x1b[22m")
}

fn italic(text: &str) -> String {
    ansi_wrap(text, "\x1b[3m", "\x1b[23m")
}

fn underline(text: &str) -> String {
    ansi_wrap(text, "\x1b[4m", "\x1b[24m")
}

fn strikethrough(text: &str) -> String {
    ansi_wrap(text, "\x1b[9m", "\x1b[29m")
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

fn green(text: &str) -> String {
    ansi_wrap(text, "\x1b[32m", "\x1b[39m")
}

struct PendingResponse {
    due: Instant,
    message: String,
}

struct ChatState {
    messages: Vec<Markdown>,
    loader: Option<Loader>,
    responding: bool,
    pending: Option<PendingResponse>,
}

impl ChatState {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
            loader: None,
            responding: false,
            pending: None,
        }
    }
}

struct ChatApp {
    welcome: Text,
    state: Rc<RefCell<ChatState>>,
    editor: Rc<RefCell<Editor>>,
}

impl Component for ChatApp {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(self.welcome.render(width));

        {
            let mut state = self.state.borrow_mut();
            if let Some(pending) = state.pending.as_ref() {
                if Instant::now() >= pending.due {
                    let message = pending.message.clone();
                    state.pending = None;
                    state.responding = false;
                    state.loader = None;
                    state
                        .messages
                        .push(Markdown::new(message, 1, 1, markdown_theme(), None));
                    self.editor.borrow_mut().set_disable_submit(false);
                }
            }
            for message in state.messages.iter_mut() {
                lines.extend(message.render(width));
            }
            if let Some(loader) = state.loader.as_mut() {
                lines.extend(loader.render(width));
            }
        }

        lines.extend(self.editor.borrow_mut().render(width));
        lines
    }

    fn invalidate(&mut self) {
        self.welcome.invalidate();
        let mut state = self.state.borrow_mut();
        for message in state.messages.iter_mut() {
            message.invalidate();
        }
        if let Some(loader) = state.loader.as_mut() {
            loader.invalidate();
        }
        self.editor.borrow_mut().invalidate();
    }

    fn set_terminal_rows(&mut self, rows: usize) {
        self.editor.borrow_mut().set_terminal_rows(rows);
    }
}

struct EditorWrapper {
    editor: Rc<RefCell<Editor>>,
    exit_flag: Rc<RefCell<bool>>,
    state: Rc<RefCell<ChatState>>,
}

impl EditorWrapper {
    fn new(
        editor: Rc<RefCell<Editor>>,
        exit_flag: Rc<RefCell<bool>>,
        state: Rc<RefCell<ChatState>>,
    ) -> Self {
        Self {
            editor,
            exit_flag,
            state,
        }
    }
}

impl Component for EditorWrapper {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.editor.borrow_mut().render(width)
    }

    fn handle_event(&mut self, event: &InputEvent) {
        if matches!(event, InputEvent::Key { key_id, .. } if key_id == "ctrl+c") {
            *self.exit_flag.borrow_mut() = true;
            return;
        }
        {
            let mut editor = self.editor.borrow_mut();
            editor.handle_event(event);
        }
        if self.state.borrow().responding {
            self.editor.borrow_mut().set_disable_submit(true);
        }
    }

    fn invalidate(&mut self) {
        self.editor.borrow_mut().invalidate();
    }

    fn set_terminal_rows(&mut self, rows: usize) {
        self.editor.borrow_mut().set_terminal_rows(rows);
    }

    fn wants_key_release(&self) -> bool {
        self.editor.borrow().wants_key_release()
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for EditorWrapper {
    fn set_focused(&mut self, focused: bool) {
        self.editor.borrow_mut().set_focused(focused);
    }

    fn is_focused(&self) -> bool {
        self.editor.borrow().is_focused()
    }
}

#[derive(Default)]
struct DummyComponent;

impl Component for DummyComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }
}

fn editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Box::new(|text| dim(text)),
        select_list: SelectListTheme {
            selected_prefix: std::sync::Arc::new(|text| blue(text)),
            selected_text: std::sync::Arc::new(|text| bold(text)),
            description: std::sync::Arc::new(|text| dim(text)),
            scroll_info: std::sync::Arc::new(|text| dim(text)),
            no_match: std::sync::Arc::new(|text| dim(text)),
        },
    }
}

fn markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: Box::new(|text| cyan(text)),
        link: Box::new(|text| blue(text)),
        link_url: Box::new(|text| dim(text)),
        code: Box::new(|text| yellow(text)),
        code_block: Box::new(|text| green(text)),
        code_block_border: Box::new(|text| dim(text)),
        quote: Box::new(|text| italic(text)),
        quote_border: Box::new(|text| dim(text)),
        hr: Box::new(|text| dim(text)),
        list_bullet: Box::new(|text| cyan(text)),
        bold: Box::new(|text| bold(text)),
        italic: Box::new(|text| italic(text)),
        strikethrough: Box::new(|text| strikethrough(text)),
        underline: Box::new(|text| underline(text)),
        highlight_code: None,
        code_block_indent: None,
    }
}

fn pick_response() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.subsec_nanos() as usize)
        .unwrap_or(0);
    RESPONSES[nanos % RESPONSES.len()].to_string()
}

fn main() -> std::io::Result<()> {
    let terminal = ProcessTerminal::new();
    let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(DummyComponent)));
    let mut tui = TUI::new(terminal, Rc::clone(&root));
    let render_handle = tui.render_handle();

    let keybindings = default_editor_keybindings_handle();
    let editor = Rc::new(RefCell::new(Editor::new(
        editor_theme(),
        keybindings.clone(),
        EditorOptions {
            render_handle: Some(render_handle.clone()),
            ..EditorOptions::default()
        },
    )));

    let commands = vec![
        CommandEntry::Command(SlashCommand {
            name: "delete".to_string(),
            description: Some("Delete the last message".to_string()),
            get_argument_completions: None,
        }),
        CommandEntry::Command(SlashCommand {
            name: "clear".to_string(),
            description: Some("Clear all messages".to_string()),
            get_argument_completions: None,
        }),
    ];
    let base_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let autocomplete_provider = CombinedAutocompleteProvider::new(commands, base_path, None);
    editor
        .borrow_mut()
        .set_autocomplete_provider(Box::new(autocomplete_provider));

    let chat_state = Rc::new(RefCell::new(ChatState::new()));
    let state_for_submit = Rc::clone(&chat_state);
    let render_for_submit = render_handle.clone();
    editor
        .borrow_mut()
        .set_on_submit(Some(Box::new(move |value| {
            let trimmed = value.trim();
            let mut state = state_for_submit.borrow_mut();

            if state.responding {
                return;
            }

            if trimmed == "/delete" {
                if state.messages.len() > 1 {
                    state.messages.pop();
                }
                render_for_submit.request_render();
                return;
            }

            if trimmed == "/clear" {
                if state.messages.len() > 1 {
                    state.messages.truncate(1);
                }
                render_for_submit.request_render();
                return;
            }

            if trimmed.is_empty() {
                return;
            }

            state.responding = true;
            drop(state);

            let mut state = state_for_submit.borrow_mut();
            let user_message = Markdown::new(value, 1, 1, markdown_theme(), None);
            state.messages.push(user_message);

            let loader = Loader::new(
                render_for_submit.clone(),
                Box::new(|text| cyan(text)),
                Box::new(|text| dim(text)),
                Some("Thinking...".to_string()),
            );
            state.loader = Some(loader);

            let response = pick_response();
            let due = Instant::now() + Duration::from_millis(1000);
            state.pending = Some(PendingResponse {
                due,
                message: response,
            });
            {
                let render_for_timer = render_for_submit.clone();
                thread::spawn(move || {
                    let now = Instant::now();
                    if due > now {
                        thread::sleep(due - now);
                    }
                    render_for_timer.request_render();
                });
            }
            render_for_submit.request_render();
        })));

    let welcome = Text::new(WELCOME_TEXT);
    let chat_app = ChatApp {
        welcome,
        state: Rc::clone(&chat_state),
        editor: Rc::clone(&editor),
    };
    *root.borrow_mut() = Box::new(chat_app);

    let exit_flag = Rc::new(RefCell::new(false));
    let editor_wrapper: Rc<RefCell<Box<dyn Component>>> =
        Rc::new(RefCell::new(Box::new(EditorWrapper::new(
            Rc::clone(&editor),
            Rc::clone(&exit_flag),
            Rc::clone(&chat_state),
        ))));
    tui.set_focus(Rc::clone(&editor_wrapper));

    tui.start()?;

    loop {
        tui.run_blocking_once();

        if *exit_flag.borrow() {
            break;
        }
    }

    tui.stop()?;
    Ok(())
}
