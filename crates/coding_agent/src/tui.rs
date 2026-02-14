use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tape_tui::core::component::Focusable;
use tape_tui::core::cursor::CursorPos;
use tape_tui::core::input::KeyEventType;
use tape_tui::{
    default_editor_keybindings_handle, Component, Editor, EditorOptions, EditorTheme, InputEvent,
    Markdown, MarkdownTheme, SelectListTheme,
};

use crate::app::{App, HostOps, Message, Mode, Role};
use crate::provider::ProviderProfile;
use crate::runtime::{ProfileSwitchResult, RuntimeController};

struct HistoryUpdateGuard(Arc<AtomicBool>);

impl Drop for HistoryUpdateGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Plan,
    Build,
}

impl ViewMode {
    fn next(self) -> Self {
        match self {
            Self::Plan => Self::Build,
            Self::Build => Self::Plan,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Build => "build",
        }
    }
}

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

fn underline(text: &str) -> String {
    ansi_wrap(text, "\x1b[4m", "\x1b[24m")
}

fn italic(text: &str) -> String {
    ansi_wrap(text, "\x1b[3m", "\x1b[23m")
}

fn strikethrough(text: &str) -> String {
    ansi_wrap(text, "\x1b[9m", "\x1b[29m")
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
    provider_profile: ProviderProfile,
    working_directory_label: String,
    transcript_render_cache: Option<TranscriptRenderCache>,
    terminal_rows: usize,
    transcript_scroll_lines: usize,
    last_transcript_budget_rows: usize,
    last_rendered_transcript_line_count: usize,
    last_rendered_transcript_revision: Option<u64>,
    editor: Editor,
    is_applying_history: Arc<AtomicBool>,
    cursor_pos: Option<CursorPos>,
    view_mode: ViewMode,
}

#[derive(Debug, Clone)]
struct TranscriptRenderCache {
    width: usize,
    transcript_revision: u64,
    lines: Arc<Vec<String>>,
}

impl AppComponent {
    pub fn new(
        app: Arc<Mutex<App>>,
        host: Arc<RuntimeController>,
        provider_profile: ProviderProfile,
    ) -> Self {
        let app_for_change = Arc::clone(&app);
        let app_for_submit = Arc::clone(&app);
        let host_for_submit = Arc::clone(&host);
        let is_applying_history = Arc::new(AtomicBool::new(false));
        let history_changer = Arc::clone(&is_applying_history);

        let mut editor = Editor::new(
            editor_theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.set_on_change(Some(Box::new(move |value| {
            if history_changer.load(Ordering::SeqCst) {
                return;
            }

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
                thread::spawn(move || loop {
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
                });
            }
        })));

        Self {
            app,
            host,
            provider_profile,
            working_directory_label: render_working_directory(),
            transcript_render_cache: None,
            terminal_rows: 0,
            transcript_scroll_lines: 0,
            last_transcript_budget_rows: 0,
            last_rendered_transcript_line_count: 0,
            last_rendered_transcript_revision: None,
            editor,
            is_applying_history,
            cursor_pos: None,
            view_mode: ViewMode::Plan,
        }
    }

    fn with_app_mut(&self, mut f: impl FnMut(&mut App, &mut dyn HostOps)) {
        let mut app = lock_unpoisoned(&self.app);
        let mut host = Arc::clone(&self.host);
        f(&mut app, &mut host);
    }

    fn render_transcript_lines_cached(&mut self, width: usize) -> (Arc<Vec<String>>, Mode, u64) {
        let (mode, transcript_revision) = {
            let app = lock_unpoisoned(&self.app);
            (app.mode.clone(), app.transcript_revision())
        };

        if let Some(cache) = self.transcript_render_cache.as_ref() {
            if cache.width == width && cache.transcript_revision == transcript_revision {
                return (Arc::clone(&cache.lines), mode, transcript_revision);
            }
        }

        let rendered_lines = {
            let app = lock_unpoisoned(&self.app);
            let mut lines = Vec::new();

            for message in &app.transcript {
                render_message_lines(&app, message, width, &mut lines);
                lines.push(separator_line(width));
            }

            Arc::new(lines)
        };

        self.transcript_render_cache = Some(TranscriptRenderCache {
            width,
            transcript_revision,
            lines: Arc::clone(&rendered_lines),
        });

        (rendered_lines, mode, transcript_revision)
    }

    fn set_editor_text_with_history_bypass(&mut self, text: &str) {
        let _guard = HistoryUpdateGuard(Arc::clone(&self.is_applying_history));
        self.is_applying_history.store(true, Ordering::SeqCst);
        self.editor.set_text(text);
    }

    fn cycle_model_shortcut(&mut self) {
        let message = match self.host.cycle_model_profile() {
            ProfileSwitchResult::Updated(profile) => {
                let model = profile.model_id.trim();
                let model = if model.is_empty() {
                    "unknown".to_string()
                } else {
                    model.to_string()
                };
                self.provider_profile = profile;
                format!("Switched model to {model}")
            }
            ProfileSwitchResult::RejectedWhileRunning => {
                "Cannot switch model while a run is active".to_string()
            }
            ProfileSwitchResult::Failed(error) => format!("Model switch failed: {error}"),
        };

        self.with_app_mut(|app, host| {
            app.push_system_message(message.as_str());
            host.request_render();
        });
    }

    fn cycle_thinking_shortcut(&mut self) {
        let message = match self.host.cycle_thinking_profile() {
            ProfileSwitchResult::Updated(profile) => {
                let thinking = profile
                    .thinking_level
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "none".to_string());
                self.provider_profile = profile;
                format!("Switched thinking mode to {thinking}")
            }
            ProfileSwitchResult::RejectedWhileRunning => {
                "Cannot switch thinking mode while a run is active".to_string()
            }
            ProfileSwitchResult::Failed(error) => format!("Thinking mode switch failed: {error}"),
        };

        self.with_app_mut(|app, host| {
            app.push_system_message(message.as_str());
            host.request_render();
        });
    }

    fn cached_transcript_line_count(&self) -> usize {
        self.transcript_render_cache
            .as_ref()
            .map(|cache| cache.lines.len())
            .unwrap_or(self.last_rendered_transcript_line_count)
    }

    fn transcript_scroll_jump_rows(&self) -> usize {
        self.last_transcript_budget_rows.max(1)
    }

    fn scroll_transcript_page_up(&mut self) {
        let total_lines = self.cached_transcript_line_count();
        self.transcript_scroll_lines = scroll_transcript_page_up(
            self.transcript_scroll_lines,
            total_lines,
            self.transcript_scroll_jump_rows(),
        );
    }

    fn scroll_transcript_page_down(&mut self) {
        let total_lines = self.cached_transcript_line_count();
        self.transcript_scroll_lines = scroll_transcript_page_down(
            self.transcript_scroll_lines,
            total_lines,
            self.transcript_scroll_jump_rows(),
        );
    }

    fn scroll_transcript_home(&mut self) {
        let total_lines = self.cached_transcript_line_count();
        self.transcript_scroll_lines = max_transcript_scroll_lines(total_lines);
    }

    fn scroll_transcript_end(&mut self) {
        self.transcript_scroll_lines = 0;
    }
}

impl Component for AppComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let (transcript_lines, mode, transcript_revision) =
            self.render_transcript_lines_cached(width);

        if self
            .last_rendered_transcript_revision
            .is_some_and(|last| last != transcript_revision)
        {
            self.transcript_scroll_lines = apply_transcript_growth_to_scroll(
                self.transcript_scroll_lines,
                self.last_rendered_transcript_line_count,
                transcript_lines.len(),
            );
        }
        self.transcript_scroll_lines =
            clamp_transcript_scroll_lines(self.transcript_scroll_lines, transcript_lines.len());
        self.last_rendered_transcript_revision = Some(transcript_revision);
        self.last_rendered_transcript_line_count = transcript_lines.len();

        let mut header_lines = Vec::new();
        append_wrapped_text(&mut header_lines, width, &render_header(), "", "");

        let mut status_lines = Vec::new();
        append_wrapped_text(&mut status_lines, width, &render_status_line(&mode), "", "");

        let mut editor_lines = self.editor.render(width);
        if let Some(editor_border) = editor_lines.get_mut(0) {
            *editor_border = render_mode_line(width, self.view_mode);
        }

        let mut footer_lines = Vec::new();
        append_wrapped_text(
            &mut footer_lines,
            width,
            &render_status_footer(width, &self.provider_profile, &self.working_directory_label),
            "",
            "",
        );

        let fixed_rows = header_lines
            .len()
            .saturating_add(status_lines.len())
            .saturating_add(editor_lines.len())
            .saturating_add(footer_lines.len());
        let transcript_budget_rows = self.terminal_rows.saturating_sub(fixed_rows);
        self.last_transcript_budget_rows = transcript_budget_rows;

        let transcript_window = compute_transcript_window(
            transcript_lines.len(),
            transcript_budget_rows,
            self.transcript_scroll_lines,
        );

        let mut lines = Vec::with_capacity(
            header_lines
                .len()
                .saturating_add(transcript_window.rendered_rows())
                .saturating_add(status_lines.len())
                .saturating_add(editor_lines.len())
                .saturating_add(footer_lines.len()),
        );
        lines.extend(header_lines);

        if transcript_window.show_indicator {
            lines.push(render_transcript_indicator_line(
                width,
                &format!(
                    "… {} earlier lines above (PgUp/PgDn/Home/End)",
                    transcript_window.start
                ),
            ));
        }
        lines.extend(
            transcript_lines[transcript_window.start..transcript_window.end]
                .iter()
                .cloned(),
        );

        lines.extend(status_lines);
        let editor_start_row = lines.len();
        lines.extend(editor_lines);
        lines.extend(footer_lines);

        self.cursor_pos = self.editor.cursor_pos().map(|position| CursorPos {
            row: position.row + editor_start_row,
            col: position.col,
        });

        lines
    }

    fn cursor_pos(&self) -> Option<CursorPos> {
        self.cursor_pos
    }

    fn set_terminal_rows(&mut self, rows: usize) {
        self.terminal_rows = rows;
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
                    let mut next_input = None;
                    self.with_app_mut(|app, host| {
                        app.on_control_c(host);
                        next_input = Some(app.input.clone());
                    });

                    if let Some(next_input) = next_input {
                        self.set_editor_text_with_history_bypass(&next_input);
                    }
                }
                "ctrl+p" => {
                    self.cycle_model_shortcut();
                }
                "ctrl+t" => {
                    self.cycle_thinking_shortcut();
                }
                "shift+tab" => {
                    self.view_mode = self.view_mode.next();
                    let mut host = Arc::clone(&self.host);
                    host.request_render();
                }
                "pageUp" => {
                    self.scroll_transcript_page_up();
                    let mut host = Arc::clone(&self.host);
                    host.request_render();
                }
                "pageDown" => {
                    self.scroll_transcript_page_down();
                    let mut host = Arc::clone(&self.host);
                    host.request_render();
                }
                "home" => {
                    self.scroll_transcript_home();
                    let mut host = Arc::clone(&self.host);
                    host.request_render();
                }
                "end" => {
                    self.scroll_transcript_end();
                    let mut host = Arc::clone(&self.host);
                    host.request_render();
                }
                "up" | "\u{1b}[A" | "\u{1b}OA" => {
                    let mut next_input = None;
                    self.with_app_mut(|app, host| {
                        app.on_input_history_previous();
                        next_input = Some(app.input.clone());
                        host.request_render();
                    });

                    if let Some(next_input) = next_input {
                        self.set_editor_text_with_history_bypass(&next_input);
                    }
                }
                "down" | "\u{1b}[B" | "\u{1b}OB" => {
                    let mut next_input = None;
                    self.with_app_mut(|app, host| {
                        app.on_input_history_next();
                        next_input = Some(app.input.clone());
                        host.request_render();
                    });

                    if let Some(next_input) = next_input {
                        self.set_editor_text_with_history_bypass(&next_input);
                    }
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
            format!("{} {}", cyan("*"), dim("Ready - awaiting your input"))
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
            let home = std::env::var("HOME").ok();
            format_working_directory_with_home(&cwd, &branch, home.as_deref())
        }
        Err(_) => dim("<unable to read current working directory>").to_string(),
    }
}

fn render_provider_metadata(profile: &ProviderProfile) -> String {
    let provider_id = profile.provider_id.trim();
    let provider_id = if provider_id.is_empty() {
        "unknown"
    } else {
        provider_id
    };

    let model_id = profile.model_id.trim();
    let model_id = if model_id.is_empty() {
        "unknown"
    } else {
        model_id
    };

    let mut metadata = format!(
        "{} {} {} {} {}",
        dim("provider"),
        cyan(provider_id),
        dim("•"),
        dim("model"),
        cyan(model_id)
    );

    if let Some(thinking_level) = profile
        .thinking_level
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        metadata.push_str(&format!(
            " {} {} {}",
            dim("•"),
            dim("thinking"),
            yellow(thinking_level)
        ));
    }

    metadata
}

fn format_working_directory_with_home(cwd: &str, branch: &str, home: Option<&str>) -> String {
    let display_path = home
        .map(|home| {
            if cwd == home {
                "~".to_string()
            } else {
                cwd.strip_prefix(&format!("{home}/"))
                    .map_or(cwd.to_string(), |rest| format!("~/{rest}"))
            }
        })
        .unwrap_or_else(|| cwd.to_string());

    format!("{} {}", dim(&display_path), dim(&format!("({branch})")))
}

fn render_status_footer(
    width: usize,
    provider_profile: &ProviderProfile,
    working_directory_label: &str,
) -> String {
    let left = working_directory_label;
    let right = render_provider_metadata(provider_profile);
    let left_width = visible_text_width(left);
    let right_width = visible_text_width(&right);

    if width == 0 {
        return String::new();
    }

    if left_width + right_width + 2 > width {
        if right_width >= width {
            right
        } else {
            format!("{:>width$}", right, width = width)
        }
    } else {
        let fill = width - (left_width + right_width);
        format!("{left}{}{}", " ".repeat(fill), right)
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

fn render_mode_line(width: usize, view_mode: ViewMode) -> String {
    let label = format!(" {} ", view_mode.label());
    let label_width = visible_text_width(&label);

    if width == 0 {
        return String::new();
    }

    if label_width >= width {
        return dim(&"─".repeat(width));
    }

    if width <= 2 + label_width {
        return dim(&"─".repeat(width));
    }

    let right_pad = width - 2 - label_width;
    format!(
        "{}{}{}",
        dim("──"),
        yellow_dim(&label),
        dim(&"─".repeat(right_pad))
    )
}

fn separator_line(width: usize) -> String {
    let max = width.max(10);
    dim(&"─".repeat(max))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TranscriptWindow {
    start: usize,
    end: usize,
    show_indicator: bool,
}

impl TranscriptWindow {
    fn rendered_rows(self) -> usize {
        self.end
            .saturating_sub(self.start)
            .saturating_add(usize::from(self.show_indicator))
    }
}

fn max_transcript_scroll_lines(total_lines: usize) -> usize {
    total_lines.saturating_sub(1)
}

fn clamp_transcript_scroll_lines(scroll_lines: usize, total_lines: usize) -> usize {
    scroll_lines.min(max_transcript_scroll_lines(total_lines))
}

fn scroll_transcript_page_up(scroll_lines: usize, total_lines: usize, jump: usize) -> usize {
    clamp_transcript_scroll_lines(scroll_lines.saturating_add(jump), total_lines)
}

fn scroll_transcript_page_down(scroll_lines: usize, total_lines: usize, jump: usize) -> usize {
    clamp_transcript_scroll_lines(scroll_lines.saturating_sub(jump), total_lines)
}

fn apply_transcript_growth_to_scroll(
    scroll_lines: usize,
    previous_total_lines: usize,
    current_total_lines: usize,
) -> usize {
    if scroll_lines == 0 {
        return 0;
    }

    let growth = current_total_lines.saturating_sub(previous_total_lines);
    let grown_scroll = scroll_lines.saturating_add(growth);
    clamp_transcript_scroll_lines(grown_scroll, current_total_lines)
}

fn render_transcript_indicator_line(width: usize, text: &str) -> String {
    if width == 0 {
        return text.to_string();
    }

    let mut chars = text.chars();
    let mut truncated = String::new();
    for _ in 0..width {
        let Some(ch) = chars.next() else {
            break;
        };
        truncated.push(ch);
    }

    if chars.next().is_some() && width >= 1 {
        truncated.pop();
        truncated.push('…');
    }

    truncated
}

fn compute_transcript_window(
    total_lines: usize,
    transcript_budget_rows: usize,
    transcript_scroll_lines: usize,
) -> TranscriptWindow {
    let scroll_lines = clamp_transcript_scroll_lines(transcript_scroll_lines, total_lines);
    let end = total_lines.saturating_sub(scroll_lines);

    if transcript_budget_rows == 0 {
        return TranscriptWindow {
            start: end,
            end,
            show_indicator: false,
        };
    }

    let mut content_budget = transcript_budget_rows;
    let mut start = end.saturating_sub(content_budget);
    let mut show_indicator = start > 0;

    if show_indicator {
        content_budget = content_budget.saturating_sub(1);
        start = end.saturating_sub(content_budget);
        show_indicator = start > 0;

        if !show_indicator {
            content_budget = transcript_budget_rows;
            start = end.saturating_sub(content_budget);
        }
    }

    TranscriptWindow {
        start,
        end,
        show_indicator,
    }
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

fn render_message_lines(app: &App, message: &Message, width: usize, lines: &mut Vec<String>) {
    let role_prefix = message_role_prefix(message);

    if message.content.is_empty() {
        append_wrapped_text(lines, width, "", &format!("{role_prefix}: "), "  ");
        return;
    }

    match message.role {
        Role::Assistant => {
            append_wrapped_text(lines, width, &format!("{role_prefix}:"), "", "");
            let markdown_lines = render_markdown_lines(width.saturating_sub(2), &message.content);
            for line in markdown_lines {
                lines.push(format!("  {line}"));
            }
        }
        Role::Tool => {
            let text_lines = message_display_lines(app, message);
            for line in text_lines {
                append_wrapped_text(lines, width, line.as_str(), "", "");
            }
        }
        _ => {
            let text_lines = message_display_lines(app, message);
            for (index, line) in text_lines.iter().enumerate() {
                let prefix = if index == 0 {
                    format!("{role_prefix}: ")
                } else {
                    "  ".to_string()
                };
                append_wrapped_text(lines, width, line, &prefix, "  ");
            }
        }
    }
}

fn message_display_lines(app: &App, message: &Message) -> Vec<String> {
    match message.role {
        Role::Tool => tool_message_display_lines(app, message),
        _ => message
            .content
            .split('\n')
            .map(ToString::to_string)
            .collect(),
    }
}

fn tool_message_display_lines(app: &App, message: &Message) -> Vec<String> {
    let fallback_lines: Vec<String> = message
        .content
        .split('\n')
        .map(ToString::to_string)
        .collect();

    let Some(run_id) = message.run_id else {
        return fallback_lines;
    };

    let Some((tool_name, call_id, kind)) = parse_tool_timeline_message(message.content.as_str())
    else {
        return fallback_lines;
    };

    match kind {
        ToolMessageKind::Started => {
            let Some(arguments) = app.tool_call_arguments(run_id, call_id) else {
                return fallback_lines;
            };

            vec![format_tool_started_line(tool_name, arguments)]
        }
        ToolMessageKind::Completed | ToolMessageKind::Failed => {
            let Some((content, is_error)) = app.tool_call_result(run_id, call_id) else {
                return fallback_lines;
            };

            let mut lines = Vec::new();
            let status = if is_error { "failed" } else { "completed" };
            lines.push(dim(&format!("{tool_name} {status}")));
            lines.extend(render_value_content(content));
            lines
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolMessageKind {
    Started,
    Completed,
    Failed,
}

fn parse_tool_timeline_message(content: &str) -> Option<(&str, &str, ToolMessageKind)> {
    let body = content.strip_prefix("Tool ")?;
    let (tool_name, call_part) = body.split_once(" (")?;
    let (call_id, suffix) = call_part.split_once(") ")?;

    let kind = if suffix.starts_with("started") {
        ToolMessageKind::Started
    } else if suffix.starts_with("completed") {
        ToolMessageKind::Completed
    } else if suffix.starts_with("failed") {
        ToolMessageKind::Failed
    } else {
        return None;
    };

    Some((tool_name, call_id, kind))
}

fn format_tool_started_line(tool_name: &str, arguments: &Value) -> String {
    match tool_name {
        "bash" => {
            let command = argument_string(arguments, "command").unwrap_or("<missing command>");
            let mut line = format!("$ {command}");
            if let Some(timeout_sec) = argument_u64(arguments, "timeout_sec") {
                line.push(' ');
                line.push_str(&dim(&format!("(timeout {timeout_sec}s)")));
            }
            line
        }
        "read" => {
            let path = argument_string(arguments, "path").unwrap_or("<missing path>");
            format!("read {path}")
        }
        "write" => {
            let path = argument_string(arguments, "path").unwrap_or("<missing path>");
            format!("write {path}")
        }
        "edit" => {
            let path = argument_string(arguments, "path").unwrap_or("<missing path>");
            format!("edit {path}")
        }
        "apply_patch" => {
            let input = argument_string(arguments, "input").unwrap_or_default();
            format!(
                "apply_patch {}",
                dim(&format!("({} chars)", input.chars().count()))
            )
        }
        _ => format!("{tool_name} {arguments}"),
    }
}

fn render_value_content(value: &Value) -> Vec<String> {
    match value {
        Value::String(content) => {
            if content.is_empty() {
                vec![dim("<empty>")]
            } else {
                content.lines().map(ToString::to_string).collect()
            }
        }
        _ => serde_json::to_string_pretty(value)
            .unwrap_or_else(|_| value.to_string())
            .lines()
            .map(ToString::to_string)
            .collect(),
    }
}

fn argument_string<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn argument_u64(arguments: &Value, key: &str) -> Option<u64> {
    arguments.get(key).and_then(Value::as_u64)
}

fn render_markdown_lines(width: usize, text: &str) -> Vec<String> {
    let mut markdown = Markdown::new(text, 0, 0, markdown_theme(), None);
    let rendered = markdown.render(width);
    rendered
        .into_iter()
        .map(|line| line.trim_end().to_string())
        .collect()
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

fn message_role_prefix(message: &Message) -> String {
    let (role, role_label) = match message.role {
        Role::User => (cyan("[user]"), "you"),
        Role::Assistant => (blue("[asst]"), "assistant"),
        Role::System => (green_dim("[sys]"), "system"),
        Role::Tool => (magenta("[tool]"), "tool"),
    };

    format!("{role} {role_label}")
}

fn append_wrapped_text(
    lines: &mut Vec<String>,
    width: usize,
    text: &str,
    first_prefix: &str,
    continuation_prefix: &str,
) {
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

        let ch = match std::str::from_utf8(&bytes[index..])
            .ok()
            .and_then(|rest| rest.chars().next())
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_window_follow_tail_uses_visible_budget() {
        let window = compute_transcript_window(100, 10, 0);
        assert_eq!(window.start, 91);
        assert_eq!(window.end, 100);
        assert!(window.show_indicator);

        let small_window = compute_transcript_window(5, 10, 0);
        assert_eq!(small_window.start, 0);
        assert_eq!(small_window.end, 5);
        assert!(!small_window.show_indicator);
    }

    #[test]
    fn transcript_window_scrolled_mode_pins_end_before_latest_lines() {
        let window = compute_transcript_window(100, 10, 5);
        assert_eq!(window.start, 86);
        assert_eq!(window.end, 95);
        assert!(window.show_indicator);
    }

    #[test]
    fn transcript_indicator_requires_start_above_zero() {
        let window = compute_transcript_window(3, 3, 0);
        assert_eq!(window.start, 0);
        assert!(!window.show_indicator);

        let with_space = compute_transcript_window(20, 1, 0);
        assert_eq!(with_space.start, 20);
        assert!(with_space.show_indicator);
        assert_eq!(with_space.end, 20);
        assert_eq!(with_space.rendered_rows(), 1);
    }

    #[test]
    fn transcript_scroll_clamps_and_home_end_stay_in_bounds() {
        assert_eq!(max_transcript_scroll_lines(0), 0);
        assert_eq!(max_transcript_scroll_lines(1), 0);
        assert_eq!(max_transcript_scroll_lines(10), 9);

        assert_eq!(clamp_transcript_scroll_lines(100, 0), 0);
        assert_eq!(clamp_transcript_scroll_lines(100, 10), 9);

        assert_eq!(scroll_transcript_page_up(0, 10, 5), 5);
        assert_eq!(scroll_transcript_page_up(7, 10, 5), 9);
        assert_eq!(scroll_transcript_page_down(7, 10, 5), 2);
        assert_eq!(scroll_transcript_page_down(2, 10, 5), 0);
    }

    #[test]
    fn transcript_scroll_follow_tail_semantics_hold_across_growth() {
        assert_eq!(apply_transcript_growth_to_scroll(0, 10, 15), 0);
        assert_eq!(apply_transcript_growth_to_scroll(3, 10, 15), 8);
        assert_eq!(apply_transcript_growth_to_scroll(9, 10, 15), 14);
    }

    #[test]
    fn transcript_indicator_line_is_single_row_and_truncates() {
        let line =
            render_transcript_indicator_line(12, "… 123 earlier lines above (PgUp/PgDn/Home/End)");
        assert_eq!(line.chars().count(), 12);
        assert!(line.ends_with('…'));

        let full =
            render_transcript_indicator_line(80, "… 3 earlier lines above (PgUp/PgDn/Home/End)");
        assert_eq!(full, "… 3 earlier lines above (PgUp/PgDn/Home/End)");
    }

    #[test]
    fn render_mode_line_is_left_anchored() {
        let line = strip_ansi(&render_mode_line(30, ViewMode::Plan));
        assert!(line.starts_with("──"));
        assert!(line.contains(" plan "));
        assert_eq!(line.chars().count(), 30);
    }

    #[test]
    fn render_markdown_lines_keeps_empty_lines() {
        let lines = render_markdown_lines(80, "first paragraph\n\nsecond paragraph");
        assert_eq!(strip_ansi(&lines[0]), "first paragraph");
        assert!(lines.len() >= 3);
        assert!(strip_ansi(&lines[1]).trim().is_empty());
    }

    #[test]
    fn render_working_directory_uses_home_alias() {
        let line = strip_ansi(&format_working_directory_with_home(
            "/Users/dev/project",
            "main",
            Some("/Users/dev"),
        ));
        assert_eq!(line, "~/project (main)");
        let line = strip_ansi(&format_working_directory_with_home(
            "/Users/dev",
            "main",
            Some("/Users/dev"),
        ));
        assert_eq!(line, "~ (main)");
        let line = strip_ansi(&format_working_directory_with_home(
            "/tmp/other",
            "main",
            Some("/Users/dev"),
        ));
        assert_eq!(line, "/tmp/other (main)");
    }

    #[test]
    fn provider_metadata_includes_provider_model_and_thinking() {
        let profile = ProviderProfile {
            provider_id: "mock".to_string(),
            model_id: "gpt-5-codex".to_string(),
            thinking_level: Some("medium".to_string()),
        };

        let line = strip_ansi(&render_provider_metadata(&profile));
        assert_eq!(line, "provider mock • model gpt-5-codex • thinking medium");
    }

    #[test]
    fn provider_metadata_includes_off_thinking_level() {
        let profile = ProviderProfile {
            provider_id: "codex-api".to_string(),
            model_id: "gpt-5.3-codex".to_string(),
            thinking_level: Some("off".to_string()),
        };

        let line = strip_ansi(&render_provider_metadata(&profile));
        assert_eq!(
            line,
            "provider codex-api • model gpt-5.3-codex • thinking off"
        );
    }

    #[test]
    fn provider_metadata_omits_thinking_when_profile_has_none() {
        let profile = ProviderProfile {
            provider_id: "mock".to_string(),
            model_id: "gpt-5-codex".to_string(),
            thinking_level: None,
        };

        let line = strip_ansi(&render_provider_metadata(&profile));
        assert_eq!(line, "provider mock • model gpt-5-codex");
    }

    #[test]
    fn view_mode_cycles_between_plan_and_build() {
        assert_eq!(ViewMode::Plan.next(), ViewMode::Build);
        assert_eq!(ViewMode::Build.next(), ViewMode::Plan);
    }

    #[test]
    fn tool_message_display_lines_include_clean_bash_started_command() {
        let mut app = App::new();
        app.mode = Mode::Running { run_id: 7 };
        app.on_tool_call_started(
            7,
            "call-1",
            "bash",
            &serde_json::json!({
                "command": "echo hello",
                "cwd": "/tmp",
                "timeout_sec": 30
            }),
        );

        let message = app
            .transcript
            .iter()
            .find(|message| message.role == Role::Tool)
            .expect("tool message should exist");

        let lines = tool_message_display_lines(&app, message);
        assert_eq!(strip_ansi(&lines[0]), "$ echo hello (timeout 30s)");
    }

    #[test]
    fn tool_message_display_lines_include_clean_completed_tool_output_content() {
        let mut app = App::new();
        app.mode = Mode::Running { run_id: 7 };
        app.on_tool_call_started(
            7,
            "call-1",
            "bash",
            &serde_json::json!({ "command": "pwd" }),
        );
        app.on_tool_call_finished(
            7,
            "bash",
            "call-1",
            false,
            &serde_json::json!("line-1\nline-2"),
            "line-1\nline-2",
        );

        let message = app
            .transcript
            .iter()
            .find(|message| message.role == Role::Tool && message.content.contains("completed"))
            .expect("completed tool message should exist");

        let lines = tool_message_display_lines(&app, message);
        assert_eq!(strip_ansi(&lines[0]), "bash completed");
        assert!(lines.iter().any(|line| line == "line-1"));
        assert!(lines.iter().any(|line| line == "line-2"));
    }

    #[test]
    fn tool_message_display_lines_leave_non_started_entries_unchanged() {
        let app = App::new();
        let message = Message {
            role: Role::Tool,
            content: "Tool bash (call-1) completed".to_string(),
            streaming: false,
            run_id: Some(7),
        };

        let lines = tool_message_display_lines(&app, &message);
        assert_eq!(lines, vec!["Tool bash (call-1) completed".to_string()]);
    }

    #[test]
    fn render_message_lines_renders_tool_rows_without_role_prefix() {
        let mut app = App::new();
        app.mode = Mode::Running { run_id: 7 };
        app.on_tool_call_started(
            7,
            "call-1",
            "bash",
            &serde_json::json!({
                "command": "head -c 16 /dev/urandom | xxd -p > hi.txt",
                "timeout_sec": 5
            }),
        );

        let message = app
            .transcript
            .iter()
            .find(|message| message.role == Role::Tool)
            .expect("tool message should exist")
            .clone();

        let mut lines = Vec::new();
        render_message_lines(&app, &message, 200, &mut lines);

        assert_eq!(
            strip_ansi(&lines[0]),
            "$ head -c 16 /dev/urandom | xxd -p > hi.txt (timeout 5s)"
        );
    }
}
