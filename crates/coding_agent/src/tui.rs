use std::collections::VecDeque;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    editor: Editor,
    is_applying_history: Arc<AtomicBool>,
    cursor_pos: Option<CursorPos>,
    view_mode: ViewMode,
    debug_stats: DebugStats,
}

#[derive(Debug, Clone)]
struct TranscriptRenderCache {
    width: usize,
    transcript_revision: u64,
    lines: Arc<Vec<String>>,
}

#[derive(Debug, Clone)]
struct DebugStats {
    render_timestamps_ms: VecDeque<u64>,
    render_durations_ms: VecDeque<u64>,
    render_count_total: u64,
    cache_hits: u64,
    cache_misses: u64,
    last_frame_lines: usize,
    last_transcript_lines: usize,
    last_revision: u64,
    last_mode: Mode,
    last_out_bytes: usize,
    last_diff_commands: usize,
    last_input_queue_depth: usize,
}

impl DebugStats {
    fn new() -> Self {
        Self {
            render_timestamps_ms: VecDeque::new(),
            render_durations_ms: VecDeque::new(),
            render_count_total: 0,
            cache_hits: 0,
            cache_misses: 0,
            last_frame_lines: 0,
            last_transcript_lines: 0,
            last_revision: 0,
            last_mode: Mode::Idle,
            last_out_bytes: 0,
            last_diff_commands: 0,
            last_input_queue_depth: 0,
        }
    }
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
            editor,
            is_applying_history,
            cursor_pos: None,
            view_mode: ViewMode::Plan,
            debug_stats: DebugStats::new(),
        }
    }

    fn with_app_mut(&self, mut f: impl FnMut(&mut App, &mut dyn HostOps)) {
        let mut app = lock_unpoisoned(&self.app);
        let mut host = Arc::clone(&self.host);
        f(&mut app, &mut host);
    }

    fn render_transcript_lines_cached(&mut self, width: usize) -> (Arc<Vec<String>>, Mode) {
        let (mode, transcript_revision) = {
            let app = lock_unpoisoned(&self.app);
            (app.mode.clone(), app.transcript_revision())
        };

        if let Some(cache) = self.transcript_render_cache.as_ref() {
            if cache.width == width && cache.transcript_revision == transcript_revision {
                self.debug_stats.cache_hits = self.debug_stats.cache_hits.saturating_add(1);
                self.debug_stats.last_transcript_lines = cache.lines.len();
                self.debug_stats.last_revision = transcript_revision;
                return (Arc::clone(&cache.lines), mode);
            }
        }

        self.debug_stats.cache_misses = self.debug_stats.cache_misses.saturating_add(1);

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

        self.debug_stats.last_transcript_lines = rendered_lines.len();
        self.debug_stats.last_revision = transcript_revision;

        (rendered_lines, mode)
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
}

impl Component for AppComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let render_started_at = Instant::now();
        let now_ms = now_millis();
        record_render_timestamp_ms(&mut self.debug_stats, now_ms);
        self.debug_stats.render_count_total = self.debug_stats.render_count_total.saturating_add(1);

        let (transcript_lines, mode) = self.render_transcript_lines_cached(width);
        let mut lines = Vec::with_capacity(transcript_lines.len().saturating_add(10));

        append_wrapped_text(&mut lines, width, &render_header(), "", "");
        lines.extend(transcript_lines.iter().cloned());

        append_wrapped_text(&mut lines, width, &render_status_line(&mode), "", "");
        let editor_start_row = lines.len();
        let mut editor_lines = self.editor.render(width);
        if let Some(editor_border) = editor_lines.get_mut(0) {
            *editor_border = render_mode_line(width, self.view_mode);
        }
        lines.extend(editor_lines);
        append_wrapped_text(
            &mut lines,
            width,
            &render_status_footer(width, &self.provider_profile, &self.working_directory_label),
            "",
            "",
        );

        let telemetry = self.host.render_telemetry_snapshot();
        self.debug_stats.last_out_bytes = telemetry.out_bytes;
        self.debug_stats.last_diff_commands = telemetry.diff_commands;
        self.debug_stats.last_input_queue_depth = telemetry.pending_input_depth;

        let render_elapsed_ms = u64::try_from(render_started_at.elapsed().as_millis()).unwrap_or(0);
        record_render_duration_ms(&mut self.debug_stats, render_elapsed_ms);

        self.debug_stats.last_mode = mode.clone();
        self.debug_stats.last_frame_lines = lines.len().saturating_add(1);
        lines.push(render_debug_line(width, &self.debug_stats));

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

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| u64::try_from(since_epoch.as_millis()).ok())
        .unwrap_or(0)
}

fn record_render_timestamp_ms(stats: &mut DebugStats, now_ms: u64) {
    stats.render_timestamps_ms.push_back(now_ms);
    while stats
        .render_timestamps_ms
        .front()
        .is_some_and(|timestamp| now_ms.saturating_sub(*timestamp) > 1000)
    {
        let _ = stats.render_timestamps_ms.pop_front();
    }
}

const RENDER_DURATION_WINDOW_SIZE: usize = 64;

fn record_render_duration_ms(stats: &mut DebugStats, duration_ms: u64) {
    stats.render_durations_ms.push_back(duration_ms);
    while stats.render_durations_ms.len() > RENDER_DURATION_WINDOW_SIZE {
        let _ = stats.render_durations_ms.pop_front();
    }
}

fn render_duration_avg_and_p95_ms(stats: &DebugStats) -> (u64, u64) {
    if stats.render_durations_ms.is_empty() {
        return (0, 0);
    }

    let sum = stats
        .render_durations_ms
        .iter()
        .fold(0u64, |acc, value| acc.saturating_add(*value));
    let count = u64::try_from(stats.render_durations_ms.len()).unwrap_or(1);
    let avg = sum / count;

    let mut sorted = stats
        .render_durations_ms
        .iter()
        .copied()
        .collect::<Vec<_>>();
    sorted.sort_unstable();
    let len = sorted.len();
    let index = if len == 1 { 0 } else { ((len - 1) * 95) / 100 };
    let p95 = sorted[index];

    (avg, p95)
}

fn mode_label(mode: &Mode) -> &'static str {
    match mode {
        Mode::Idle => "idle",
        Mode::Running { .. } => "running",
        Mode::Error(_) => "error",
        Mode::Exiting => "exiting",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DebugBand {
    VeryGood,
    Good,
    SoSo,
    Bad,
    VeryBad,
}

fn bright_red_bold(text: &str) -> String {
    ansi_wrap(text, "\x1b[1m\x1b[91m", "\x1b[39m\x1b[22m")
}

fn colorize_band(text: &str, band: DebugBand) -> String {
    match band {
        DebugBand::VeryGood => green(text),
        DebugBand::Good => cyan(text),
        DebugBand::SoSo => yellow(text),
        DebugBand::Bad => red(text),
        DebugBand::VeryBad => bright_red_bold(text),
    }
}

fn debug_band_label(band: DebugBand) -> &'static str {
    match band {
        DebugBand::VeryGood => "VG",
        DebugBand::Good => "G",
        DebugBand::SoSo => "SS",
        DebugBand::Bad => "B",
        DebugBand::VeryBad => "VB",
    }
}

fn band_for_rps(rps: usize) -> DebugBand {
    if rps <= 8 {
        DebugBand::VeryGood
    } else if rps <= 14 {
        DebugBand::Good
    } else if rps <= 22 {
        DebugBand::SoSo
    } else if rps <= 35 {
        DebugBand::Bad
    } else {
        DebugBand::VeryBad
    }
}

fn band_for_frame(frame_lines: usize) -> DebugBand {
    if frame_lines <= 120 {
        DebugBand::VeryGood
    } else if frame_lines <= 260 {
        DebugBand::Good
    } else if frame_lines <= 500 {
        DebugBand::SoSo
    } else if frame_lines <= 900 {
        DebugBand::Bad
    } else {
        DebugBand::VeryBad
    }
}

fn band_for_tr(transcript_lines: usize) -> DebugBand {
    if transcript_lines <= 100 {
        DebugBand::VeryGood
    } else if transcript_lines <= 250 {
        DebugBand::Good
    } else if transcript_lines <= 600 {
        DebugBand::SoSo
    } else if transcript_lines <= 1200 {
        DebugBand::Bad
    } else {
        DebugBand::VeryBad
    }
}

fn band_for_hit(hit_pct: u64) -> DebugBand {
    if hit_pct >= 98 {
        DebugBand::VeryGood
    } else if hit_pct >= 92 {
        DebugBand::Good
    } else if hit_pct >= 80 {
        DebugBand::SoSo
    } else if hit_pct >= 60 {
        DebugBand::Bad
    } else {
        DebugBand::VeryBad
    }
}

fn overall_band(bands: &[DebugBand]) -> DebugBand {
    bands.iter().copied().max().unwrap_or(DebugBand::VeryGood)
}

fn culprit_metric_label<'a>(
    global_band: DebugBand,
    metric_bands: &'a [(&'a str, DebugBand)],
) -> &'a str {
    metric_bands
        .iter()
        .find(|(_, band)| *band == global_band)
        .map(|(metric, _)| *metric)
        .unwrap_or("na")
}

fn truncate_ansi_to_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let bytes = text.as_bytes();
    let mut output = String::new();
    let mut index = 0usize;
    let mut visible = 0usize;
    let mut truncated = false;

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
            output.push_str(std::str::from_utf8(&bytes[start..index]).unwrap_or_default());
            continue;
        }

        let ch = match std::str::from_utf8(&bytes[index..])
            .ok()
            .and_then(|rest| rest.chars().next())
        {
            Some(ch) => ch,
            None => break,
        };

        if visible >= width {
            truncated = true;
            break;
        }

        output.push(ch);
        visible = visible.saturating_add(1);
        index += ch.len_utf8();
    }

    if truncated && output.contains("\x1b[") {
        output.push_str("\x1b[0m");
    }

    output
}

fn render_debug_line(width: usize, stats: &DebugStats) -> String {
    let _ = stats.render_count_total;
    let rps = stats.render_timestamps_ms.len();
    let cache_total = stats.cache_hits.saturating_add(stats.cache_misses);
    let hit_warm = cache_total >= 30;
    let hit_pct = if cache_total == 0 {
        0
    } else {
        stats.cache_hits.saturating_mul(100) / cache_total
    };

    let rps_band = band_for_rps(rps);
    let frame_band = band_for_frame(stats.last_frame_lines);
    let tr_band = band_for_tr(stats.last_transcript_lines);
    let hit_band = hit_warm.then_some(band_for_hit(hit_pct));

    let mut metric_bands = vec![("rps", rps_band), ("frame", frame_band), ("tr", tr_band)];
    if let Some(hit_band) = hit_band {
        metric_bands.push(("hit", hit_band));
    }
    let aggregate_bands = metric_bands
        .iter()
        .map(|(_, band)| *band)
        .collect::<Vec<_>>();
    let global_band = overall_band(&aggregate_bands);
    let culprit = culprit_metric_label(global_band, &metric_bands);

    let prefix = colorize_band(
        &format!("DBG[{}|{}]", debug_band_label(global_band), culprit),
        global_band,
    );
    let rps_token = format!("rps:{}", colorize_band(&rps.to_string(), rps_band));
    let frame_token = format!(
        "frame:{}",
        colorize_band(&stats.last_frame_lines.to_string(), frame_band)
    );
    let tr_token = format!(
        "tr:{}",
        colorize_band(&stats.last_transcript_lines.to_string(), tr_band)
    );
    let hit_token = if let Some(hit_band) = hit_band {
        format!("hit:{}", colorize_band(&format!("{hit_pct}%"), hit_band))
    } else {
        format!("hit:{}", dim("n/a"))
    };
    let (render_avg_ms, render_p95_ms) = render_duration_avg_and_p95_ms(stats);
    let ms_token = format!("ms:{render_avg_ms}/{render_p95_ms}");
    let out_token = format!("out:{}", stats.last_out_bytes);
    let cmd_token = format!("cmd:{}", stats.last_diff_commands);
    let inq_token = format!("inq:{}", stats.last_input_queue_depth);

    let sep = dim(" • ");
    let line = format!(
        "{prefix}{sep}{rps_token}{sep}{frame_token}{sep}{tr_token}{sep}{hit_token}{sep}rev:{}{sep}mode:{}{sep}{ms_token}{sep}{out_token}{sep}{cmd_token}{sep}{inq_token}",
        stats.last_revision,
        mode_label(&stats.last_mode),
        sep = sep
    );

    truncate_ansi_to_width(&line, width)
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
    use std::sync::{Arc, Mutex};

    use tape_tui::{Terminal, TUI};

    use super::*;
    use crate::app::Role;
    use crate::provider::{CancelSignal, RunEvent, RunProvider, RunRequest};

    #[derive(Default)]
    struct NullTerminal;

    impl Terminal for NullTerminal {
        fn start(
            &mut self,
            _on_input: Box<dyn FnMut(String) + Send>,
            _on_resize: Box<dyn FnMut() + Send>,
        ) -> std::io::Result<()> {
            Ok(())
        }

        fn stop(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}

        fn write(&mut self, _data: &str) {}

        fn columns(&self) -> u16 {
            120
        }

        fn rows(&self) -> u16 {
            40
        }
    }

    struct NoopProvider;

    impl RunProvider for NoopProvider {
        fn profile(&self) -> ProviderProfile {
            ProviderProfile {
                provider_id: "test".to_string(),
                model_id: "test-model".to_string(),
                thinking_level: None,
            }
        }

        fn run(
            &self,
            req: RunRequest,
            _cancel: CancelSignal,
            _execute_tool: &mut dyn FnMut(
                crate::provider::ToolCallRequest,
            ) -> crate::provider::ToolResult,
            emit: &mut dyn FnMut(RunEvent),
        ) -> Result<(), String> {
            emit(RunEvent::Started { run_id: req.run_id });
            emit(RunEvent::Finished { run_id: req.run_id });
            Ok(())
        }
    }

    fn healthy_debug_stats() -> DebugStats {
        let mut stats = DebugStats::new();
        stats.render_timestamps_ms = VecDeque::from(vec![1000, 1100, 1200, 1300]);
        stats.render_durations_ms = VecDeque::from(vec![1, 2, 2, 3, 4]);
        stats.last_frame_lines = 80;
        stats.last_transcript_lines = 60;
        stats.cache_hits = 99;
        stats.cache_misses = 1;
        stats.last_revision = 42;
        stats.last_mode = Mode::Idle;
        stats.last_out_bytes = 1200;
        stats.last_diff_commands = 9;
        stats.last_input_queue_depth = 0;
        stats
    }

    fn startup_debug_stats() -> DebugStats {
        let mut stats = DebugStats::new();
        stats.render_timestamps_ms = VecDeque::from(vec![1000]);
        stats.render_durations_ms = VecDeque::from(vec![1]);
        stats.last_frame_lines = 7;
        stats.last_transcript_lines = 0;
        stats.cache_hits = 1;
        stats.cache_misses = 2;
        stats.last_revision = 0;
        stats.last_mode = Mode::Idle;
        stats.last_out_bytes = 80;
        stats.last_diff_commands = 2;
        stats.last_input_queue_depth = 0;
        stats
    }

    #[test]
    fn debug_band_thresholds_are_stable() {
        assert_eq!(band_for_rps(8), DebugBand::VeryGood);
        assert_eq!(band_for_rps(14), DebugBand::Good);
        assert_eq!(band_for_rps(22), DebugBand::SoSo);
        assert_eq!(band_for_rps(35), DebugBand::Bad);
        assert_eq!(band_for_rps(36), DebugBand::VeryBad);

        assert_eq!(band_for_frame(120), DebugBand::VeryGood);
        assert_eq!(band_for_frame(260), DebugBand::Good);
        assert_eq!(band_for_frame(500), DebugBand::SoSo);
        assert_eq!(band_for_frame(900), DebugBand::Bad);
        assert_eq!(band_for_frame(901), DebugBand::VeryBad);

        assert_eq!(band_for_tr(100), DebugBand::VeryGood);
        assert_eq!(band_for_tr(250), DebugBand::Good);
        assert_eq!(band_for_tr(600), DebugBand::SoSo);
        assert_eq!(band_for_tr(1200), DebugBand::Bad);
        assert_eq!(band_for_tr(1201), DebugBand::VeryBad);

        assert_eq!(band_for_hit(98), DebugBand::VeryGood);
        assert_eq!(band_for_hit(92), DebugBand::Good);
        assert_eq!(band_for_hit(80), DebugBand::SoSo);
        assert_eq!(band_for_hit(60), DebugBand::Bad);
        assert_eq!(band_for_hit(59), DebugBand::VeryBad);
    }

    #[test]
    fn debug_overall_band_uses_worst_metric() {
        let result = overall_band(&[
            DebugBand::VeryGood,
            DebugBand::Good,
            DebugBand::SoSo,
            DebugBand::VeryBad,
        ]);
        assert_eq!(result, DebugBand::VeryBad);
    }

    #[test]
    fn culprit_metric_label_reports_first_worst_metric() {
        let culprit = culprit_metric_label(
            DebugBand::Bad,
            &[
                ("rps", DebugBand::SoSo),
                ("frame", DebugBand::Bad),
                ("tr", DebugBand::Bad),
            ],
        );
        assert_eq!(culprit, "frame");
    }

    #[test]
    fn render_debug_line_contains_core_fields() {
        let mut stats = DebugStats::new();
        stats.last_frame_lines = 12;
        stats.last_transcript_lines = 34;
        stats.cache_hits = 3;
        stats.cache_misses = 1;
        stats.last_revision = 7;
        stats.last_mode = Mode::Running { run_id: 42 };
        stats.render_timestamps_ms.push_back(1000);
        stats.render_timestamps_ms.push_back(1100);

        let line = render_debug_line(200, &stats);
        assert!(line.contains("rps:"));
        assert!(line.contains("frame:"));
        assert!(line.contains("tr:"));
        assert!(line.contains("hit:"));
        assert!(line.contains("rev:"));
        assert!(line.contains("mode:"));
        assert!(line.contains("ms:"));
        assert!(line.contains("out:"));
        assert!(line.contains("cmd:"));
        assert!(line.contains("inq:"));
    }

    #[test]
    fn render_duration_avg_and_p95_are_stable() {
        let mut stats = DebugStats::new();
        stats.render_durations_ms = VecDeque::from(vec![1, 2, 3, 10, 20]);
        assert_eq!(render_duration_avg_and_p95_ms(&stats), (7, 10));
    }

    #[test]
    fn render_debug_line_contains_band_prefix() {
        let line = render_debug_line(200, &healthy_debug_stats());
        let plain = strip_ansi(&line);
        assert!(plain.contains("DBG["));
        assert!(plain.contains("|"));
    }

    #[test]
    fn render_debug_line_colorized_tokens_present() {
        let line = render_debug_line(200, &healthy_debug_stats());
        assert!(line.contains("\x1b["));
        assert!(line.contains("rps:"));
        assert!(line.contains("frame:"));
        assert!(line.contains("tr:"));
        assert!(line.contains("hit:"));
    }

    #[test]
    fn render_debug_line_truncation_is_ansi_safe() {
        let line = render_debug_line(10, &healthy_debug_stats());
        assert!(visible_text_width(&line) <= 10);
        assert!(line.ends_with("\x1b[0m") || !line.contains("\x1b["));
    }

    #[test]
    fn idle_good_case_renders_greenish_prefix() {
        let line = render_debug_line(200, &healthy_debug_stats());
        assert!(line.contains("DBG[VG|"));
        assert!(line.contains("\x1b[32mDBG[VG|"));
    }

    #[test]
    fn startup_hit_rate_is_warmup_and_does_not_dominate_overall_band() {
        let line = render_debug_line(300, &startup_debug_stats());
        let plain = strip_ansi(&line);
        assert!(plain.contains("DBG[VG|"));
        assert!(plain.contains("hit:n/a"));
    }

    #[test]
    fn render_debug_line_truncates_to_width() {
        let mut stats = DebugStats::new();
        stats.last_mode = Mode::Error("boom".to_string());
        let line = render_debug_line(12, &stats);
        assert!(visible_text_width(&line) <= 12);
    }

    #[test]
    fn cache_hit_miss_counters_progress() {
        let app = Arc::new(Mutex::new(App::new()));
        let runtime = TUI::new(NullTerminal);
        let host = RuntimeController::new(
            Arc::clone(&app),
            runtime.runtime_handle(),
            Arc::new(NoopProvider),
        );
        let mut component = AppComponent::new(
            app,
            host,
            ProviderProfile {
                provider_id: "test".to_string(),
                model_id: "test-model".to_string(),
                thinking_level: None,
            },
        );

        let _ = component.render(80);
        assert_eq!(component.debug_stats.cache_hits, 0);
        assert_eq!(component.debug_stats.cache_misses, 1);

        let _ = component.render(80);
        assert_eq!(component.debug_stats.cache_hits, 1);
        assert_eq!(component.debug_stats.cache_misses, 1);
    }

    #[test]
    fn rolling_rps_window_evicts_old_samples() {
        let mut stats = DebugStats::new();
        record_render_timestamp_ms(&mut stats, 1000);
        record_render_timestamp_ms(&mut stats, 1500);
        record_render_timestamp_ms(&mut stats, 2000);

        assert_eq!(stats.render_timestamps_ms.len(), 3);

        record_render_timestamp_ms(&mut stats, 2501);

        assert_eq!(stats.render_timestamps_ms.len(), 2);
        assert_eq!(stats.render_timestamps_ms[0], 2000);
        assert_eq!(stats.render_timestamps_ms[1], 2501);
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
