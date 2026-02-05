use std::cell::RefCell;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use pi_tui::core::component::{Component, Focusable};
use pi_tui::render::slice::slice_by_column;
use pi_tui::{
    matches_key, set_editor_keybindings, truncate_to_width, visible_width, Editor, EditorAction,
    EditorKeybindingsConfig, EditorKeybindingsManager, EditorOptions, EditorTheme, Markdown,
    MarkdownTheme, OverlayAnchor, OverlayMargin, OverlayOptions, OverlayHandle, ProcessTerminal,
    SelectItem, SelectList, SelectListTheme, SizeValue, TUI,
};

const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

const HEADER_TITLE: &str = "Markdown Playground";

const HEADER_HINTS: &str = "Ctrl+P palette  •  Ctrl+C quit";

const FOOTER_HINTS: &str =
    "Enter: newline  •  Ctrl+P: palette  •  Esc: close palette  •  Ctrl+C: quit";

const SAMPLE_MARKDOWN: &str = r#"# Markdown Playground

Type Markdown on the left and see the preview on the right.

## Features

- **Bold**, *italic*, ~~strikethrough~~
- Links: [pi-tui](https://github.com/badlogic/pi-mono)
- Inline `code`

> Blockquotes wrap to the preview width.

---

### Code block

```rust
fn main() {
    println!("Hello from pi-tui!");
}
```

| Column | Value |
| --- | --- |
| A | 1 |
| B | 2 |
"#;

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

fn select_list_theme() -> SelectListTheme {
    SelectListTheme {
        selected_prefix: std::sync::Arc::new(|text| blue(text)),
        selected_text: std::sync::Arc::new(|text| bold(text)),
        description: std::sync::Arc::new(|text| dim(text)),
        scroll_info: std::sync::Arc::new(|text| dim(text)),
        no_match: std::sync::Arc::new(|text| dim(text)),
    }
}

fn editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Box::new(|text| dim(text)),
        select_list: select_list_theme(),
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

fn fixed_width(line: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = if visible_width(line) > width {
        slice_by_column(line, 0, width, true)
    } else {
        line.to_string()
    };

    // Reset before padding to avoid styled padding bleeding into separators.
    out.push_str(SEGMENT_RESET);

    let current = visible_width(&out);
    if current < width {
        out.push_str(&" ".repeat(width - current));
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaletteAction {
    InsertSample,
    ClearEditor,
    Quit,
    Close,
}

#[derive(Default)]
struct PaletteState {
    toggle_requested: bool,
    action: Option<PaletteAction>,
}

struct PaletteOverlay {
    list: SelectList,
    state: Rc<RefCell<PaletteState>>,
    exit_flag: Rc<RefCell<bool>>,
}

impl PaletteOverlay {
    fn new(state: Rc<RefCell<PaletteState>>, exit_flag: Rc<RefCell<bool>>) -> Self {
        let items = vec![
            SelectItem::new(
                "insert_sample",
                "Insert sample markdown",
                Some("Replace the editor contents with a Markdown sample".to_string()),
            ),
            SelectItem::new(
                "clear_editor",
                "Clear editor",
                Some("Delete all text".to_string()),
            ),
            SelectItem::new("quit", "Quit", Some("Exit the demo".to_string())),
        ];

        let mut list = SelectList::new(items, 8, select_list_theme());
        {
            let state_for_select = Rc::clone(&state);
            list.set_on_select(Some(Box::new(move |item| {
                let action = match item.value.as_str() {
                    "insert_sample" => PaletteAction::InsertSample,
                    "clear_editor" => PaletteAction::ClearEditor,
                    "quit" => PaletteAction::Quit,
                    _ => PaletteAction::Close,
                };
                state_for_select.borrow_mut().action = Some(action);
            })));
        }
        {
            let state_for_cancel = Rc::clone(&state);
            list.set_on_cancel(Some(Box::new(move || {
                state_for_cancel.borrow_mut().action = Some(PaletteAction::Close);
            })));
        }

        Self {
            list,
            state,
            exit_flag,
        }
    }
}

impl Component for PaletteOverlay {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();

        let title = bold("Command Palette");
        let hints = dim("Enter: run  Esc: close");
        let header = format!("{title}  {hints}");
        lines.push(dim(&"─".repeat(width.max(1))));
        lines.push(fixed_width(&header, width));
        lines.push(String::new());

        let inner_width = width.saturating_sub(4).max(1);
        for line in self.list.render(inner_width) {
            let content = format!("  {line}");
            lines.push(fixed_width(&content, width));
        }

        lines.push(dim(&"─".repeat(width.max(1))));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        if matches_key(data, "ctrl+c") {
            *self.exit_flag.borrow_mut() = true;
            return;
        }
        if matches_key(data, "ctrl+p") {
            self.state.borrow_mut().action = Some(PaletteAction::Close);
            return;
        }
        self.list.handle_input(data);
    }

    fn invalidate(&mut self) {
        self.list.invalidate();
    }
}

struct EditorWrapper {
    editor: Rc<RefCell<Editor>>,
    palette: Rc<RefCell<PaletteState>>,
    exit_flag: Rc<RefCell<bool>>,
}

impl EditorWrapper {
    fn new(
        editor: Rc<RefCell<Editor>>,
        palette: Rc<RefCell<PaletteState>>,
        exit_flag: Rc<RefCell<bool>>,
    ) -> Self {
        Self {
            editor,
            palette,
            exit_flag,
        }
    }
}

impl Component for EditorWrapper {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.editor.borrow_mut().render(width)
    }

    fn handle_input(&mut self, data: &str) {
        if matches_key(data, "ctrl+c") {
            *self.exit_flag.borrow_mut() = true;
            return;
        }
        if matches_key(data, "ctrl+p") {
            self.palette.borrow_mut().toggle_requested = true;
            return;
        }

        self.editor.borrow_mut().handle_input(data);
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

struct PlaygroundApp {
    editor: Rc<RefCell<Editor>>,
    markdown: Markdown,
    draft: Rc<RefCell<String>>,
    last_applied_draft: String,
    terminal_rows: usize,
}

impl PlaygroundApp {
    fn new(editor: Rc<RefCell<Editor>>, draft: Rc<RefCell<String>>) -> Self {
        Self {
            editor,
            markdown: Markdown::new("", 1, 1, markdown_theme(), None),
            draft,
            last_applied_draft: String::new(),
            terminal_rows: 0,
        }
    }

    fn apply_draft_if_needed(&mut self) {
        let current = self.draft.borrow().clone();
        if current != self.last_applied_draft {
            self.last_applied_draft = current.clone();
            self.markdown.set_text(current);
        }
    }
}

impl Component for PlaygroundApp {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.apply_draft_if_needed();

        let width = width.max(1);
        let sep = dim("│");
        let sep_width = if width >= 3 { 1 } else { 0 };
        let left_width = if sep_width == 0 {
            width
        } else {
            (width - sep_width) / 2
        }
        .max(1);
        let right_width = width.saturating_sub(left_width + sep_width);

        let mut lines = Vec::new();

        let title = bold(&cyan(HEADER_TITLE));
        let header = format!("{title}  {}", dim(HEADER_HINTS));
        lines.push(truncate_to_width(&header, width, "", true));

        let footer = dim(FOOTER_HINTS);

        let available_height = self.terminal_rows.saturating_sub(2);

        let left_lines = self.editor.borrow_mut().render(left_width);
        let right_lines = if right_width > 0 {
            self.markdown.render(right_width)
        } else {
            Vec::new()
        };

        let main_len = left_lines.len().max(right_lines.len());
        let mut main_lines = Vec::with_capacity(main_len);

        for idx in 0..main_len {
            let left = left_lines.get(idx).map(String::as_str).unwrap_or("");
            let right = right_lines.get(idx).map(String::as_str).unwrap_or("");

            let left_fixed = fixed_width(left, left_width);
            if sep_width == 0 {
                main_lines.push(left_fixed);
                continue;
            }

            let right_fixed = fixed_width(right, right_width);
            main_lines.push(format!("{left_fixed}{sep}{SEGMENT_RESET}{right_fixed}"));
        }

        if available_height > 0 {
            if main_lines.len() > available_height {
                main_lines.truncate(available_height);
            }
            while main_lines.len() < available_height {
                let left_blank = fixed_width("", left_width);
                if sep_width == 0 {
                    main_lines.push(left_blank);
                } else {
                    let right_blank = fixed_width("", right_width);
                    main_lines.push(format!("{left_blank}{sep}{SEGMENT_RESET}{right_blank}"));
                }
            }
        }

        lines.extend(main_lines);
        lines.push(truncate_to_width(&footer, width, "", true));
        lines
    }

    fn invalidate(&mut self) {
        self.editor.borrow_mut().invalidate();
        self.markdown.invalidate();
    }

    fn set_terminal_rows(&mut self, rows: usize) {
        self.terminal_rows = rows;
        self.editor.borrow_mut().set_terminal_rows(rows);
    }
}

fn overlay_options() -> OverlayOptions {
    let mut options = OverlayOptions::default();
    options.anchor = Some(OverlayAnchor::Center);
    options.margin = Some(OverlayMargin::uniform(2));
    options.width = Some(SizeValue::percent(60.0));
    options.min_width = Some(34);
    options.max_height = Some(SizeValue::percent(60.0));
    options
}

fn install_playground_keybindings() {
    let mut config = EditorKeybindingsConfig::new();
    config.set(EditorAction::Submit, Vec::<String>::new());
    config.set(
        EditorAction::NewLine,
        vec!["enter".to_string(), "shift+enter".to_string()],
    );
    set_editor_keybindings(EditorKeybindingsManager::new(config));
}

fn main() {
    install_playground_keybindings();

    let terminal = ProcessTerminal::new();
    let root: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(EmptyComponent)));
    let mut tui = TUI::new(terminal, Rc::clone(&root));
    let render_handle = tui.render_handle();

    let draft = Rc::new(RefCell::new(String::new()));
    let draft_for_change = Rc::clone(&draft);
    let render_for_change = render_handle.clone();

    let editor = Rc::new(RefCell::new(Editor::new(
        editor_theme(),
        EditorOptions {
            render_handle: Some(render_handle.clone()),
            ..EditorOptions::default()
        },
    )));
    editor.borrow_mut().set_on_change(Some(Box::new(move |text| {
        *draft_for_change.borrow_mut() = text;
        render_for_change.request_render();
    })));
    editor.borrow_mut().set_text(SAMPLE_MARKDOWN);

    let app = PlaygroundApp::new(Rc::clone(&editor), Rc::clone(&draft));
    *root.borrow_mut() = Box::new(app);

    let exit_flag = Rc::new(RefCell::new(false));
    let palette_state = Rc::new(RefCell::new(PaletteState::default()));

    let editor_wrapper: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(
        EditorWrapper::new(Rc::clone(&editor), Rc::clone(&palette_state), Rc::clone(&exit_flag)),
    )));
    tui.set_focus(Rc::clone(&editor_wrapper));

    let mut palette_handle: Option<OverlayHandle> = None;

    tui.start();

    loop {
        tui.run_once();

        if *exit_flag.borrow() {
            break;
        }

        let toggle = {
            let mut palette = palette_state.borrow_mut();
            if palette.toggle_requested {
                palette.toggle_requested = false;
                true
            } else {
                false
            }
        };
        if toggle {
            if let Some(handle) = palette_handle.take() {
                handle.hide();
                tui.request_render();
            } else {
                let overlay: Rc<RefCell<Box<dyn Component>>> = Rc::new(RefCell::new(Box::new(
                    PaletteOverlay::new(Rc::clone(&palette_state), Rc::clone(&exit_flag)),
                )));
                let handle = tui.show_overlay(Rc::clone(&overlay), Some(overlay_options()));
                palette_handle = Some(handle);
            }
        }

        let action = { palette_state.borrow_mut().action.take() };
        if let Some(action) = action {
            if let Some(handle) = palette_handle.take() {
                handle.hide();
            }

            match action {
                PaletteAction::InsertSample => {
                    editor.borrow_mut().set_text(SAMPLE_MARKDOWN);
                }
                PaletteAction::ClearEditor => {
                    editor.borrow_mut().set_text("");
                }
                PaletteAction::Quit => {
                    *exit_flag.borrow_mut() = true;
                }
                PaletteAction::Close => {}
            }

            tui.request_render();
        }

        thread::sleep(Duration::from_millis(16));
    }

    tui.stop();
}

#[derive(Default)]
struct EmptyComponent;

impl Component for EmptyComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }
}

