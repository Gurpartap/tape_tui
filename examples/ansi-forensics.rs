use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use pi_tui::core::component::{Component, Focusable};
use pi_tui::core::text::slice::slice_by_column;
use pi_tui::{
    default_editor_keybindings_handle, truncate_to_width, visible_width, Editor, EditorAction,
    EditorHeightMode, EditorKeybindingsConfig, EditorKeybindingsHandle, EditorOptions,
    EditorPasteMode, EditorTheme, InputEvent, KeyEventType, Markdown, MarkdownTheme,
    ProcessTerminal, Terminal, TUI,
};

const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

const HEADER_TITLE: &str = "ANSI Forensics";
const HEADER_HINTS: &str = "Ctrl+C quit  •  Ctrl+T auto tick  •  Ctrl+L clear stats";

const FOOTER_HINTS: &str =
    "Type in the editor to force diffs. Toggle auto tick to generate steady tiny updates.";

const SAMPLE_MARKDOWN: &str = r#"# ANSI Forensics

This demo is measuring the *bytes* the TUI writes to the terminal.

Try:

- Type a few characters (small diffs).
- Paste a big chunk (bigger diffs).
- Toggle the auto tick (steady minimal updates).

> The stats panel itself is rendered by the same diff renderer: it is part of the measured output.
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

fn editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Box::new(dim),
        select_list: pi_tui::SelectListTheme {
            selected_prefix: Arc::new(blue),
            selected_text: Arc::new(bold),
            description: Arc::new(dim),
            scroll_info: Arc::new(dim),
            no_match: Arc::new(dim),
        },
    }
}

fn markdown_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: Box::new(cyan),
        link: Box::new(blue),
        link_url: Box::new(dim),
        code: Box::new(yellow),
        code_block: Box::new(green),
        code_block_border: Box::new(dim),
        quote: Box::new(italic),
        quote_border: Box::new(dim),
        hr: Box::new(dim),
        list_bullet: Box::new(cyan),
        bold: Box::new(bold),
        italic: Box::new(italic),
        strikethrough: Box::new(strikethrough),
        underline: Box::new(underline),
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

    // Reset before padding to avoid styled padding bleeding across separators.
    out.push_str(SEGMENT_RESET);

    let current = visible_width(&out);
    if current < width {
        out.push_str(&" ".repeat(width - current));
    }
    out
}

#[derive(Clone, Debug)]
struct TickStats {
    bytes_written: usize,
    total_bytes: usize,
    sync_open: bool,
    sync_close: bool,
    clear_2j: bool,
    clear_3j: bool,
    home: bool,
    crlf: usize,
    esc_k: usize,
    cursor_moves: usize,
    sample: String,
}

#[derive(Default)]
struct ForensicsState {
    tick_history: VecDeque<TickStats>,
    terminal_rows: usize,
    auto_tick: bool,
}

impl ForensicsState {
    fn push_tick(&mut self, tick: TickStats) {
        const MAX_TICKS: usize = 50;
        self.tick_history.push_back(tick);
        while self.tick_history.len() > MAX_TICKS {
            self.tick_history.pop_front();
        }
    }

    fn clear_ticks(&mut self) {
        self.tick_history.clear();
    }
}

struct ForensicsApp {
    editor: Rc<RefCell<Editor>>,
    markdown: Markdown,
    draft: Rc<RefCell<String>>,
    last_applied_draft: String,
    state: Rc<RefCell<ForensicsState>>,
    start: Instant,
}

impl ForensicsApp {
    fn new(
        editor: Rc<RefCell<Editor>>,
        draft: Rc<RefCell<String>>,
        state: Rc<RefCell<ForensicsState>>,
        start: Instant,
    ) -> Self {
        Self {
            editor,
            markdown: Markdown::new("", 1, 1, markdown_theme(), None),
            draft,
            last_applied_draft: String::new(),
            state,
            start,
        }
    }

    fn apply_draft_if_needed(&mut self) {
        let current = self.draft.borrow().clone();
        if current != self.last_applied_draft {
            self.last_applied_draft = current.clone();
            self.markdown.set_text(current);
        }
    }

    fn stats_lines(&self, width: usize) -> Vec<String> {
        let state = self.state.borrow();
        let last = state.tick_history.back();
        let total_bytes = last.map(|t| t.total_bytes).unwrap_or(0);

        let (
            bytes_written,
            sync_open,
            sync_close,
            clear_2j,
            clear_3j,
            home,
            crlf,
            esc_k,
            cursor_moves,
            sample,
        ) = last
            .map(|t| {
                (
                    t.bytes_written,
                    t.sync_open,
                    t.sync_close,
                    t.clear_2j,
                    t.clear_3j,
                    t.home,
                    t.crlf,
                    t.esc_k,
                    t.cursor_moves,
                    t.sample.as_str(),
                )
            })
            .unwrap_or((0, false, false, false, false, false, 0, 0, 0, ""));

        let bytes_label = cyan(&bytes_written.to_string()).to_string();
        let total_label = dim(&total_bytes.to_string()).to_string();

        let sync_label = if sync_open && sync_close {
            green("yes")
        } else if sync_open || sync_close {
            yellow("partial")
        } else {
            dim("no")
        };

        let clear_label = if clear_3j || clear_2j {
            yellow("yes")
        } else {
            dim("no")
        };

        let line1 = format!(
            "{} {}  {} {}",
            dim("last write:"),
            bytes_label,
            dim("total:"),
            total_label
        );
        let line2 = format!(
            "{} {}  {} {}  {} {}",
            dim("sync:"),
            sync_label,
            dim("full clear:"),
            clear_label,
            dim("cursor moves:"),
            cyan(&cursor_moves.to_string())
        );
        let line3 = format!(
            "{} {}  {} {}  {} {}",
            dim("CRLF:"),
            cyan(&crlf.to_string()),
            dim("ESC[K:"),
            cyan(&esc_k.to_string()),
            dim("home:"),
            if home { yellow("yes") } else { dim("no") }
        );

        let spark = ascii_sparkline(state.tick_history.iter().map(|t| t.bytes_written));
        let line4 = format!("{} {}", dim("bytes trend:"), dim(&spark));

        let auto = if state.auto_tick {
            green("ON")
        } else {
            dim("OFF")
        };
        let clock = if state.auto_tick {
            let elapsed = self.start.elapsed();
            cyan(&format!(
                "{}.{:03}s",
                elapsed.as_secs(),
                elapsed.subsec_millis()
            ))
        } else {
            dim("n/a")
        };
        let line5 = format!(
            "{} {}  {} {}",
            dim("auto tick:"),
            auto,
            dim("clock:"),
            clock
        );

        let sample_line = format!("{} {}", dim("output sample:"), dim(sample));

        vec![
            truncate_to_width(&line1, width, "", true),
            truncate_to_width(&line2, width, "", true),
            truncate_to_width(&line3, width, "", true),
            truncate_to_width(&line4, width, "", true),
            truncate_to_width(&line5, width, "", true),
            truncate_to_width(&sample_line, width, "", true),
        ]
    }
}

impl Component for ForensicsApp {
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

        lines.extend(self.stats_lines(width));

        let footer = dim(FOOTER_HINTS);

        // header + stats panel + footer
        let reserved = 1usize + 6usize + 1usize;
        let terminal_rows = self.state.borrow().terminal_rows;
        let available_height = terminal_rows.saturating_sub(reserved);

        let left_lines = {
            let mut editor = self.editor.borrow_mut();
            editor.set_terminal_rows(available_height);
            editor.render(left_width)
        };

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
        self.state.borrow_mut().terminal_rows = rows;
    }
}

struct EditorWrapper {
    editor: Rc<RefCell<Editor>>,
    state: Rc<RefCell<ForensicsState>>,
    exit_flag: Rc<RefCell<bool>>,
    auto_tick: Arc<AtomicBool>,
}

impl EditorWrapper {
    fn new(
        editor: Rc<RefCell<Editor>>,
        state: Rc<RefCell<ForensicsState>>,
        exit_flag: Rc<RefCell<bool>>,
        auto_tick: Arc<AtomicBool>,
    ) -> Self {
        Self {
            editor,
            state,
            exit_flag,
            auto_tick,
        }
    }
}

impl Component for EditorWrapper {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.editor.borrow_mut().render(width)
    }

    fn handle_event(&mut self, event: &InputEvent) {
        if matches!(
            event,
            InputEvent::Key {
                key_id,
                event_type: KeyEventType::Press,
                ..
            } if key_id == "ctrl+c"
        ) {
            *self.exit_flag.borrow_mut() = true;
            return;
        }
        if matches!(
            event,
            InputEvent::Key {
                key_id,
                event_type: KeyEventType::Press,
                ..
            } if key_id == "ctrl+t"
        ) {
            let mut state = self.state.borrow_mut();
            state.auto_tick = !state.auto_tick;
            self.auto_tick.store(state.auto_tick, Ordering::SeqCst);
            return;
        }
        if matches!(
            event,
            InputEvent::Key {
                key_id,
                event_type: KeyEventType::Press,
                ..
            } if key_id == "ctrl+l"
        ) {
            self.state.borrow_mut().clear_ticks();
            return;
        }

        self.editor.borrow_mut().handle_event(event);
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

struct TickCapture {
    bytes_written: usize,
    total_bytes: usize,
    output: Vec<u8>,
}

struct Collector {
    max_tick_output_bytes: usize,
    total_bytes: usize,
    tick_bytes: usize,
    tick_output: Vec<u8>,
}

impl Collector {
    fn new(max_tick_output_bytes: usize) -> Self {
        Self {
            max_tick_output_bytes,
            total_bytes: 0,
            tick_bytes: 0,
            tick_output: Vec::new(),
        }
    }

    fn begin_tick(&mut self) {
        self.tick_bytes = 0;
        self.tick_output.clear();
    }

    fn record_write(&mut self, data: &str) {
        let bytes = data.as_bytes();
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        self.tick_bytes = self.tick_bytes.saturating_add(bytes.len());

        self.tick_output.extend_from_slice(bytes);
        if self.tick_output.len() > self.max_tick_output_bytes {
            let excess = self.tick_output.len() - self.max_tick_output_bytes;
            self.tick_output.drain(0..excess);
        }
    }

    fn end_tick(&mut self) -> TickCapture {
        TickCapture {
            bytes_written: self.tick_bytes,
            total_bytes: self.total_bytes,
            output: std::mem::take(&mut self.tick_output),
        }
    }
}

struct ForensicsTerminal<T: Terminal> {
    inner: T,
    collector: Arc<Mutex<Collector>>,
}

impl<T: Terminal> ForensicsTerminal<T> {
    fn new(inner: T, collector: Arc<Mutex<Collector>>) -> Self {
        Self { inner, collector }
    }
}

impl<T: Terminal> Terminal for ForensicsTerminal<T> {
    fn start(
        &mut self,
        on_input: Box<dyn FnMut(String) + Send>,
        on_resize: Box<dyn FnMut() + Send>,
    ) -> std::io::Result<()> {
        self.inner.start(on_input, on_resize)
    }

    fn stop(&mut self) -> std::io::Result<()> {
        self.inner.stop()
    }

    fn drain_input(&mut self, max_ms: u64, idle_ms: u64) {
        self.inner.drain_input(max_ms, idle_ms);
    }

    fn write(&mut self, data: &str) {
        if let Ok(mut collector) = self.collector.lock() {
            collector.record_write(data);
        }
        self.inner.write(data);
    }

    fn columns(&self) -> u16 {
        self.inner.columns()
    }

    fn rows(&self) -> u16 {
        self.inner.rows()
    }
}

fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn count_subsequence(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0usize;
    let mut idx = 0usize;
    while idx + needle.len() <= haystack.len() {
        let Some(pos) = haystack[idx..]
            .windows(needle.len())
            .position(|window| window == needle)
        else {
            break;
        };
        count = count.saturating_add(1);
        idx = idx.saturating_add(pos + needle.len());
    }
    count
}

fn count_csi_with_final_byte(bytes: &[u8], final_byte: u8) -> usize {
    let mut count = 0usize;
    let mut i = 0usize;
    while i + 2 <= bytes.len() {
        if bytes[i] != 0x1b || bytes[i + 1] != b'[' {
            i += 1;
            continue;
        }

        let start = i;
        let mut next_i = start + 2;
        let mut j = start + 2;
        while j < bytes.len() && j - start <= 32 {
            let b = bytes[j];
            if (0x40..=0x7e).contains(&b) {
                if b == final_byte {
                    count = count.saturating_add(1);
                }
                next_i = j + 1;
                break;
            }
            j += 1;
        }

        i = next_i;
    }
    count
}

fn escape_sample(bytes: &[u8], max_chars: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        if used >= max_chars {
            break;
        }
        match ch {
            '\x1b' => {
                out.push_str("<ESC>");
                used = used.saturating_add(5);
            }
            '\r' => {
                out.push_str("\\r");
                used = used.saturating_add(2);
            }
            '\n' => {
                out.push_str("\\n");
                used = used.saturating_add(2);
            }
            c if c.is_control() => {
                let rendered = format!("\\x{:02x}", c as u32);
                used = used.saturating_add(rendered.len());
                out.push_str(&rendered);
            }
            _ => {
                out.push(ch);
                used = used.saturating_add(1);
            }
        }
    }
    out
}

fn ascii_sparkline<I>(values: I) -> String
where
    I: IntoIterator<Item = usize>,
{
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return String::new();
    }

    let max_value = values.iter().copied().max().unwrap_or(0);
    if max_value == 0 {
        return ".".repeat(values.len());
    }

    const LEVELS: &[u8] = b".:-=+*#%@";
    let mut out = String::with_capacity(values.len());
    for value in values {
        let idx = (value.saturating_mul(LEVELS.len().saturating_sub(1))) / max_value;
        out.push(LEVELS[idx] as char);
    }
    out
}

fn derive_stats(capture: TickCapture) -> TickStats {
    let output = &capture.output;

    let sync_open = contains_subsequence(output, b"\x1b[?2026h");
    let sync_close = contains_subsequence(output, b"\x1b[?2026l");

    let clear_2j = contains_subsequence(output, b"\x1b[2J");
    let clear_3j = contains_subsequence(output, b"\x1b[3J");
    let home = contains_subsequence(output, b"\x1b[H");

    let crlf = count_subsequence(output, b"\r\n");
    let esc_k = count_subsequence(output, b"\x1b[K");
    let cursor_moves = count_csi_with_final_byte(output, b'H');

    let sample = escape_sample(output, 120);

    TickStats {
        bytes_written: capture.bytes_written,
        total_bytes: capture.total_bytes,
        sync_open,
        sync_close,
        clear_2j,
        clear_3j,
        home,
        crlf,
        esc_k,
        cursor_moves,
        sample,
    }
}

fn install_demo_keybindings(handle: &EditorKeybindingsHandle) {
    // Make the embedded editor behave like a multi-line scratchpad: Enter inserts a newline.
    let mut config = EditorKeybindingsConfig::new();
    config.set(EditorAction::Submit, Vec::<String>::new());
    config.set(
        EditorAction::NewLine,
        vec!["enter".to_string(), "shift+enter".to_string()],
    );
    let mut kb = handle.lock().expect("editor keybindings lock poisoned");
    kb.set_config(config);
}

fn main() -> std::io::Result<()> {
    let keybindings = default_editor_keybindings_handle();
    install_demo_keybindings(&keybindings);

    let collector = Arc::new(Mutex::new(Collector::new(64 * 1024)));
    let terminal = ForensicsTerminal::new(ProcessTerminal::new(), Arc::clone(&collector));

    let root: Rc<RefCell<Box<dyn Component>>> =
        Rc::new(RefCell::new(Box::new(pi_tui::widgets::Spacer::new())));
    let mut tui = TUI::new(terminal, Rc::clone(&root));
    let render_handle = tui.render_handle();

    let state = Rc::new(RefCell::new(ForensicsState::default()));
    let auto_tick = Arc::new(AtomicBool::new(false));

    let draft = Rc::new(RefCell::new(String::new()));
    let draft_for_change = Rc::clone(&draft);
    let render_for_change = render_handle.clone();

    let editor = Rc::new(RefCell::new(Editor::new(
        editor_theme(),
        keybindings.clone(),
        EditorOptions {
            height_mode: Some(EditorHeightMode::FillAvailable),
            paste_mode: Some(EditorPasteMode::Literal),
            render_handle: Some(render_handle.clone()),
            ..EditorOptions::default()
        },
    )));

    editor
        .borrow_mut()
        .set_on_change(Some(Box::new(move |text| {
            *draft_for_change.borrow_mut() = text;
            render_for_change.request_render();
        })));

    editor.borrow_mut().set_text(SAMPLE_MARKDOWN);
    *draft.borrow_mut() = SAMPLE_MARKDOWN.to_string();

    let start = Instant::now();
    let app = ForensicsApp::new(
        Rc::clone(&editor),
        Rc::clone(&draft),
        Rc::clone(&state),
        start,
    );
    *root.borrow_mut() = Box::new(app);

    let exit_flag = Rc::new(RefCell::new(false));
    let editor_wrapper: Rc<RefCell<Box<dyn Component>>> =
        Rc::new(RefCell::new(Box::new(EditorWrapper::new(
            Rc::clone(&editor),
            Rc::clone(&state),
            Rc::clone(&exit_flag),
            Arc::clone(&auto_tick),
        ))));
    tui.set_focus(Rc::clone(&editor_wrapper));

    tui.start()?;
    render_handle.request_render();

    {
        let auto_tick = Arc::clone(&auto_tick);
        let render_handle = render_handle.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(250));
            if auto_tick.load(Ordering::SeqCst) {
                render_handle.request_render();
            }
        });
    }

    loop {
        {
            let mut c = collector.lock().expect("collector lock poisoned");
            c.begin_tick();
        }

        tui.run_blocking_once();

        if *exit_flag.borrow() {
            break;
        }

        let capture = {
            let mut c = collector.lock().expect("collector lock poisoned");
            c.end_tick()
        };

        if capture.bytes_written > 0 {
            let tick = derive_stats(capture);
            state.borrow_mut().push_tick(tick);
        }
    }

    tui.stop()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::count_csi_with_final_byte;

    #[test]
    fn count_csi_with_final_byte_counts_simple_sequence() {
        let bytes = b"\x1b[H";
        assert_eq!(count_csi_with_final_byte(bytes, b'H'), 1);
    }
}
