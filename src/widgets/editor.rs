//! Editor widget (Phase 25).

use std::cmp::{max, min};
use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;

use crate::core::component::{Component, Focusable};
use crate::core::editor_component::EditorComponent;
use crate::core::keybindings::{get_editor_keybindings, EditorAction};
use crate::render::utils::{grapheme_segments, is_punctuation_char, is_whitespace_char};
use crate::render::width::visible_width;
use crate::widgets::select_list::SelectListTheme;

const CURSOR_MARKER: &str = "\x1b_pi:c\x07";
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

const MAX_PASTE_LINES: usize = 10;
const MAX_PASTE_CHARS: usize = 1000;

/// Represents a chunk of text for word-wrap layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    pub text: String,
    pub start_index: usize,
    pub end_index: usize,
}

/// Split a line into word-wrapped chunks.
pub fn word_wrap_line(line: &str, max_width: usize) -> Vec<TextChunk> {
    if line.is_empty() || max_width == 0 {
        return vec![TextChunk {
            text: String::new(),
            start_index: 0,
            end_index: 0,
        }];
    }

    let line_width = visible_width(line);
    if line_width <= max_width {
        return vec![TextChunk {
            text: line.to_string(),
            start_index: 0,
            end_index: line.len(),
        }];
    }

    let mut chunks = Vec::new();
    let segments: Vec<(usize, &str)> = line.grapheme_indices(true).collect();

    let mut current_width = 0usize;
    let mut chunk_start = 0usize;
    let mut wrap_opp_index: Option<usize> = None;
    let mut wrap_opp_width = 0usize;

    for (idx, (char_index, grapheme)) in segments.iter().enumerate() {
        let g_width = visible_width(grapheme);
        let is_ws = is_whitespace_segment(grapheme);

        if current_width + g_width > max_width {
            if let Some(opp) = wrap_opp_index {
                chunks.push(TextChunk {
                    text: line[chunk_start..opp].to_string(),
                    start_index: chunk_start,
                    end_index: opp,
                });
                chunk_start = opp;
                current_width = current_width.saturating_sub(wrap_opp_width);
            } else if chunk_start < *char_index {
                chunks.push(TextChunk {
                    text: line[chunk_start..*char_index].to_string(),
                    start_index: chunk_start,
                    end_index: *char_index,
                });
                chunk_start = *char_index;
                current_width = 0;
            }
            wrap_opp_index = None;
        }

        current_width = current_width.saturating_add(g_width);

        if is_ws {
            if let Some((next_index, next_segment)) = segments.get(idx + 1) {
                if !is_whitespace_segment(next_segment) {
                    wrap_opp_index = Some(*next_index);
                    wrap_opp_width = current_width;
                }
            }
        }
    }

    chunks.push(TextChunk {
        text: line[chunk_start..].to_string(),
        start_index: chunk_start,
        end_index: line.len(),
    });

    chunks
}

#[derive(Debug, Clone)]
struct EditorState {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

#[derive(Debug, Clone)]
struct LayoutLine {
    text: String,
    has_cursor: bool,
    cursor_pos: Option<usize>,
}

pub struct EditorTheme {
    pub border_color: Box<dyn Fn(&str) -> String>,
    pub select_list: SelectListTheme,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EditorOptions {
    pub padding_x: Option<usize>,
    pub autocomplete_max_visible: Option<usize>,
}

enum JumpMode {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

pub struct Editor {
    state: EditorState,
    focused: bool,
    #[allow(dead_code)]
    select_list_theme: SelectListTheme,
    padding_x: usize,
    #[allow(dead_code)]
    autocomplete_max_visible: usize,
    last_width: usize,
    scroll_offset: usize,
    border_color: Box<dyn Fn(&str) -> String>,
    terminal_rows: usize,
    preferred_visual_col: Option<usize>,
    jump_mode: Option<JumpMode>,
    disable_submit: bool,
    pastes: HashMap<u32, String>,
    paste_counter: u32,
    paste_buffer: String,
    is_in_paste: bool,
    kill_ring: Vec<String>,
    last_action: Option<LastAction>,
    undo_stack: Vec<EditorState>,
    on_submit: Option<Box<dyn FnMut(String)>>,
    on_change: Option<Box<dyn FnMut(String)>>,
    history: Vec<String>,
    history_index: isize,
}

impl Editor {
    pub fn new(theme: EditorTheme, options: EditorOptions) -> Self {
        let padding_x = options.padding_x.unwrap_or(0);
        let max_visible = options.autocomplete_max_visible.unwrap_or(5);
        let autocomplete_max_visible = max(3, min(20, max_visible));
        let border_color = theme.border_color;
        let select_list_theme = theme.select_list;
        Self {
            state: EditorState {
                lines: vec![String::new()],
                cursor_line: 0,
                cursor_col: 0,
            },
            focused: false,
            select_list_theme,
            padding_x,
            autocomplete_max_visible,
            last_width: 80,
            scroll_offset: 0,
            border_color,
            terminal_rows: 0,
            preferred_visual_col: None,
            jump_mode: None,
            disable_submit: false,
            pastes: HashMap::new(),
            paste_counter: 0,
            paste_buffer: String::new(),
            is_in_paste: false,
            kill_ring: Vec::new(),
            last_action: None,
            undo_stack: Vec::new(),
            on_submit: None,
            on_change: None,
            history: Vec::new(),
            history_index: -1,
        }
    }

    pub fn set_terminal_rows(&mut self, rows: usize) {
        self.terminal_rows = rows;
    }

    pub fn get_lines(&self) -> Vec<String> {
        self.state.lines.clone()
    }

    pub fn get_text(&self) -> String {
        self.state.lines.join("\n")
    }

    pub fn get_expanded_text(&self) -> String {
        let text = self.get_text();
        self.replace_paste_markers(&text)
    }

    pub fn get_cursor(&self) -> (usize, usize) {
        (self.state.cursor_line, self.state.cursor_col)
    }

    pub fn set_text(&mut self, text: &str) {
        self.last_action = None;
        self.history_index = -1;
        if self.get_text() != text {
            self.push_undo_snapshot();
        }
        self.set_text_internal(text);
    }

    pub fn insert_text_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.push_undo_snapshot();
        self.last_action = None;
        self.history_index = -1;
        self.insert_text_at_cursor_internal(text);
    }

    pub fn set_padding_x(&mut self, padding: usize) {
        self.padding_x = padding;
    }

    pub fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        self.autocomplete_max_visible = max(3, min(20, max_visible));
    }

    pub fn set_border_color(&mut self, border_color: Box<dyn Fn(&str) -> String>) {
        self.border_color = border_color;
    }

    pub fn set_on_submit(&mut self, handler: Option<Box<dyn FnMut(String)>>) {
        self.on_submit = handler;
    }

    pub fn set_on_change(&mut self, handler: Option<Box<dyn FnMut(String)>>) {
        self.on_change = handler;
    }

    pub fn set_disable_submit(&mut self, disabled: bool) {
        self.disable_submit = disabled;
    }

    fn emit_change(&mut self) {
        if self.on_change.is_some() {
            let text = self.get_text();
            if let Some(handler) = self.on_change.as_mut() {
                handler(text);
            }
        }
    }

    fn clamp_cursor(&mut self) {
        if self.state.lines.is_empty() {
            self.state.lines.push(String::new());
            self.state.cursor_line = 0;
            self.state.cursor_col = 0;
            return;
        }
        if self.state.cursor_line >= self.state.lines.len() {
            self.state.cursor_line = self.state.lines.len().saturating_sub(1);
        }
        let line_len = self
            .state
            .lines
            .get(self.state.cursor_line)
            .map(|line| line.len())
            .unwrap_or(0);
        if self.state.cursor_col > line_len {
            self.state.cursor_col = line_len;
        }
        if let Some(line) = self.state.lines.get(self.state.cursor_line) {
            while self.state.cursor_col > 0 && !line.is_char_boundary(self.state.cursor_col) {
                self.state.cursor_col = self.state.cursor_col.saturating_sub(1);
            }
        }
    }

    fn insert_text_at_cursor_internal(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let inserted_lines: Vec<&str> = normalized.split('\n').collect();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let before_cursor = &current_line[..self.state.cursor_col];
        let after_cursor = &current_line[self.state.cursor_col..];

        if inserted_lines.len() == 1 {
            self.state.lines[self.state.cursor_line] =
                format!("{before_cursor}{normalized}{after_cursor}");
            self.set_cursor_col(self.state.cursor_col + normalized.len());
        } else {
            let mut next_lines = Vec::new();
            next_lines.extend_from_slice(&self.state.lines[..self.state.cursor_line]);
            next_lines.push(format!("{before_cursor}{}", inserted_lines[0]));

            if inserted_lines.len() > 2 {
                for mid in &inserted_lines[1..inserted_lines.len() - 1] {
                    next_lines.push((*mid).to_string());
                }
            }

            let last_inserted = inserted_lines.last().copied().unwrap_or("");
            next_lines.push(format!("{last_inserted}{after_cursor}"));
            next_lines.extend_from_slice(&self.state.lines[self.state.cursor_line + 1..]);

            self.state.lines = next_lines;
            self.state.cursor_line = self.state.cursor_line.saturating_add(inserted_lines.len() - 1);
            self.set_cursor_col(last_inserted.len());
        }

        self.emit_change();
    }

    fn insert_character(&mut self, ch: &str, skip_undo_coalescing: bool) {
        if ch.is_empty() {
            return;
        }

        self.history_index = -1;

        if !skip_undo_coalescing {
            if ch.chars().any(is_whitespace_char) || self.last_action != Some(LastAction::TypeWord) {
                self.push_undo_snapshot();
            }
            self.last_action = Some(LastAction::TypeWord);
        }

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let before = &current_line[..self.state.cursor_col];
        let after = &current_line[self.state.cursor_col..];
        self.state.lines[self.state.cursor_line] = format!("{before}{ch}{after}");
        self.set_cursor_col(self.state.cursor_col + ch.len());

        self.emit_change();
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.history_index = -1;
        self.last_action = None;
        self.push_undo_snapshot();

        let cleaned = pasted_text.replace("\r\n", "\n").replace('\r', "\n");
        let tab_expanded = cleaned.replace('\t', "    ");
        let mut filtered: String = tab_expanded
            .chars()
            .filter(|ch| *ch == '\n' || (*ch as u32) >= 32)
            .collect();

        if filtered.starts_with('/') || filtered.starts_with('~') || filtered.starts_with('.') {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .map(String::as_str)
                .unwrap_or("");
            let char_before = if self.state.cursor_col > 0 {
                current_line[..self.state.cursor_col]
                    .chars()
                    .last()
            } else {
                None
            };
            if let Some(ch) = char_before {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    filtered = format!(" {filtered}");
                }
            }
        }

        let pasted_lines_count = filtered.split('\n').count();
        let total_chars = filtered.encode_utf16().count();

        if pasted_lines_count > MAX_PASTE_LINES || total_chars > MAX_PASTE_CHARS {
            self.paste_counter = self.paste_counter.saturating_add(1);
            let paste_id = self.paste_counter;
            self.pastes.insert(paste_id, filtered);

            let marker = if pasted_lines_count > MAX_PASTE_LINES {
                format!("[paste #{paste_id} +{} lines]", pasted_lines_count)
            } else {
                format!("[paste #{paste_id} {total_chars} chars]")
            };
            self.insert_text_at_cursor_internal(&marker);
            return;
        }

        if pasted_lines_count == 1 {
            for ch in filtered.chars() {
                let ch = ch.to_string();
                self.insert_character(&ch, true);
            }
            return;
        }

        self.insert_text_at_cursor_internal(&filtered);
    }

    fn add_new_line(&mut self) {
        self.history_index = -1;
        self.last_action = None;
        self.push_undo_snapshot();

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();
        let before = current_line[..self.state.cursor_col].to_string();
        let after = current_line[self.state.cursor_col..].to_string();

        self.state.lines[self.state.cursor_line] = before;
        self.state
            .lines
            .insert(self.state.cursor_line + 1, after);
        self.state.cursor_line += 1;
        self.set_cursor_col(0);

        self.emit_change();
    }

    fn should_submit_on_backslash_enter(
        &self,
        data: &str,
        keybindings: &crate::core::keybindings::EditorKeybindingsManager,
    ) -> bool {
        if self.disable_submit {
            return false;
        }
        if !crate::core::input::matches_key(data, "enter") {
            return false;
        }
        let submit_keys = keybindings.get_keys(EditorAction::Submit);
        let has_shift_enter = submit_keys
            .iter()
            .any(|key| key == "shift+enter" || key == "shift+return");
        if !has_shift_enter {
            return false;
        }
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .map(String::as_str)
            .unwrap_or("");
        self.state.cursor_col > 0 && current_line[..self.state.cursor_col].ends_with('\\')
    }

    fn submit_value(&mut self) {
        let text = self.get_text();
        let mut result = text.trim().to_string();
        result = self.replace_paste_markers(&result);

        self.state = EditorState {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
        };
        self.pastes.clear();
        self.paste_counter = 0;
        self.history_index = -1;
        self.scroll_offset = 0;
        self.undo_stack.clear();
        self.last_action = None;

        self.emit_change();
        if let Some(handler) = self.on_submit.as_mut() {
            handler(result);
        }
    }

    fn handle_backspace(&mut self) {
        self.history_index = -1;
        self.last_action = None;

        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();
            let line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let before_cursor = &line[..self.state.cursor_col];
            let mut graphemes: Vec<&str> = grapheme_segments(before_cursor).collect();
            let grapheme_len = graphemes.pop().map(|seg| seg.len()).unwrap_or(1);
            let start = self.state.cursor_col.saturating_sub(grapheme_len);
            let after = &line[self.state.cursor_col..];
            self.state.lines[self.state.cursor_line] =
                format!("{}{}", &line[..start], after);
            self.set_cursor_col(start);
        } else if self.state.cursor_line > 0 {
            self.push_undo_snapshot();
            let current = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let prev_index = self.state.cursor_line - 1;
            let previous = self
                .state
                .lines
                .get(prev_index)
                .cloned()
                .unwrap_or_default();
            let new_line = format!("{previous}{current}");
            self.state.lines[prev_index] = new_line;
            self.state.lines.remove(self.state.cursor_line);
            self.state.cursor_line = prev_index;
            self.set_cursor_col(previous.len());
        }

        self.emit_change();
    }

    fn handle_forward_delete(&mut self) {
        self.history_index = -1;
        self.last_action = None;

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col < current_line.len() {
            self.push_undo_snapshot();
            let after_cursor = &current_line[self.state.cursor_col..];
            let mut graphemes = grapheme_segments(after_cursor);
            let first = graphemes.next().unwrap_or("");
            let end = self.state.cursor_col.saturating_add(first.len());
            self.state.lines[self.state.cursor_line] =
                format!("{}{}", &current_line[..self.state.cursor_col], &current_line[end..]);
        } else if self.state.cursor_line + 1 < self.state.lines.len() {
            self.push_undo_snapshot();
            let next_line = self
                .state
                .lines
                .get(self.state.cursor_line + 1)
                .cloned()
                .unwrap_or_default();
            self.state.lines[self.state.cursor_line] =
                format!("{current_line}{next_line}");
            self.state.lines.remove(self.state.cursor_line + 1);
        }

        self.emit_change();
    }

    fn delete_to_start_of_line(&mut self) {
        self.history_index = -1;

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();
            let deleted = current_line[..self.state.cursor_col].to_string();
            self.add_to_kill_ring(&deleted, true);
            self.last_action = Some(LastAction::Kill);
            self.state.lines[self.state.cursor_line] =
                current_line[self.state.cursor_col..].to_string();
            self.set_cursor_col(0);
        } else if self.state.cursor_line > 0 {
            self.push_undo_snapshot();
            self.add_to_kill_ring("\n", true);
            self.last_action = Some(LastAction::Kill);

            let prev_index = self.state.cursor_line - 1;
            let previous = self
                .state
                .lines
                .get(prev_index)
                .cloned()
                .unwrap_or_default();
            let merged = format!("{previous}{current_line}");
            self.state.lines[prev_index] = merged;
            self.state.lines.remove(self.state.cursor_line);
            self.state.cursor_line = prev_index;
            self.set_cursor_col(previous.len());
        }

        self.emit_change();
    }

    fn delete_to_end_of_line(&mut self) {
        self.history_index = -1;

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col < current_line.len() {
            self.push_undo_snapshot();
            let deleted = current_line[self.state.cursor_col..].to_string();
            self.add_to_kill_ring(&deleted, false);
            self.last_action = Some(LastAction::Kill);
            self.state.lines[self.state.cursor_line] =
                current_line[..self.state.cursor_col].to_string();
        } else if self.state.cursor_line + 1 < self.state.lines.len() {
            self.push_undo_snapshot();
            self.add_to_kill_ring("\n", false);
            self.last_action = Some(LastAction::Kill);

            let next_line = self
                .state
                .lines
                .get(self.state.cursor_line + 1)
                .cloned()
                .unwrap_or_default();
            self.state.lines[self.state.cursor_line] =
                format!("{current_line}{next_line}");
            self.state.lines.remove(self.state.cursor_line + 1);
        }

        self.emit_change();
    }

    fn delete_word_backwards(&mut self) {
        self.history_index = -1;

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.push_undo_snapshot();
                self.add_to_kill_ring("\n", true);
                self.last_action = Some(LastAction::Kill);

                let prev_index = self.state.cursor_line - 1;
                let previous = self
                    .state
                    .lines
                    .get(prev_index)
                    .cloned()
                    .unwrap_or_default();
                self.state.lines[prev_index] = format!("{previous}{current_line}");
                self.state.lines.remove(self.state.cursor_line);
                self.state.cursor_line = prev_index;
                self.set_cursor_col(previous.len());
            }
        } else {
            self.push_undo_snapshot();
            let was_kill = self.last_action == Some(LastAction::Kill);
            let old_col = self.state.cursor_col;
            self.move_word_backwards();
            let delete_from = self.state.cursor_col;
            self.set_cursor_col(old_col);
            self.last_action = if was_kill {
                Some(LastAction::Kill)
            } else {
                None
            };
            let deleted = current_line[delete_from..old_col].to_string();
            self.add_to_kill_ring(&deleted, true);
            self.last_action = Some(LastAction::Kill);
            self.state.lines[self.state.cursor_line] =
                format!("{}{}", &current_line[..delete_from], &current_line[old_col..]);
            self.set_cursor_col(delete_from);
        }

        self.emit_change();
    }

    fn delete_word_forwards(&mut self) {
        self.history_index = -1;

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .cloned()
            .unwrap_or_default();

        if self.state.cursor_col >= current_line.len() {
            if self.state.cursor_line + 1 < self.state.lines.len() {
                self.push_undo_snapshot();
                self.add_to_kill_ring("\n", false);
                self.last_action = Some(LastAction::Kill);

                let next_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line + 1)
                    .cloned()
                    .unwrap_or_default();
                self.state.lines[self.state.cursor_line] =
                    format!("{current_line}{next_line}");
                self.state.lines.remove(self.state.cursor_line + 1);
            }
        } else {
            self.push_undo_snapshot();
            let was_kill = self.last_action == Some(LastAction::Kill);
            let old_col = self.state.cursor_col;
            self.move_word_forwards();
            let delete_to = self.state.cursor_col;
            self.set_cursor_col(old_col);
            self.last_action = if was_kill {
                Some(LastAction::Kill)
            } else {
                None
            };
            let deleted = current_line[old_col..delete_to].to_string();
            self.add_to_kill_ring(&deleted, false);
            self.last_action = Some(LastAction::Kill);
            self.state.lines[self.state.cursor_line] =
                format!("{}{}", &current_line[..old_col], &current_line[delete_to..]);
        }

        self.emit_change();
    }

    fn yank(&mut self) {
        if self.kill_ring.is_empty() {
            return;
        }
        self.push_undo_snapshot();

        let text = self.kill_ring.last().cloned().unwrap_or_default();
        self.insert_yanked_text(&text);
        self.last_action = Some(LastAction::Yank);
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }
        self.push_undo_snapshot();
        self.delete_yanked_text();

        if let Some(last) = self.kill_ring.pop() {
            self.kill_ring.insert(0, last);
        }
        let text = self.kill_ring.last().cloned().unwrap_or_default();
        self.insert_yanked_text(&text);
        self.last_action = Some(LastAction::Yank);
    }

    fn insert_yanked_text(&mut self, text: &str) {
        self.history_index = -1;
        let lines: Vec<&str> = text.split('\n').collect();

        if lines.len() == 1 {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let before = &current_line[..self.state.cursor_col];
            let after = &current_line[self.state.cursor_col..];
            self.state.lines[self.state.cursor_line] = format!("{before}{text}{after}");
            self.set_cursor_col(self.state.cursor_col + text.len());
        } else {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let before = &current_line[..self.state.cursor_col];
            let after = &current_line[self.state.cursor_col..];

            self.state.lines[self.state.cursor_line] = format!("{before}{}", lines[0]);
            for (idx, line) in lines.iter().enumerate().skip(1).take(lines.len() - 2) {
                self.state
                    .lines
                    .insert(self.state.cursor_line + idx, (*line).to_string());
            }

            let last_idx = self.state.cursor_line + lines.len() - 1;
            self.state
                .lines
                .insert(last_idx, format!("{}{}", lines[lines.len() - 1], after));
            self.state.cursor_line = last_idx;
            self.set_cursor_col(lines[lines.len() - 1].len());
        }

        self.emit_change();
    }

    fn delete_yanked_text(&mut self) {
        let yanked_text = self.kill_ring.last().cloned().unwrap_or_default();
        if yanked_text.is_empty() {
            return;
        }
        let yank_lines: Vec<&str> = yanked_text.split('\n').collect();

        if yank_lines.len() == 1 {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .cloned()
                .unwrap_or_default();
            let delete_len = yanked_text.len();
            let start = self.state.cursor_col.saturating_sub(delete_len);
            let before = &current_line[..start];
            let after = &current_line[self.state.cursor_col..];
            self.state.lines[self.state.cursor_line] = format!("{before}{after}");
            self.set_cursor_col(start);
        } else {
            let start_line = self
                .state
                .cursor_line
                .saturating_sub(yank_lines.len().saturating_sub(1));
            let line_at_start = self
                .state
                .lines
                .get(start_line)
                .cloned()
                .unwrap_or_default();
            let start_col = line_at_start
                .len()
                .saturating_sub(yank_lines[0].len());
            let after_cursor = self
                .state
                .lines
                .get(self.state.cursor_line)
                .map(|line| line[self.state.cursor_col..].to_string())
                .unwrap_or_default();
            let before_yank = line_at_start[..start_col].to_string();

            self.state
                .lines
                .splice(start_line..=self.state.cursor_line, [format!("{before_yank}{after_cursor}")]);
            self.state.cursor_line = start_line;
            self.set_cursor_col(start_col);
        }

        self.emit_change();
    }

    fn add_to_kill_ring(&mut self, text: &str, prepend: bool) {
        if text.is_empty() {
            return;
        }
        if self.last_action == Some(LastAction::Kill) && !self.kill_ring.is_empty() {
            if let Some(last) = self.kill_ring.pop() {
                if prepend {
                    self.kill_ring.push(format!("{text}{last}"));
                } else {
                    self.kill_ring.push(format!("{last}{text}"));
                }
            }
        } else {
            self.kill_ring.push(text.to_string());
        }
    }

    fn capture_undo_snapshot(&self) -> EditorState {
        self.state.clone()
    }

    fn restore_undo_snapshot(&mut self, snapshot: EditorState) {
        self.state = snapshot;
    }

    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(self.capture_undo_snapshot());
    }

    fn undo(&mut self) {
        self.history_index = -1;
        if self.undo_stack.is_empty() {
            return;
        }
        if let Some(snapshot) = self.undo_stack.pop() {
            self.restore_undo_snapshot(snapshot);
        }
        self.last_action = None;
        self.preferred_visual_col = None;
        self.emit_change();
    }

    fn replace_paste_markers(&self, input: &str) -> String {
        let bytes = input.as_bytes();
        let mut result = String::new();
        let mut idx = 0usize;
        while idx < bytes.len() {
            if bytes[idx..].starts_with(b"[paste #") {
                let start = idx;
                let mut cursor = idx + b"[paste #".len();
                let mut paste_id: u32 = 0;
                let mut has_id = false;
                while cursor < bytes.len() {
                    let ch = bytes[cursor];
                    if ch.is_ascii_digit() {
                        paste_id = paste_id.saturating_mul(10).saturating_add((ch - b'0') as u32);
                        has_id = true;
                        cursor += 1;
                    } else {
                        break;
                    }
                }

                if !has_id {
                    result.push_str("[paste #");
                    idx = start + b"[paste #".len();
                    continue;
                }

                if let Some(close_rel) = bytes[cursor..].iter().position(|b| *b == b']') {
                    let end = cursor + close_rel;
                    if let Some(content) = self.pastes.get(&paste_id) {
                        result.push_str(content);
                    } else {
                        result.push_str(&input[start..=end]);
                    }
                    idx = end + 1;
                    continue;
                }
            }
            let ch = input[idx..].chars().next().unwrap();
            result.push(ch);
            idx += ch.len_utf8();
        }
        result
    }

    fn layout_text(&self, content_width: usize) -> Vec<LayoutLine> {
        let mut layout_lines = Vec::new();

        if self.state.lines.is_empty()
            || (self.state.lines.len() == 1 && self.state.lines[0].is_empty())
        {
            layout_lines.push(LayoutLine {
                text: String::new(),
                has_cursor: true,
                cursor_pos: Some(0),
            });
            return layout_lines;
        }

        for (line_idx, line) in self.state.lines.iter().enumerate() {
            let is_current = line_idx == self.state.cursor_line;
            let line_visible_width = visible_width(line);

            if line_visible_width <= content_width {
                if is_current {
                    layout_lines.push(LayoutLine {
                        text: line.clone(),
                        has_cursor: true,
                        cursor_pos: Some(self.state.cursor_col),
                    });
                } else {
                    layout_lines.push(LayoutLine {
                        text: line.clone(),
                        has_cursor: false,
                        cursor_pos: None,
                    });
                }
            } else {
                let chunks = word_wrap_line(line, content_width);
                for (chunk_index, chunk) in chunks.iter().enumerate() {
                    let is_last_chunk = chunk_index + 1 == chunks.len();
                    let mut has_cursor = false;
                    let mut adjusted_cursor = 0usize;

                    if is_current {
                        if is_last_chunk {
                            has_cursor = self.state.cursor_col >= chunk.start_index;
                            adjusted_cursor = self.state.cursor_col.saturating_sub(chunk.start_index);
                        } else if self.state.cursor_col >= chunk.start_index
                            && self.state.cursor_col < chunk.end_index
                        {
                            has_cursor = true;
                            adjusted_cursor = self.state.cursor_col.saturating_sub(chunk.start_index);
                            if adjusted_cursor > chunk.text.len() {
                                adjusted_cursor = chunk.text.len();
                            }
                        }
                    }

                    if has_cursor {
                        layout_lines.push(LayoutLine {
                            text: chunk.text.clone(),
                            has_cursor: true,
                            cursor_pos: Some(adjusted_cursor),
                        });
                    } else {
                        layout_lines.push(LayoutLine {
                            text: chunk.text.clone(),
                            has_cursor: false,
                            cursor_pos: None,
                        });
                    }
                }
            }
        }

        layout_lines
    }

    fn build_visual_line_map(&self, width: usize) -> Vec<VisualLine> {
        let mut visual_lines = Vec::new();

        for (idx, line) in self.state.lines.iter().enumerate() {
            let line_width = visible_width(line);
            if line.is_empty() {
                visual_lines.push(VisualLine {
                    logical_line: idx,
                    start_col: 0,
                    length: 0,
                });
            } else if line_width <= width {
                visual_lines.push(VisualLine {
                    logical_line: idx,
                    start_col: 0,
                    length: line.len(),
                });
            } else {
                let chunks = word_wrap_line(line, width);
                for chunk in chunks {
                    visual_lines.push(VisualLine {
                        logical_line: idx,
                        start_col: chunk.start_index,
                        length: chunk.end_index.saturating_sub(chunk.start_index),
                    });
                }
            }
        }

        visual_lines
    }

    fn find_current_visual_line(&self, visual_lines: &[VisualLine]) -> usize {
        for (idx, line) in visual_lines.iter().enumerate() {
            if line.logical_line == self.state.cursor_line {
                let col_in_segment = self.state.cursor_col.saturating_sub(line.start_col);
                let is_last_segment = idx + 1 == visual_lines.len()
                    || visual_lines[idx + 1].logical_line != line.logical_line;
                if col_in_segment < line.length || (is_last_segment && col_in_segment <= line.length) {
                    return idx;
                }
            }
        }
        visual_lines.len().saturating_sub(1)
    }

    fn move_cursor(&mut self, delta_line: isize, delta_col: isize) {
        self.last_action = None;
        let visual_lines = self.build_visual_line_map(self.last_width);
        let current_visual_line = self.find_current_visual_line(&visual_lines);

        if delta_line != 0 {
            let delta = if delta_line < 0 {
                (-delta_line) as usize
            } else {
                delta_line as usize
            };
            let target_visual = if delta_line.is_negative() {
                current_visual_line.saturating_sub(delta)
            } else {
                min(visual_lines.len().saturating_sub(1), current_visual_line.saturating_add(delta))
            };
            if target_visual < visual_lines.len() {
                self.move_to_visual_line(&visual_lines, current_visual_line, target_visual);
            }
        }

        if delta_col != 0 {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .map(String::as_str)
                .unwrap_or("");

            if delta_col > 0 {
                if self.state.cursor_col < current_line.len() {
                    let after_cursor = &current_line[self.state.cursor_col..];
                    let mut graphemes = grapheme_segments(after_cursor);
                    if let Some(first) = graphemes.next() {
                        self.set_cursor_col(self.state.cursor_col + first.len());
                    } else {
                        self.set_cursor_col(self.state.cursor_col + 1);
                    }
                } else if self.state.cursor_line + 1 < self.state.lines.len() {
                    self.state.cursor_line += 1;
                    self.set_cursor_col(0);
                } else if let Some(current_vl) = visual_lines.get(current_visual_line) {
                    self.preferred_visual_col = Some(self.state.cursor_col.saturating_sub(current_vl.start_col));
                }
            } else if self.state.cursor_col > 0 {
                let before_cursor = &current_line[..self.state.cursor_col];
                let mut graphemes: Vec<&str> = grapheme_segments(before_cursor).collect();
                if let Some(last) = graphemes.pop() {
                    self.set_cursor_col(self.state.cursor_col.saturating_sub(last.len()));
                } else {
                    self.set_cursor_col(self.state.cursor_col.saturating_sub(1));
                }
            } else if self.state.cursor_line > 0 {
                self.state.cursor_line = self.state.cursor_line.saturating_sub(1);
                let prev_line = self.state.lines[self.state.cursor_line].as_str();
                self.set_cursor_col(prev_line.len());
            }
        }
    }

    fn move_to_visual_line(
        &mut self,
        visual_lines: &[VisualLine],
        current_visual_line: usize,
        target_visual_line: usize,
    ) {
        let Some(current_vl) = visual_lines.get(current_visual_line) else {
            return;
        };
        let Some(target_vl) = visual_lines.get(target_visual_line) else {
            return;
        };

        let current_visual_col = self.state.cursor_col.saturating_sub(current_vl.start_col);

        let is_last_source = current_visual_line + 1 >= visual_lines.len()
            || visual_lines[current_visual_line + 1].logical_line != current_vl.logical_line;
        let source_max = if is_last_source {
            current_vl.length
        } else {
            current_vl.length.saturating_sub(1)
        };

        let is_last_target = target_visual_line + 1 >= visual_lines.len()
            || visual_lines[target_visual_line + 1].logical_line != target_vl.logical_line;
        let target_max = if is_last_target {
            target_vl.length
        } else {
            target_vl.length.saturating_sub(1)
        };

        let move_col = self.compute_vertical_move_column(current_visual_col, source_max, target_max);
        self.state.cursor_line = target_vl.logical_line;
        let target_col = target_vl.start_col.saturating_add(move_col);
        let line_len = self
            .state
            .lines
            .get(self.state.cursor_line)
            .map(|line| line.len())
            .unwrap_or(0);
        self.state.cursor_col = min(target_col, line_len);
    }

    fn compute_vertical_move_column(
        &mut self,
        current_visual_col: usize,
        source_max: usize,
        target_max: usize,
    ) -> usize {
        let has_preferred = self.preferred_visual_col.is_some();
        let cursor_in_middle = current_visual_col < source_max;
        let target_too_short = target_max < current_visual_col;

        if !has_preferred || cursor_in_middle {
            if target_too_short {
                self.preferred_visual_col = Some(current_visual_col);
                return target_max;
            }
            self.preferred_visual_col = None;
            return current_visual_col;
        }

        let preferred = self.preferred_visual_col.unwrap_or(0);
        let target_cant_fit = target_max < preferred;
        if target_too_short || target_cant_fit {
            return target_max;
        }

        self.preferred_visual_col = None;
        preferred
    }

    fn move_to_line_start(&mut self) {
        self.last_action = None;
        self.set_cursor_col(0);
    }

    fn move_to_line_end(&mut self) {
        self.last_action = None;
        if let Some(line) = self.state.lines.get(self.state.cursor_line) {
            self.set_cursor_col(line.len());
        } else {
            self.set_cursor_col(0);
        }
    }

    fn move_word_backwards(&mut self) {
        self.last_action = None;
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .map(String::as_str)
            .unwrap_or("");

        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.state.cursor_line = self.state.cursor_line.saturating_sub(1);
                let prev_line = self.state.lines[self.state.cursor_line].as_str();
                self.set_cursor_col(prev_line.len());
            }
            return;
        }

        let before_cursor = &current_line[..self.state.cursor_col];
        let mut graphemes: Vec<&str> = grapheme_segments(before_cursor).collect();
        let mut new_col = self.state.cursor_col;

        while let Some(last) = graphemes.last() {
            if is_whitespace_segment(last) {
                new_col = new_col.saturating_sub(last.len());
                graphemes.pop();
            } else {
                break;
            }
        }

        if let Some(last) = graphemes.last() {
            if is_punctuation_segment(last) {
                while let Some(last) = graphemes.last() {
                    if is_punctuation_segment(last) {
                        new_col = new_col.saturating_sub(last.len());
                        graphemes.pop();
                    } else {
                        break;
                    }
                }
            } else {
                while let Some(last) = graphemes.last() {
                    if !is_whitespace_segment(last) && !is_punctuation_segment(last) {
                        new_col = new_col.saturating_sub(last.len());
                        graphemes.pop();
                    } else {
                        break;
                    }
                }
            }
        }

        self.set_cursor_col(new_col);
    }

    fn move_word_forwards(&mut self) {
        self.last_action = None;
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .map(String::as_str)
            .unwrap_or("");

        if self.state.cursor_col >= current_line.len() {
            if self.state.cursor_line + 1 < self.state.lines.len() {
                self.state.cursor_line += 1;
                self.set_cursor_col(0);
            }
            return;
        }

        let after_cursor = &current_line[self.state.cursor_col..];
        let mut iter = grapheme_segments(after_cursor);
        let mut next = iter.next();
        let mut new_col = self.state.cursor_col;

        while let Some(seg) = next {
            if is_whitespace_segment(seg) {
                new_col += seg.len();
                next = iter.next();
            } else {
                break;
            }
        }

        if let Some(seg) = next {
            if is_punctuation_segment(seg) {
                let mut current = Some(seg);
                while let Some(seg) = current {
                    if is_punctuation_segment(seg) {
                        new_col += seg.len();
                        current = iter.next();
                    } else {
                        break;
                    }
                }
            } else {
                let mut current = Some(seg);
                while let Some(seg) = current {
                    if !is_whitespace_segment(seg) && !is_punctuation_segment(seg) {
                        new_col += seg.len();
                        current = iter.next();
                    } else {
                        break;
                    }
                }
            }
        }

        self.set_cursor_col(new_col);
    }

    fn page_scroll(&mut self, direction: isize) {
        self.last_action = None;
        let page_size = max(5, (self.terminal_rows.saturating_mul(3)) / 10);
        let visual_lines = self.build_visual_line_map(self.last_width);
        let current_visual_line = self.find_current_visual_line(&visual_lines);
        let target_visual = if direction.is_negative() {
            current_visual_line.saturating_sub(page_size)
        } else {
            min(
                visual_lines.len().saturating_sub(1),
                current_visual_line.saturating_add(page_size),
            )
        };
        self.move_to_visual_line(&visual_lines, current_visual_line, target_visual);
    }

    fn set_cursor_col(&mut self, col: usize) {
        self.state.cursor_col = col;
        self.preferred_visual_col = None;
        if let Some(line) = self.state.lines.get(self.state.cursor_line) {
            if self.state.cursor_col > line.len() {
                self.state.cursor_col = line.len();
            }
            while self.state.cursor_col > 0 && !line.is_char_boundary(self.state.cursor_col) {
                self.state.cursor_col = self.state.cursor_col.saturating_sub(1);
            }
        }
    }

    fn is_on_first_visual_line(&self) -> bool {
        let visual_lines = self.build_visual_line_map(self.last_width);
        self.find_current_visual_line(&visual_lines) == 0
    }

    fn is_on_last_visual_line(&self) -> bool {
        let visual_lines = self.build_visual_line_map(self.last_width);
        let current = self.find_current_visual_line(&visual_lines);
        current + 1 == visual_lines.len()
    }

    fn is_editor_empty(&self) -> bool {
        self.state.lines.len() == 1 && self.state.lines[0].is_empty()
    }

    fn navigate_history(&mut self, direction: isize) {
        self.last_action = None;
        if self.history.is_empty() {
            return;
        }
        let new_index = self.history_index - direction;
        if new_index < -1 || new_index as usize >= self.history.len() {
            return;
        }
        if self.history_index == -1 && new_index >= 0 {
            self.push_undo_snapshot();
        }
        self.history_index = new_index;
        if self.history_index == -1 {
            self.set_text_internal("");
        } else {
            let idx = self.history_index as usize;
            let text = self.history.get(idx).cloned().unwrap_or_default();
            self.set_text_internal(&text);
        }
    }

    fn set_text_internal(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut lines: Vec<String> = normalized.split('\n').map(|part| part.to_string()).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        self.state.lines = lines;
        self.state.cursor_line = self.state.lines.len().saturating_sub(1);
        let last_len = self.state.lines[self.state.cursor_line].len();
        self.set_cursor_col(last_len);
        self.scroll_offset = 0;
        self.emit_change();
    }

    fn jump_to_char(&mut self, target: &str, direction: JumpMode) {
        self.last_action = None;
        let is_forward = matches!(direction, JumpMode::Forward);
        let total_lines = self.state.lines.len();
        let mut line_idx = self.state.cursor_line as isize;
        let end = if is_forward { total_lines as isize } else { -1 };
        let step = if is_forward { 1 } else { -1 };

        while line_idx != end {
            let line = self
                .state
                .lines
                .get(line_idx as usize)
                .map(String::as_str)
                .unwrap_or("");
            let is_current = line_idx as usize == self.state.cursor_line;

            let found = if is_forward {
                let start_index = if is_current {
                    let after = &line[self.state.cursor_col..];
                    if let Some(first) = after.chars().next() {
                        self.state.cursor_col + first.len_utf8()
                    } else {
                        line.len()
                    }
                } else {
                    0
                };
                if start_index <= line.len() {
                    line[start_index..].find(target).map(|offset| start_index + offset)
                } else {
                    None
                }
            } else if is_current {
                if self.state.cursor_col == 0 {
                    None
                } else {
                    let search_slice = &line[..self.state.cursor_col];
                    search_slice.rfind(target)
                }
            } else {
                line.rfind(target)
            };

            if let Some(found_idx) = found {
                self.state.cursor_line = line_idx as usize;
                self.set_cursor_col(found_idx);
                return;
            }

            line_idx += step;
        }
    }
}

impl Component for Editor {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.clamp_cursor();

        let max_padding = width.saturating_sub(1) / 2;
        let padding_x = min(self.padding_x, max_padding);
        let content_width = max(1, width.saturating_sub(padding_x * 2));
        let layout_width = max(1, content_width.saturating_sub(if padding_x > 0 { 0 } else { 1 }));
        self.last_width = layout_width;

        let horizontal = (self.border_color)("─");
        let layout_lines = self.layout_text(layout_width);

        let max_visible_lines = max(5, (self.terminal_rows.saturating_mul(3)) / 10);
        let cursor_line_index = layout_lines
            .iter()
            .position(|line| line.has_cursor)
            .unwrap_or(0);

        if cursor_line_index < self.scroll_offset {
            self.scroll_offset = cursor_line_index;
        } else if cursor_line_index >= self.scroll_offset + max_visible_lines {
            self.scroll_offset = cursor_line_index.saturating_sub(max_visible_lines - 1);
        }

        let max_scroll = layout_lines.len().saturating_sub(max_visible_lines);
        if self.scroll_offset > max_scroll {
            self.scroll_offset = max_scroll;
        }

        let visible_lines = layout_lines
            .iter()
            .skip(self.scroll_offset)
            .take(max_visible_lines)
            .cloned()
            .collect::<Vec<_>>();

        let mut result = Vec::new();
        let left_padding = " ".repeat(padding_x);
        let right_padding = left_padding.clone();

        if self.scroll_offset > 0 {
            let indicator = format!("─── ↑ {} more ", self.scroll_offset);
            let remaining = width.saturating_sub(visible_width(&indicator));
            let line = format!("{}{}", indicator, "─".repeat(remaining));
            result.push((self.border_color)(&line));
        } else {
            result.push(horizontal.repeat(width));
        }

        let emit_cursor_marker = self.focused;

        for layout_line in &visible_lines {
            let mut display_text = layout_line.text.clone();
            let mut line_visible_width = visible_width(&display_text);
            let mut cursor_in_padding = false;

            if layout_line.has_cursor {
                if let Some(cursor_pos) = layout_line.cursor_pos {
                    let cursor_pos = min(cursor_pos, display_text.len());
                    let (before, after) = display_text.split_at(cursor_pos);
                    let marker = if emit_cursor_marker { CURSOR_MARKER } else { "" };

                    if !after.is_empty() {
                        let mut graphemes = grapheme_segments(after);
                        let first = graphemes.next().unwrap_or("");
                        let rest = &after[first.len()..];
                        let cursor = format!("\x1b[7m{first}\x1b[0m");
                        display_text = format!("{before}{marker}{cursor}{rest}");
                    } else {
                        let cursor = "\x1b[7m \x1b[0m";
                        display_text = format!("{before}{marker}{cursor}");
                        line_visible_width = line_visible_width.saturating_add(1);
                        if line_visible_width > content_width && padding_x > 0 {
                            cursor_in_padding = true;
                        }
                    }
                }
            }

            let padding = " ".repeat(content_width.saturating_sub(line_visible_width));
            let line_right_padding = if cursor_in_padding && !right_padding.is_empty() {
                right_padding[1..].to_string()
            } else {
                right_padding.clone()
            };
            result.push(format!(
                "{left_padding}{display_text}{padding}{line_right_padding}"
            ));
        }

        let lines_below = layout_lines
            .len()
            .saturating_sub(self.scroll_offset + visible_lines.len());
        if lines_below > 0 {
            let indicator = format!("─── ↓ {} more ", lines_below);
            let remaining = width.saturating_sub(visible_width(&indicator));
            let line = format!("{}{}", indicator, "─".repeat(remaining));
            result.push((self.border_color)(&line));
        } else {
            result.push(horizontal.repeat(width));
        }

        result
    }

    fn set_terminal_rows(&mut self, rows: usize) {
        Editor::set_terminal_rows(self, rows);
    }

    fn handle_input(&mut self, data: &str) {
        self.clamp_cursor();

        let mut data = data.to_string();

        {
            let kb = get_editor_keybindings();
            let kb = kb.lock().expect("editor keybindings lock poisoned");
            if let Some(jump_mode) = self.jump_mode.take() {
                if kb.matches(&data, EditorAction::JumpForward)
                    || kb.matches(&data, EditorAction::JumpBackward)
                {
                    return;
                }

                if data.chars().next().map(|ch| (ch as u32) >= 32).unwrap_or(false) {
                    self.jump_to_char(&data, jump_mode);
                    return;
                }
            }
        }

        if data.contains(PASTE_START) {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            data = data.replacen(PASTE_START, "", 1);
        }

        if self.is_in_paste {
            self.paste_buffer.push_str(&data);
            if let Some(end_index) = self.paste_buffer.find(PASTE_END) {
                let paste_content = self.paste_buffer[..end_index].to_string();
                if !paste_content.is_empty() {
                    self.handle_paste(&paste_content);
                }
                self.is_in_paste = false;
                let remaining = self.paste_buffer[end_index + PASTE_END.len()..].to_string();
                self.paste_buffer.clear();
                if !remaining.is_empty() {
                    self.handle_input(&remaining);
                }
            }
            return;
        }

        let kb = get_editor_keybindings();
        let kb = kb.lock().expect("editor keybindings lock poisoned");

        if kb.matches(&data, EditorAction::Copy) {
            return;
        }

        if kb.matches(&data, EditorAction::Undo) {
            self.undo();
            return;
        }

        if kb.matches(&data, EditorAction::DeleteToLineEnd) {
            self.delete_to_end_of_line();
            return;
        }
        if kb.matches(&data, EditorAction::DeleteToLineStart) {
            self.delete_to_start_of_line();
            return;
        }
        if kb.matches(&data, EditorAction::DeleteWordBackward) {
            self.delete_word_backwards();
            return;
        }
        if kb.matches(&data, EditorAction::DeleteWordForward) {
            self.delete_word_forwards();
            return;
        }
        if kb.matches(&data, EditorAction::DeleteCharBackward)
            || crate::core::input::matches_key(&data, "shift+backspace")
        {
            self.handle_backspace();
            return;
        }
        if kb.matches(&data, EditorAction::DeleteCharForward)
            || crate::core::input::matches_key(&data, "shift+delete")
        {
            self.handle_forward_delete();
            return;
        }

        if kb.matches(&data, EditorAction::Yank) {
            self.yank();
            return;
        }
        if kb.matches(&data, EditorAction::YankPop) {
            self.yank_pop();
            return;
        }

        if kb.matches(&data, EditorAction::CursorLineStart) {
            self.move_to_line_start();
            return;
        }
        if kb.matches(&data, EditorAction::CursorLineEnd) {
            self.move_to_line_end();
            return;
        }
        if kb.matches(&data, EditorAction::CursorWordLeft) {
            self.move_word_backwards();
            return;
        }
        if kb.matches(&data, EditorAction::CursorWordRight) {
            self.move_word_forwards();
            return;
        }

        let is_new_line = kb.matches(&data, EditorAction::NewLine)
            || (data.as_bytes().first() == Some(&b'\n') && data.len() > 1)
            || data == "\x1b\r"
            || data == "\x1b[13;2~"
            || (data.len() > 1 && data.contains('\x1b') && data.contains('\r'))
            || data == "\n";
        if is_new_line {
            if self.should_submit_on_backslash_enter(&data, &kb) {
                self.handle_backspace();
                self.submit_value();
                return;
            }
            self.add_new_line();
            return;
        }

        if kb.matches(&data, EditorAction::Submit) {
            if self.disable_submit {
                return;
            }

            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .map(String::as_str)
                .unwrap_or("");
            if self.state.cursor_col > 0 && current_line[..self.state.cursor_col].ends_with('\\') {
                self.handle_backspace();
                self.add_new_line();
                return;
            }

            self.submit_value();
            return;
        }

        if kb.matches(&data, EditorAction::CursorUp) {
            if self.is_editor_empty() {
                self.navigate_history(-1);
            } else if self.history_index > -1 && self.is_on_first_visual_line() {
                self.navigate_history(-1);
            } else if self.is_on_first_visual_line() {
                self.move_to_line_start();
            } else {
                self.move_cursor(-1, 0);
            }
            return;
        }
        if kb.matches(&data, EditorAction::CursorDown) {
            if self.history_index > -1 && self.is_on_last_visual_line() {
                self.navigate_history(1);
            } else if self.is_on_last_visual_line() {
                self.move_to_line_end();
            } else {
                self.move_cursor(1, 0);
            }
            return;
        }
        if kb.matches(&data, EditorAction::CursorRight) {
            self.move_cursor(0, 1);
            return;
        }
        if kb.matches(&data, EditorAction::CursorLeft) {
            self.move_cursor(0, -1);
            return;
        }

        if kb.matches(&data, EditorAction::PageUp) {
            self.page_scroll(-1);
            return;
        }
        if kb.matches(&data, EditorAction::PageDown) {
            self.page_scroll(1);
            return;
        }

        if kb.matches(&data, EditorAction::JumpForward) {
            self.jump_mode = Some(JumpMode::Forward);
            return;
        }
        if kb.matches(&data, EditorAction::JumpBackward) {
            self.jump_mode = Some(JumpMode::Backward);
            return;
        }

        if crate::core::input::matches_key(&data, "shift+space") {
            self.insert_character(" ", false);
            return;
        }

        if let Some(decoded) = decode_kitty_printable(&data) {
            self.insert_character(&decoded, false);
            return;
        }

        if data.chars().next().map(|ch| (ch as u32) >= 32).unwrap_or(false) {
            self.insert_character(&data, false);
        }
    }

    fn invalidate(&mut self) {}

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for Editor {
    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }
}

impl EditorComponent for Editor {
    fn get_text(&self) -> String {
        self.state.lines.join("\n")
    }

    fn set_text(&mut self, text: &str) {
        Editor::set_text(self, text);
    }

    fn insert_text_at_cursor(&mut self, text: &str) {
        Editor::insert_text_at_cursor(self, text);
    }

    fn get_expanded_text(&self) -> String {
        Editor::get_expanded_text(self)
    }

    fn set_on_submit(&mut self, handler: Option<Box<dyn FnMut(String)>>) {
        self.on_submit = handler;
    }

    fn set_on_change(&mut self, handler: Option<Box<dyn FnMut(String)>>) {
        self.on_change = handler;
    }

    fn add_to_history(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.first().map(|item| item == trimmed).unwrap_or(false) {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        if self.history.len() > 100 {
            self.history.pop();
        }
    }

    fn set_border_color(&mut self, border_color: Box<dyn Fn(&str) -> String>) {
        Editor::set_border_color(self, border_color);
    }

    fn set_padding_x(&mut self, padding: usize) {
        Editor::set_padding_x(self, padding);
    }

    fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        Editor::set_autocomplete_max_visible(self, max_visible);
    }
}

#[derive(Debug, Clone)]
struct VisualLine {
    logical_line: usize,
    start_col: usize,
    length: usize,
}

fn is_whitespace_segment(segment: &str) -> bool {
    segment.chars().any(is_whitespace_char)
}

fn is_punctuation_segment(segment: &str) -> bool {
    segment.chars().any(is_punctuation_char)
}

fn decode_kitty_printable(data: &str) -> Option<String> {
    if !data.starts_with("\x1b[") || !data.ends_with('u') {
        return None;
    }
    let inner = &data[2..data.len() - 1];
    let (left, right) = match inner.split_once(';') {
        Some((left, right)) => (left, right),
        None => (inner, ""),
    };

    let mut left_parts = left.split(':');
    let codepoint = left_parts.next().and_then(|value| value.parse::<u32>().ok())?;
    let shifted = left_parts
        .next()
        .and_then(|value| if value.is_empty() { None } else { value.parse::<u32>().ok() });

    let mod_value = if right.is_empty() {
        1u32
    } else {
        right
            .split(':')
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(1)
    };
    let modifier = mod_value.saturating_sub(1);

    const MOD_SHIFT: u32 = 1;
    const MOD_ALT: u32 = 2;
    const MOD_CTRL: u32 = 4;

    if modifier & (MOD_ALT | MOD_CTRL) != 0 {
        return None;
    }

    let mut effective = codepoint;
    if modifier & MOD_SHIFT != 0 {
        if let Some(shifted) = shifted {
            effective = shifted;
        }
    }
    if effective < 32 {
        return None;
    }
    char::from_u32(effective).map(|ch| ch.to_string())
}

#[cfg(test)]
mod tests {
    use super::{word_wrap_line, Editor, EditorOptions, EditorTheme};
    use crate::core::component::Component;
    use crate::widgets::select_list::SelectListTheme;

    fn theme() -> EditorTheme {
        EditorTheme {
            border_color: Box::new(|text| text.to_string()),
            select_list: SelectListTheme {
                selected_prefix: Box::new(|text| text.to_string()),
                selected_text: Box::new(|text| text.to_string()),
                description: Box::new(|text| text.to_string()),
                scroll_info: Box::new(|text| text.to_string()),
                no_match: Box::new(|text| text.to_string()),
            },
        }
    }

    #[test]
    fn word_wrap_line_breaks_long_words() {
        let chunks = word_wrap_line("abcdefgh", 3);
        let texts: Vec<String> = chunks.into_iter().map(|chunk| chunk.text).collect();
        assert_eq!(texts, vec!["abc", "def", "gh"]);
    }

    #[test]
    fn word_wrap_line_records_indices() {
        let chunks = word_wrap_line("hello world", 5);
        assert_eq!(chunks[0].start_index, 0);
        assert_eq!(chunks[0].end_index, 5);
        assert_eq!(chunks.last().unwrap().end_index, "hello world".len());
    }

    #[test]
    fn editor_moves_across_lines() {
        let mut editor = Editor::new(theme(), EditorOptions::default());
        editor.set_text("one\ntwo");
        editor.state.cursor_line = 0;
        editor.state.cursor_col = 3;

        editor.handle_input("\x1b[C");
        assert_eq!(editor.get_cursor(), (1, 0));

        editor.handle_input("\x1b[D");
        assert_eq!(editor.get_cursor(), (0, 3));
    }

    #[test]
    fn editor_scrolls_to_keep_cursor_visible() {
        let mut editor = Editor::new(theme(), EditorOptions::default());
        editor.set_terminal_rows(10);
        let lines = (0..10).map(|idx| format!("line {idx}")).collect::<Vec<_>>();
        editor.state.lines = lines;
        editor.state.cursor_line = 7;
        editor.state.cursor_col = 0;

        let _ = editor.render(20);
        assert_eq!(editor.scroll_offset, 3);
    }

    #[test]
    fn editor_renders_cursor_marker_when_focused() {
        let mut editor = Editor::new(theme(), EditorOptions::default());
        editor.state.lines = vec!["hi".to_string()];
        editor.state.cursor_line = 0;
        editor.state.cursor_col = 1;
        editor.focused = true;
        let lines = editor.render(10);
        assert!(lines.iter().any(|line| line.contains("\x1b_pi:c")));
    }

    #[test]
    fn editor_top_border_when_scrolled() {
        let mut editor = Editor::new(theme(), EditorOptions::default());
        editor.set_terminal_rows(10);
        editor.state.lines = (0..10).map(|idx| format!("row {idx}")).collect();
        editor.state.cursor_line = 8;
        editor.state.cursor_col = 0;
        let lines = editor.render(20);
        assert!(lines[0].contains("↑"));
    }

    #[test]
    fn editor_undo_coalesces_words() {
        let mut editor = Editor::new(theme(), EditorOptions::default());
        for ch in "hello world".chars() {
            let input = ch.to_string();
            editor.handle_input(&input);
        }
        editor.handle_input("\x1f"); // ctrl+-
        assert_eq!(editor.get_text(), "hello");
        editor.handle_input("\x1f");
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn editor_kill_and_yank_restore_line() {
        let mut editor = Editor::new(theme(), EditorOptions::default());
        editor.set_text("hello world");
        editor.state.cursor_line = 0;
        editor.set_cursor_col(5);

        editor.handle_input("\x0b"); // ctrl+k
        assert_eq!(editor.get_text(), "hello");

        editor.handle_input("\x19"); // ctrl+y
        assert_eq!(editor.get_text(), "hello world");
    }

    #[test]
    fn editor_large_paste_inserts_marker_and_expands() {
        let mut editor = Editor::new(theme(), EditorOptions::default());
        let lines = (0..11).map(|idx| format!("line{idx}")).collect::<Vec<_>>();
        let paste = lines.join("\n");
        let input = format!("\x1b[200~{paste}\x1b[201~");
        editor.handle_input(&input);
        let text = editor.get_text();
        assert!(text.contains("[paste #1 +11 lines]"));
        assert_eq!(editor.get_expanded_text(), paste);
    }
}
