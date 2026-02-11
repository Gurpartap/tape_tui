//! Editor widget (Phase 25).

use std::cmp::{max, min};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use unicode_segmentation::UnicodeSegmentation;

use crate::core::autocomplete::{
    AbortSignal, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
};
use crate::core::component::{Component, Focusable};
use crate::core::cursor::CursorPos;
use crate::core::editor_component::EditorComponent;
use crate::core::input_event::InputEvent;
use crate::core::keybindings::{EditorAction, EditorKeybindingsHandle};
use crate::core::text::utils::{grapheme_segments, is_punctuation_char, is_whitespace_char};
use crate::core::text::width::visible_width;
use crate::runtime::tui::{Command, RuntimeHandle};
use crate::widgets::select_list::{SelectItem, SelectList, SelectListTheme};

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct AutocompleteSnapshot {
    text: String,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorHeightMode {
    /// Preserve pi-tui parity behavior (chat-style editor height heuristic).
    Default,
    /// Expand the editor to fill the available vertical space passed via `set_terminal_rows`.
    FillAvailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorPasteMode {
    /// Preserve pi-tui parity behavior (large pastes are replaced by paste markers).
    Default,
    /// Always insert the literal pasted content, never inserting paste markers.
    Literal,
}

#[derive(Clone, Default)]
pub struct EditorOptions {
    pub padding_x: Option<usize>,
    pub autocomplete_max_visible: Option<usize>,
    pub height_mode: Option<EditorHeightMode>,
    pub paste_mode: Option<EditorPasteMode>,
    pub render_handle: Option<RuntimeHandle>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutocompleteState {
    Regular,
    Force,
}

pub struct Editor {
    state: EditorState,
    focused: bool,
    last_cursor_pos: Option<CursorPos>,
    select_list_theme: SelectListTheme,
    padding_x: usize,
    autocomplete_max_visible: usize,
    render_handle: Option<RuntimeHandle>,
    keybindings: EditorKeybindingsHandle,
    autocomplete_provider: Option<Box<dyn AutocompleteProvider>>,
    autocomplete_list: Option<SelectList>,
    autocomplete_state: Option<AutocompleteState>,
    autocomplete_prefix: String,
    autocomplete_abort_signal: Option<AbortSignal>,
    autocomplete_snapshot: Option<AutocompleteSnapshot>,
    autocomplete_selection_changed: bool,
    autocomplete_selected_value: Option<String>,
    autocomplete_update_slot: Option<Arc<Mutex<Vec<AutocompleteSuggestions>>>>,
    autocomplete_async_handle: Option<JoinHandle<Option<AutocompleteSuggestions>>>,
    autocomplete_has_updates: bool,
    last_width: usize,
    scroll_offset: usize,
    border_color: Box<dyn Fn(&str) -> String>,
    terminal_rows: usize,
    height_mode: EditorHeightMode,
    paste_mode: EditorPasteMode,
    preferred_visual_col: Option<usize>,
    jump_mode: Option<JumpMode>,
    disable_submit: bool,
    pastes: HashMap<u32, String>,
    paste_counter: u32,
    kill_ring: Vec<String>,
    last_action: Option<LastAction>,
    undo_stack: Vec<EditorState>,
    on_submit: Option<Box<dyn FnMut(String)>>,
    on_change: Option<Box<dyn FnMut(String)>>,
    history: Vec<String>,
    history_index: isize,
}

impl Editor {
    pub fn new(
        theme: EditorTheme,
        keybindings: EditorKeybindingsHandle,
        options: EditorOptions,
    ) -> Self {
        let padding_x = options.padding_x.unwrap_or(0);
        let max_visible = options.autocomplete_max_visible.unwrap_or(5);
        let autocomplete_max_visible = max_visible.clamp(3, 20);
        let height_mode = options.height_mode.unwrap_or(EditorHeightMode::Default);
        let paste_mode = options.paste_mode.unwrap_or(EditorPasteMode::Default);
        let render_handle = options.render_handle;
        let border_color = theme.border_color;
        let select_list_theme = theme.select_list;
        Self {
            state: EditorState {
                lines: vec![String::new()],
                cursor_line: 0,
                cursor_col: 0,
            },
            focused: false,
            last_cursor_pos: None,
            select_list_theme,
            padding_x,
            autocomplete_max_visible,
            render_handle,
            keybindings,
            autocomplete_provider: None,
            autocomplete_list: None,
            autocomplete_state: None,
            autocomplete_prefix: String::new(),
            autocomplete_abort_signal: None,
            autocomplete_snapshot: None,
            autocomplete_selection_changed: false,
            autocomplete_selected_value: None,
            autocomplete_update_slot: None,
            autocomplete_async_handle: None,
            autocomplete_has_updates: false,
            last_width: 80,
            scroll_offset: 0,
            border_color,
            terminal_rows: 0,
            height_mode,
            paste_mode,
            preferred_visual_col: None,
            jump_mode: None,
            disable_submit: false,
            pastes: HashMap::new(),
            paste_counter: 0,
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
        if self.padding_x != padding {
            self.padding_x = padding;
            self.request_render();
        }
    }

    pub fn get_padding_x(&self) -> usize {
        self.padding_x
    }

    pub fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        let new_value = max_visible.clamp(3, 20);
        if self.autocomplete_max_visible != new_value {
            self.autocomplete_max_visible = new_value;
            self.request_render();
        }
    }

    pub fn get_autocomplete_max_visible(&self) -> usize {
        self.autocomplete_max_visible
    }

    pub fn set_autocomplete_provider(&mut self, provider: Box<dyn AutocompleteProvider>) {
        self.autocomplete_provider = Some(provider);
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

    fn request_render(&self) {
        if let Some(handle) = self.render_handle.as_ref() {
            handle.dispatch(Command::RequestRender);
        }
    }

    fn is_slash_menu_allowed(&self) -> bool {
        self.state.cursor_line == 0
    }

    fn is_at_start_of_message(&self) -> bool {
        if !self.is_slash_menu_allowed() {
            return false;
        }
        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .map(String::as_str)
            .unwrap_or("");
        let before_cursor = current_line
            .get(..self.state.cursor_col)
            .unwrap_or(current_line);
        let trimmed = before_cursor.trim();
        trimmed.is_empty() || trimmed == "/"
    }

    fn is_in_slash_command_context(&self, text_before_cursor: &str) -> bool {
        self.is_slash_menu_allowed() && text_before_cursor.trim_start().starts_with('/')
    }

    fn is_in_at_file_context(&self, text_before_cursor: &str) -> bool {
        let Some(at_pos) = text_before_cursor.rfind('@') else {
            return false;
        };
        let before = if at_pos == 0 {
            None
        } else {
            text_before_cursor[..at_pos].chars().last()
        };
        let after = &text_before_cursor[at_pos + 1..];
        let before_ok = at_pos == 0 || before.map(|c| c.is_whitespace()).unwrap_or(false);
        let after_ok = !after.chars().any(|c| c.is_whitespace());
        before_ok && after_ok
    }

    fn capture_autocomplete_snapshot(&self) -> AutocompleteSnapshot {
        AutocompleteSnapshot {
            text: self.get_text(),
            cursor_line: self.state.cursor_line,
            cursor_col: self.state.cursor_col,
        }
    }

    fn is_autocomplete_snapshot_current(&self, snapshot: &AutocompleteSnapshot) -> bool {
        snapshot.text == self.get_text()
            && snapshot.cursor_line == self.state.cursor_line
            && snapshot.cursor_col == self.state.cursor_col
    }

    fn abort_autocomplete_request(&mut self) {
        if let Some(signal) = self.autocomplete_abort_signal.take() {
            signal.abort();
        }
        self.autocomplete_snapshot = None;
        self.autocomplete_update_slot = None;
        self.autocomplete_async_handle = None;
        self.autocomplete_has_updates = false;
    }

    fn apply_autocomplete_suggestions(&mut self, suggestions: AutocompleteSuggestions) {
        if suggestions.items.is_empty() {
            self.cancel_autocomplete();
            return;
        }

        if self.autocomplete_prefix != suggestions.prefix {
            self.autocomplete_selection_changed = false;
            self.autocomplete_selected_value = None;
        }

        let selected_index = if self.autocomplete_selection_changed {
            if let Some(selected_value) = self.autocomplete_selected_value.as_ref() {
                suggestions
                    .items
                    .iter()
                    .position(|item| item.value == *selected_value)
            } else {
                None
            }
        } else {
            None
        };

        let items = suggestions
            .items
            .iter()
            .map(|item| {
                SelectItem::new(
                    item.value.clone(),
                    item.label.clone(),
                    item.description.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut list = SelectList::new(
            items,
            self.autocomplete_max_visible,
            self.select_list_theme.clone(),
            self.keybindings.clone(),
        );
        if let Some(index) = selected_index {
            list.set_selected_index(index);
        }

        self.autocomplete_list = Some(list);
        self.autocomplete_prefix = suggestions.prefix;
        self.autocomplete_state = Some(AutocompleteState::Regular);

        let selected = self
            .autocomplete_list
            .as_ref()
            .and_then(|list| list.get_selected_item());
        self.autocomplete_selected_value = selected.map(|item| item.value.clone());
    }

    fn start_async_autocomplete(&mut self) {
        if self.autocomplete_provider.is_none() {
            return;
        }

        self.abort_autocomplete_request();
        self.autocomplete_state = None;
        self.autocomplete_list = None;
        self.autocomplete_prefix.clear();
        self.autocomplete_selection_changed = false;
        self.autocomplete_selected_value = None;
        self.autocomplete_has_updates = false;

        let signal = AbortSignal::new();
        let updates: Arc<Mutex<Vec<AutocompleteSuggestions>>> = Arc::new(Mutex::new(Vec::new()));
        let updates_clone = Arc::clone(&updates);
        let signal_clone = signal.clone();
        let render_handle = self.render_handle.clone();

        let lines_snapshot = self.state.lines.clone();
        let cursor_line = self.state.cursor_line;
        let cursor_col = self.state.cursor_col;
        let snapshot = self.capture_autocomplete_snapshot();
        self.autocomplete_snapshot = Some(snapshot);

        let provider = match self.autocomplete_provider.as_ref() {
            Some(provider) => provider,
            None => return,
        };

        let handle = provider.get_suggestions_async(
            lines_snapshot,
            cursor_line,
            cursor_col,
            Some(signal.clone()),
            Some(Box::new(move |suggestions| {
                if signal_clone.is_aborted() || suggestions.items.is_empty() {
                    return;
                }
                if let Ok(mut slot) = updates_clone.lock() {
                    slot.push(suggestions);
                }
                if let Some(handle) = render_handle.as_ref() {
                    handle.dispatch(Command::RequestRender);
                }
            })),
        );

        if handle.is_none() {
            self.cancel_autocomplete();
            return;
        }

        self.autocomplete_abort_signal = Some(signal);
        self.autocomplete_update_slot = Some(updates);
        self.autocomplete_async_handle = handle;
    }

    fn try_trigger_autocomplete(&mut self, explicit_tab: bool) {
        let Some(provider) = self.autocomplete_provider.as_ref() else {
            return;
        };

        if explicit_tab
            && !provider.should_trigger_file_completion(
                &self.state.lines,
                self.state.cursor_line,
                self.state.cursor_col,
            )
        {
            return;
        }

        let suggestions = provider.get_suggestions(
            &self.state.lines,
            self.state.cursor_line,
            self.state.cursor_col,
        );
        if let Some(suggestions) = suggestions {
            if !suggestions.items.is_empty() {
                self.abort_autocomplete_request();
                self.apply_autocomplete_suggestions(suggestions);
                return;
            }
        }

        self.start_async_autocomplete();
    }

    fn handle_tab_completion(&mut self) {
        let Some(_) = self.autocomplete_provider.as_ref() else {
            return;
        };

        let current_line = self
            .state
            .lines
            .get(self.state.cursor_line)
            .map(String::as_str)
            .unwrap_or("");
        let before_cursor = current_line
            .get(..self.state.cursor_col)
            .unwrap_or(current_line);

        if self.is_in_slash_command_context(before_cursor)
            && !before_cursor.trim_start().contains(' ')
        {
            self.handle_slash_command_completion();
        } else {
            self.force_file_autocomplete(true);
        }
    }

    fn handle_slash_command_completion(&mut self) {
        self.try_trigger_autocomplete(true);
    }

    fn force_file_autocomplete(&mut self, explicit_tab: bool) {
        let suggestions = self.autocomplete_provider.as_ref().and_then(|provider| {
            provider.get_force_file_suggestions(
                &self.state.lines,
                self.state.cursor_line,
                self.state.cursor_col,
            )
        });
        let Some(suggestions) = suggestions else {
            self.try_trigger_autocomplete(true);
            return;
        };

        if suggestions.items.is_empty() {
            self.cancel_autocomplete();
            return;
        }

        self.abort_autocomplete_request();
        if explicit_tab && suggestions.items.len() == 1 {
            let item = suggestions.items[0].clone();
            let result = {
                let provider = self
                    .autocomplete_provider
                    .as_ref()
                    .expect("autocomplete provider missing");
                provider.apply_completion(
                    &self.state.lines,
                    self.state.cursor_line,
                    self.state.cursor_col,
                    &item,
                    &suggestions.prefix,
                )
            };
            self.push_undo_snapshot();
            self.last_action = None;
            self.state.lines = result.lines;
            self.state.cursor_line = result.cursor_line;
            self.set_cursor_col(result.cursor_col);
            self.emit_change();
            return;
        }

        let items = suggestions
            .items
            .iter()
            .map(|item| {
                SelectItem::new(
                    item.value.clone(),
                    item.label.clone(),
                    item.description.clone(),
                )
            })
            .collect::<Vec<_>>();
        let list = SelectList::new(
            items,
            self.autocomplete_max_visible,
            self.select_list_theme.clone(),
            self.keybindings.clone(),
        );

        self.autocomplete_prefix = suggestions.prefix;
        self.autocomplete_list = Some(list);
        self.autocomplete_state = Some(AutocompleteState::Force);
    }

    fn cancel_autocomplete(&mut self) {
        self.abort_autocomplete_request();
        self.autocomplete_state = None;
        self.autocomplete_list = None;
        self.autocomplete_prefix.clear();
        self.autocomplete_selection_changed = false;
        self.autocomplete_selected_value = None;
    }

    pub fn is_showing_autocomplete(&self) -> bool {
        self.autocomplete_state.is_some()
    }

    fn update_autocomplete(&mut self) {
        if self.autocomplete_state.is_none() || self.autocomplete_provider.is_none() {
            return;
        }

        if self.autocomplete_state == Some(AutocompleteState::Force) {
            self.force_file_autocomplete(false);
            return;
        }

        let provider = match self.autocomplete_provider.as_ref() {
            Some(provider) => provider,
            None => return,
        };

        let suggestions = provider.get_suggestions(
            &self.state.lines,
            self.state.cursor_line,
            self.state.cursor_col,
        );
        if let Some(suggestions) = suggestions {
            if !suggestions.items.is_empty() {
                self.abort_autocomplete_request();
                self.apply_autocomplete_suggestions(suggestions);
                return;
            }
        }

        self.start_async_autocomplete();
    }

    fn poll_autocomplete_async(&mut self) {
        self.drain_autocomplete_updates();

        let finished = self
            .autocomplete_async_handle
            .as_ref()
            .map(|handle| handle.is_finished())
            .unwrap_or(false);
        if !finished {
            return;
        }

        let handle = self.autocomplete_async_handle.take();
        let Some(handle) = handle else {
            return;
        };
        let result = handle.join();
        match result {
            Ok(Some(suggestions)) => {
                if let Some(snapshot) = self.autocomplete_snapshot.clone() {
                    if self.is_autocomplete_snapshot_current(&snapshot) {
                        self.apply_autocomplete_suggestions(suggestions);
                        self.request_render();
                    }
                }
            }
            Ok(None) => {
                if !self.autocomplete_has_updates {
                    self.cancel_autocomplete();
                }
            }
            Err(_) => {}
        }
    }

    fn drain_autocomplete_updates(&mut self) {
        let Some(updates) = self.autocomplete_update_slot.as_ref() else {
            return;
        };
        let pending = {
            let Ok(mut slot) = updates.lock() else {
                return;
            };
            if slot.is_empty() {
                return;
            }
            std::mem::take(&mut *slot)
        };
        let Some(snapshot) = self.autocomplete_snapshot.clone() else {
            return;
        };
        if !self.is_autocomplete_snapshot_current(&snapshot) {
            return;
        }

        for suggestions in pending {
            if suggestions.items.is_empty() {
                continue;
            }
            self.apply_autocomplete_suggestions(suggestions);
            self.autocomplete_has_updates = true;
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
            self.state.cursor_line = self
                .state
                .cursor_line
                .saturating_add(inserted_lines.len() - 1);
            self.set_cursor_col(last_inserted.len());
        }

        self.emit_change();

        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        } else {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .map(String::as_str)
                .unwrap_or("");
            let text_before_cursor = current_line
                .get(..self.state.cursor_col)
                .unwrap_or(current_line);
            if self.is_in_slash_command_context(text_before_cursor)
                || self.is_in_at_file_context(text_before_cursor)
            {
                self.try_trigger_autocomplete(false);
            }
        }
    }

    fn insert_character(&mut self, ch: &str, skip_undo_coalescing: bool) {
        if ch.is_empty() {
            return;
        }

        self.history_index = -1;

        if !skip_undo_coalescing {
            if ch.chars().any(is_whitespace_char) || self.last_action != Some(LastAction::TypeWord)
            {
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

        if self.autocomplete_state.is_none() {
            if ch == "/" && self.is_at_start_of_message() {
                self.try_trigger_autocomplete(false);
            } else if ch == "@" {
                let current_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .map(String::as_str)
                    .unwrap_or("");
                let text_before_cursor = current_line
                    .get(..self.state.cursor_col)
                    .unwrap_or(current_line);
                let mut chars = text_before_cursor.chars().rev();
                let before_at = chars.nth(1);
                if text_before_cursor.chars().count() == 1
                    || matches!(before_at, Some(' ') | Some('\t'))
                {
                    self.try_trigger_autocomplete(false);
                }
            } else if ch
                .chars()
                .any(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
            {
                let current_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .map(String::as_str)
                    .unwrap_or("");
                let text_before_cursor = current_line
                    .get(..self.state.cursor_col)
                    .unwrap_or(current_line);
                if self.is_in_slash_command_context(text_before_cursor)
                    || self.is_in_at_file_context(text_before_cursor)
                {
                    self.try_trigger_autocomplete(false);
                }
            }
        } else {
            self.update_autocomplete();
        }
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
                current_line[..self.state.cursor_col].chars().last()
            } else {
                None
            };
            if let Some(ch) = char_before {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    filtered = format!(" {filtered}");
                }
            }
        }

        if self.paste_mode == EditorPasteMode::Literal {
            self.insert_text_at_cursor_internal(&filtered);
            return;
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
        self.state.lines.insert(self.state.cursor_line + 1, after);
        self.state.cursor_line += 1;
        self.set_cursor_col(0);

        self.emit_change();
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
            self.state.lines[self.state.cursor_line] = format!("{}{}", &line[..start], after);
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

        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        } else {
            let current_line = self
                .state
                .lines
                .get(self.state.cursor_line)
                .map(String::as_str)
                .unwrap_or("");
            let text_before_cursor = current_line
                .get(..self.state.cursor_col)
                .unwrap_or(current_line);
            if self.is_in_slash_command_context(text_before_cursor)
                || self.is_in_at_file_context(text_before_cursor)
            {
                self.try_trigger_autocomplete(false);
            }
        }
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
            self.state.lines[self.state.cursor_line] = format!(
                "{}{}",
                &current_line[..self.state.cursor_col],
                &current_line[end..]
            );
        } else if self.state.cursor_line + 1 < self.state.lines.len() {
            self.push_undo_snapshot();
            let next_line = self
                .state
                .lines
                .get(self.state.cursor_line + 1)
                .cloned()
                .unwrap_or_default();
            self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
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
            self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
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
            self.state.lines[self.state.cursor_line] = format!(
                "{}{}",
                &current_line[..delete_from],
                &current_line[old_col..]
            );
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
                self.state.lines[self.state.cursor_line] = format!("{current_line}{next_line}");
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
            let start_col = line_at_start.len().saturating_sub(yank_lines[0].len());
            let after_cursor = self
                .state
                .lines
                .get(self.state.cursor_line)
                .map(|line| line[self.state.cursor_col..].to_string())
                .unwrap_or_default();
            let before_yank = line_at_start[..start_col].to_string();

            self.state.lines.splice(
                start_line..=self.state.cursor_line,
                [format!("{before_yank}{after_cursor}")],
            );
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
                        paste_id = paste_id
                            .saturating_mul(10)
                            .saturating_add((ch - b'0') as u32);
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

                let mut valid_suffix = true;
                if cursor < bytes.len() && bytes[cursor] == b' ' {
                    cursor += 1;
                    if cursor < bytes.len() && bytes[cursor] == b'+' {
                        cursor += 1;
                        let digits_start = cursor;
                        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                            cursor += 1;
                        }
                        if digits_start == cursor {
                            valid_suffix = false;
                        } else if bytes[cursor..].starts_with(b" lines") {
                            cursor += b" lines".len();
                        } else {
                            valid_suffix = false;
                        }
                    } else {
                        let digits_start = cursor;
                        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                            cursor += 1;
                        }
                        if digits_start == cursor {
                            valid_suffix = false;
                        } else if bytes[cursor..].starts_with(b" chars") {
                            cursor += b" chars".len();
                        } else {
                            valid_suffix = false;
                        }
                    }
                }

                if valid_suffix && cursor < bytes.len() && bytes[cursor] == b']' {
                    if let Some(content) = self.pastes.get(&paste_id) {
                        result.push_str(content);
                    } else {
                        result.push_str(&input[start..=cursor]);
                    }
                    idx = cursor + 1;
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
                            adjusted_cursor =
                                self.state.cursor_col.saturating_sub(chunk.start_index);
                        } else if self.state.cursor_col >= chunk.start_index
                            && self.state.cursor_col < chunk.end_index
                        {
                            has_cursor = true;
                            adjusted_cursor =
                                self.state.cursor_col.saturating_sub(chunk.start_index);
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
                if col_in_segment < line.length
                    || (is_last_segment && col_in_segment <= line.length)
                {
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
                min(
                    visual_lines.len().saturating_sub(1),
                    current_visual_line.saturating_add(delta),
                )
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
                    self.preferred_visual_col =
                        Some(self.state.cursor_col.saturating_sub(current_vl.start_col));
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

        let move_col =
            self.compute_vertical_move_column(current_visual_col, source_max, target_max);
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
        if new_index < -1 || (new_index >= 0 && new_index as usize >= self.history.len()) {
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
        let mut lines: Vec<String> = normalized
            .split('\n')
            .map(|part| part.to_string())
            .collect();
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
                    line[start_index..]
                        .find(target)
                        .map(|offset| start_index + offset)
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
        self.poll_autocomplete_async();
        self.last_cursor_pos = None;

        let max_padding = width.saturating_sub(1) / 2;
        let padding_x = min(self.padding_x, max_padding);
        let content_width = max(1, width.saturating_sub(padding_x * 2));
        let layout_width = max(
            1,
            content_width.saturating_sub(if padding_x > 0 { 0 } else { 1 }),
        );
        self.last_width = layout_width;

        let horizontal = (self.border_color)("─");
        let layout_lines = self.layout_text(layout_width);

        let default_visible_lines = max(5, (self.terminal_rows.saturating_mul(3)) / 10);
        let (max_visible_lines, fill_exact_height) = match self.height_mode {
            EditorHeightMode::Default => (default_visible_lines, false),
            EditorHeightMode::FillAvailable => {
                if self.terminal_rows == 0 {
                    // Without a height, preserve the Default behavior instead of forcing a fixed layout.
                    (default_visible_lines, false)
                } else {
                    (self.terminal_rows.saturating_sub(2), true)
                }
            }
        };
        let cursor_line_index = layout_lines
            .iter()
            .position(|line| line.has_cursor)
            .unwrap_or(0);

        if cursor_line_index < self.scroll_offset {
            self.scroll_offset = cursor_line_index;
        } else if cursor_line_index >= self.scroll_offset + max_visible_lines {
            self.scroll_offset =
                cursor_line_index.saturating_sub(max_visible_lines.saturating_sub(1));
        }

        let max_scroll = layout_lines.len().saturating_sub(max_visible_lines);
        if self.scroll_offset > max_scroll {
            self.scroll_offset = max_scroll;
        }

        let mut visible_lines = layout_lines
            .iter()
            .skip(self.scroll_offset)
            .take(max_visible_lines)
            .cloned()
            .collect::<Vec<_>>();

        if fill_exact_height && visible_lines.len() < max_visible_lines {
            let missing = max_visible_lines - visible_lines.len();
            visible_lines.extend((0..missing).map(|_| LayoutLine {
                text: String::new(),
                has_cursor: false,
                cursor_pos: None,
            }));
        }

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

        let emit_cursor = self.focused && self.autocomplete_state.is_none();

        for (visible_idx, layout_line) in visible_lines.iter().enumerate() {
            let mut display_text = layout_line.text.clone();
            let mut line_visible_width = visible_width(&display_text);
            let mut cursor_in_padding = false;

            if layout_line.has_cursor {
                if let Some(cursor_pos) = layout_line.cursor_pos {
                    let cursor_pos = min(cursor_pos, display_text.len());
                    let (before, after) = display_text.split_at(cursor_pos);

                    if emit_cursor {
                        let col = padding_x.saturating_add(visible_width(before));
                        let row = 1 + visible_idx;
                        self.last_cursor_pos = Some(CursorPos { row, col });
                    }

                    if !after.is_empty() {
                        let mut graphemes = grapheme_segments(after);
                        let first = graphemes.next().unwrap_or("");
                        let rest = &after[first.len()..];
                        let cursor = format!("\x1b[7m{first}\x1b[0m");
                        display_text = format!("{before}{cursor}{rest}");
                    } else {
                        let cursor = "\x1b[7m \x1b[0m";
                        display_text = format!("{before}{cursor}");
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

        if !(fill_exact_height && self.terminal_rows == 1) {
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
        }

        if self.autocomplete_state.is_some() {
            if let Some(list) = self.autocomplete_list.as_mut() {
                let autocomplete_result = list.render(content_width);
                for line in autocomplete_result {
                    let line_width = visible_width(&line);
                    let padding = " ".repeat(content_width.saturating_sub(line_width));
                    result.push(format!("{left_padding}{line}{padding}{right_padding}"));
                }
            }
        }

        result
    }

    fn cursor_pos(&self) -> Option<CursorPos> {
        self.last_cursor_pos
    }

    fn set_terminal_rows(&mut self, rows: usize) {
        Editor::set_terminal_rows(self, rows);
    }

    fn handle_event(&mut self, event: &InputEvent) {
        self.clamp_cursor();
        self.poll_autocomplete_async();

        let key_id = match event {
            InputEvent::Key { key_id, .. } => Some(key_id.as_str()),
            _ => None,
        };

        if let Some(jump_mode) = self.jump_mode.take() {
            let is_jump_key = {
                let kb = self
                    .keybindings
                    .lock()
                    .expect("editor keybindings lock poisoned");
                kb.matches(key_id, EditorAction::JumpForward)
                    || kb.matches(key_id, EditorAction::JumpBackward)
            };
            if is_jump_key {
                return;
            }

            if let InputEvent::Text { text, .. } = event {
                if let Some(ch) = text.chars().next() {
                    if (ch as u32) >= 32 {
                        self.jump_to_char(&ch.to_string(), jump_mode);
                        return;
                    }
                }
            }
        }

        if let InputEvent::Paste { text, .. } = event {
            if !text.is_empty() {
                self.handle_paste(text);
            }
            return;
        }

        let (
            is_copy,
            is_undo,
            is_select_cancel,
            is_select_up,
            is_select_down,
            is_tab,
            is_select_confirm,
        ) = {
            let kb = self
                .keybindings
                .lock()
                .expect("editor keybindings lock poisoned");
            (
                kb.matches(key_id, EditorAction::Copy),
                kb.matches(key_id, EditorAction::Undo),
                kb.matches(key_id, EditorAction::SelectCancel),
                kb.matches(key_id, EditorAction::SelectUp),
                kb.matches(key_id, EditorAction::SelectDown),
                kb.matches(key_id, EditorAction::Tab),
                kb.matches(key_id, EditorAction::SelectConfirm),
            )
        };

        if is_copy {
            return;
        }

        if is_undo {
            self.undo();
            return;
        }

        if self.autocomplete_state.is_some() {
            if is_select_cancel {
                self.cancel_autocomplete();
                return;
            }

            if is_select_up || is_select_down {
                if let Some(list) = self.autocomplete_list.as_mut() {
                    list.handle_event(event);
                    self.autocomplete_selection_changed = true;
                    self.autocomplete_selected_value =
                        list.get_selected_item().map(|item| item.value.clone());
                }
                return;
            }

            if is_tab {
                let selected = self
                    .autocomplete_list
                    .as_ref()
                    .and_then(|list| list.get_selected_item())
                    .cloned();
                if let Some(selected) = selected {
                    let item = AutocompleteItem {
                        value: selected.value.clone(),
                        label: selected.label.clone(),
                        description: selected.description.clone(),
                    };
                    if let Some(provider) = self.autocomplete_provider.as_ref() {
                        let result = provider.apply_completion(
                            &self.state.lines,
                            self.state.cursor_line,
                            self.state.cursor_col,
                            &item,
                            &self.autocomplete_prefix,
                        );
                        self.push_undo_snapshot();
                        self.last_action = None;
                        self.state.lines = result.lines;
                        self.state.cursor_line = result.cursor_line;
                        self.set_cursor_col(result.cursor_col);
                        self.cancel_autocomplete();
                        self.emit_change();
                    }
                }
                return;
            }

            if is_select_confirm {
                let mut fallthrough = false;
                let selected = self
                    .autocomplete_list
                    .as_ref()
                    .and_then(|list| list.get_selected_item())
                    .cloned();
                if let Some(selected) = selected {
                    let item = AutocompleteItem {
                        value: selected.value.clone(),
                        label: selected.label.clone(),
                        description: selected.description.clone(),
                    };
                    if let Some(provider) = self.autocomplete_provider.as_ref() {
                        let result = provider.apply_completion(
                            &self.state.lines,
                            self.state.cursor_line,
                            self.state.cursor_col,
                            &item,
                            &self.autocomplete_prefix,
                        );
                        self.push_undo_snapshot();
                        self.last_action = None;
                        self.state.lines = result.lines;
                        self.state.cursor_line = result.cursor_line;
                        self.set_cursor_col(result.cursor_col);

                        if self.autocomplete_prefix.starts_with('/') {
                            self.cancel_autocomplete();
                            fallthrough = true;
                        } else {
                            self.cancel_autocomplete();
                            self.emit_change();
                            return;
                        }
                    }
                }

                if !fallthrough {
                    return;
                }
            }
        }

        if self.autocomplete_state.is_none() && is_tab {
            self.handle_tab_completion();
            return;
        }

        enum Action {
            DeleteToLineEnd,
            DeleteToLineStart,
            DeleteWordBackward,
            DeleteWordForward,
            Backspace,
            ForwardDelete,
            Yank,
            YankPop,
            CursorLineStart,
            CursorLineEnd,
            CursorWordLeft,
            CursorWordRight,
            NewLine,
            Submit,
            CursorUp,
            CursorDown,
            CursorRight,
            CursorLeft,
            PageUp,
            PageDown,
            JumpForward,
            JumpBackward,
        }

        let action = {
            let kb = self
                .keybindings
                .lock()
                .expect("editor keybindings lock poisoned");

            if kb.matches(key_id, EditorAction::DeleteToLineEnd) {
                Some(Action::DeleteToLineEnd)
            } else if kb.matches(key_id, EditorAction::DeleteToLineStart) {
                Some(Action::DeleteToLineStart)
            } else if kb.matches(key_id, EditorAction::DeleteWordBackward) {
                Some(Action::DeleteWordBackward)
            } else if kb.matches(key_id, EditorAction::DeleteWordForward) {
                Some(Action::DeleteWordForward)
            } else if kb.matches(key_id, EditorAction::DeleteCharBackward)
                || key_id == Some("shift+backspace")
            {
                Some(Action::Backspace)
            } else if kb.matches(key_id, EditorAction::DeleteCharForward)
                || key_id == Some("shift+delete")
            {
                Some(Action::ForwardDelete)
            } else if kb.matches(key_id, EditorAction::Yank) {
                Some(Action::Yank)
            } else if kb.matches(key_id, EditorAction::YankPop) {
                Some(Action::YankPop)
            } else if kb.matches(key_id, EditorAction::CursorLineStart) {
                Some(Action::CursorLineStart)
            } else if kb.matches(key_id, EditorAction::CursorLineEnd) {
                Some(Action::CursorLineEnd)
            } else if kb.matches(key_id, EditorAction::CursorWordLeft) {
                Some(Action::CursorWordLeft)
            } else if kb.matches(key_id, EditorAction::CursorWordRight) {
                Some(Action::CursorWordRight)
            } else if kb.matches(key_id, EditorAction::NewLine) {
                Some(Action::NewLine)
            } else if kb.matches(key_id, EditorAction::Submit) {
                Some(Action::Submit)
            } else if kb.matches(key_id, EditorAction::CursorUp) {
                Some(Action::CursorUp)
            } else if kb.matches(key_id, EditorAction::CursorDown) {
                Some(Action::CursorDown)
            } else if kb.matches(key_id, EditorAction::CursorRight) {
                Some(Action::CursorRight)
            } else if kb.matches(key_id, EditorAction::CursorLeft) {
                Some(Action::CursorLeft)
            } else if kb.matches(key_id, EditorAction::PageUp) {
                Some(Action::PageUp)
            } else if kb.matches(key_id, EditorAction::PageDown) {
                Some(Action::PageDown)
            } else if kb.matches(key_id, EditorAction::JumpForward) {
                Some(Action::JumpForward)
            } else if kb.matches(key_id, EditorAction::JumpBackward) {
                Some(Action::JumpBackward)
            } else {
                None
            }
        };

        match action {
            Some(Action::DeleteToLineEnd) => {
                self.delete_to_end_of_line();
                return;
            }
            Some(Action::DeleteToLineStart) => {
                self.delete_to_start_of_line();
                return;
            }
            Some(Action::DeleteWordBackward) => {
                self.delete_word_backwards();
                return;
            }
            Some(Action::DeleteWordForward) => {
                self.delete_word_forwards();
                return;
            }
            Some(Action::Backspace) => {
                self.handle_backspace();
                return;
            }
            Some(Action::ForwardDelete) => {
                self.handle_forward_delete();
                return;
            }
            Some(Action::Yank) => {
                self.yank();
                return;
            }
            Some(Action::YankPop) => {
                self.yank_pop();
                return;
            }
            Some(Action::CursorLineStart) => {
                self.move_to_line_start();
                return;
            }
            Some(Action::CursorLineEnd) => {
                self.move_to_line_end();
                return;
            }
            Some(Action::CursorWordLeft) => {
                self.move_word_backwards();
                return;
            }
            Some(Action::CursorWordRight) => {
                self.move_word_forwards();
                return;
            }
            Some(Action::NewLine) => {
                self.add_new_line();
                return;
            }
            Some(Action::Submit) => {
                if self.disable_submit {
                    return;
                }

                let current_line = self
                    .state
                    .lines
                    .get(self.state.cursor_line)
                    .map(String::as_str)
                    .unwrap_or("");
                if self.state.cursor_col > 0
                    && current_line[..self.state.cursor_col].ends_with('\\')
                {
                    self.handle_backspace();
                    self.add_new_line();
                    return;
                }

                self.submit_value();
                return;
            }
            Some(Action::CursorUp) => {
                if self.is_editor_empty()
                    || (self.history_index > -1 && self.is_on_first_visual_line())
                {
                    self.navigate_history(-1);
                } else if self.is_on_first_visual_line() {
                    self.move_to_line_start();
                } else {
                    self.move_cursor(-1, 0);
                }
                return;
            }
            Some(Action::CursorDown) => {
                if self.history_index > -1 && self.is_on_last_visual_line() {
                    self.navigate_history(1);
                } else if self.is_on_last_visual_line() {
                    self.move_to_line_end();
                } else {
                    self.move_cursor(1, 0);
                }
                return;
            }
            Some(Action::CursorRight) => {
                self.move_cursor(0, 1);
                return;
            }
            Some(Action::CursorLeft) => {
                self.move_cursor(0, -1);
                return;
            }
            Some(Action::PageUp) => {
                self.page_scroll(-1);
                return;
            }
            Some(Action::PageDown) => {
                self.page_scroll(1);
                return;
            }
            Some(Action::JumpForward) => {
                self.jump_mode = Some(JumpMode::Forward);
                return;
            }
            Some(Action::JumpBackward) => {
                self.jump_mode = Some(JumpMode::Backward);
                return;
            }
            None => {}
        }

        if let InputEvent::Text { text, .. } = event {
            for ch in text.chars() {
                self.insert_character(&ch.to_string(), false);
            }
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
        if self
            .history
            .first()
            .map(|item| item == trimmed)
            .unwrap_or(false)
        {
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

    fn get_padding_x(&self) -> usize {
        Editor::get_padding_x(self)
    }

    fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        Editor::set_autocomplete_max_visible(self, max_visible);
    }

    fn get_autocomplete_max_visible(&self) -> usize {
        Editor::get_autocomplete_max_visible(self)
    }

    fn set_autocomplete_provider(&mut self, provider: Box<dyn AutocompleteProvider>) {
        Editor::set_autocomplete_provider(self, provider);
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

#[cfg(test)]
mod tests {
    use super::{
        word_wrap_line, Editor, EditorHeightMode, EditorOptions, EditorPasteMode, EditorTheme,
    };
    use crate::core::autocomplete::{
        AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
        CombinedAutocompleteProvider, CommandEntry, CompletionResult, SlashCommand,
    };
    use crate::core::component::Component;
    use crate::core::cursor::CursorPos;
    use crate::core::editor_component::EditorComponent;
    use crate::core::input_event::parse_input_events;
    use crate::default_editor_keybindings_handle;
    use crate::widgets::select_list::SelectListTheme;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    fn theme() -> EditorTheme {
        EditorTheme {
            border_color: Box::new(|text| text.to_string()),
            select_list: SelectListTheme {
                selected_prefix: Arc::new(|text| text.to_string()),
                selected_text: Arc::new(|text| text.to_string()),
                description: Arc::new(|text| text.to_string()),
                scroll_info: Arc::new(|text| text.to_string()),
                no_match: Arc::new(|text| text.to_string()),
            },
        }
    }

    fn send(editor: &mut Editor, data: &str) {
        for event in parse_input_events(data, false) {
            editor.handle_event(&event);
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
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.set_text("one\ntwo");
        editor.state.cursor_line = 0;
        editor.state.cursor_col = 3;

        send(&mut editor, "\x1b[C");
        assert_eq!(editor.get_cursor(), (1, 0));

        send(&mut editor, "\x1b[D");
        assert_eq!(editor.get_cursor(), (0, 3));
    }

    #[test]
    fn editor_scrolls_to_keep_cursor_visible() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.set_terminal_rows(10);
        let lines = (0..10).map(|idx| format!("line {idx}")).collect::<Vec<_>>();
        editor.state.lines = lines;
        editor.state.cursor_line = 7;
        editor.state.cursor_col = 0;

        let _ = editor.render(20);
        assert_eq!(editor.scroll_offset, 3);
    }

    #[test]
    fn editor_reports_cursor_pos_when_focused() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.state.lines = vec!["hi".to_string()];
        editor.state.cursor_line = 0;
        editor.state.cursor_col = 1;
        editor.focused = true;
        let lines = editor.render(10);
        assert!(!lines.iter().any(|line| line.contains("\x1b_pi:c")));
        assert_eq!(editor.cursor_pos(), Some(CursorPos { row: 1, col: 1 }));
    }

    #[test]
    fn editor_getters_reflect_options() {
        let options = EditorOptions {
            padding_x: Some(2),
            autocomplete_max_visible: Some(7),
            ..EditorOptions::default()
        };
        let editor = Editor::new(theme(), default_editor_keybindings_handle(), options);
        assert_eq!(editor.get_padding_x(), 2);
        assert_eq!(editor.get_autocomplete_max_visible(), 7);
    }

    #[test]
    fn editor_fill_available_renders_more_lines_than_default() {
        let text = (0..50)
            .map(|idx| format!("line {idx}"))
            .collect::<Vec<_>>()
            .join("\n");

        let mut default_editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        default_editor.set_terminal_rows(20);
        default_editor.set_text(&text);
        let default_lines = default_editor.render(20);

        let mut fill_editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions {
                height_mode: Some(EditorHeightMode::FillAvailable),
                ..EditorOptions::default()
            },
        );
        fill_editor.set_terminal_rows(20);
        fill_editor.set_text(&text);
        let fill_lines = fill_editor.render(20);

        assert_eq!(fill_lines.len(), 20);
        assert!(fill_lines.len() > default_lines.len());
    }

    #[test]
    fn editor_fill_available_pads_short_content_to_terminal_rows() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions {
                height_mode: Some(EditorHeightMode::FillAvailable),
                ..EditorOptions::default()
            },
        );
        editor.set_terminal_rows(20);
        editor.set_text("hi");

        let lines = editor.render(20);
        assert_eq!(lines.len(), 20);
        assert_eq!(lines.last().unwrap(), &"─".repeat(20));
    }

    #[test]
    fn editor_top_border_when_scrolled() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.set_terminal_rows(10);
        editor.state.lines = (0..10).map(|idx| format!("row {idx}")).collect();
        editor.state.cursor_line = 8;
        editor.state.cursor_col = 0;
        let lines = editor.render(20);
        assert!(lines[0].contains("↑"));
    }

    #[test]
    fn editor_undo_coalesces_words() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        for ch in "hello world".chars() {
            let input = ch.to_string();
            send(&mut editor, &input);
        }
        send(&mut editor, "\x1f"); // ctrl+-
        assert_eq!(editor.get_text(), "hello");
        send(&mut editor, "\x1f");
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn editor_kill_and_yank_restore_line() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.set_text("hello world");
        editor.state.cursor_line = 0;
        editor.set_cursor_col(5);

        send(&mut editor, "\x0b"); // ctrl+k
        assert_eq!(editor.get_text(), "hello");

        send(&mut editor, "\x19"); // ctrl+y
        assert_eq!(editor.get_text(), "hello world");
    }

    #[test]
    fn editor_history_navigation_bounds_and_exits_on_edit() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        EditorComponent::add_to_history(&mut editor, "first");
        EditorComponent::add_to_history(&mut editor, "second");

        send(&mut editor, "\x1b[A");
        assert_eq!(editor.get_text(), "second");
        assert_eq!(editor.history_index, 0);

        send(&mut editor, "\x1b[A");
        assert_eq!(editor.get_text(), "first");
        assert_eq!(editor.history_index, 1);

        send(&mut editor, "\x1b[A");
        assert_eq!(editor.get_text(), "first");
        assert_eq!(editor.history_index, 1);

        send(&mut editor, "\x1b[B");
        assert_eq!(editor.get_text(), "second");
        assert_eq!(editor.history_index, 0);

        send(&mut editor, "\x1b[B");
        assert_eq!(editor.get_text(), "");
        assert_eq!(editor.history_index, -1);

        send(&mut editor, "\x1b[A");
        assert_eq!(editor.get_text(), "second");
        assert_eq!(editor.history_index, 0);

        send(&mut editor, "!");
        assert_eq!(editor.get_text(), "second!");
        assert_eq!(editor.history_index, -1);

        send(&mut editor, "\x1b[B");
        assert_eq!(editor.get_text(), "second!");
        assert_eq!(editor.history_index, -1);
    }

    #[test]
    fn editor_grapheme_movement_and_deletion_are_cluster_aware() {
        let emoji = "👨‍👩‍👧‍👦";
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );

        editor.set_text(&format!("a{emoji}b"));
        editor.state.cursor_line = 0;
        editor.set_cursor_col(editor.state.lines[0].len());

        editor.move_cursor(0, -1);
        assert_eq!(editor.get_cursor(), (0, format!("a{emoji}").len()));
        editor.move_cursor(0, -1);
        assert_eq!(editor.get_cursor(), (0, 1));
        editor.move_cursor(0, 1);
        assert_eq!(editor.get_cursor(), (0, 1 + emoji.len()));

        editor.set_text(&format!("a{emoji}b"));
        editor.state.cursor_line = 0;
        editor.set_cursor_col(1 + emoji.len());
        editor.handle_backspace();
        assert_eq!(editor.get_text(), "ab");
        assert_eq!(editor.get_cursor(), (0, 1));

        editor.set_text(&format!("a{emoji}b"));
        editor.state.cursor_line = 0;
        editor.set_cursor_col(1);
        editor.handle_forward_delete();
        assert_eq!(editor.get_text(), "ab");
        assert_eq!(editor.get_cursor(), (0, 1));
    }

    #[test]
    fn editor_word_navigation_and_deletion_handle_punctuation_and_multiline() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );

        editor.set_text("foo,  bar");
        editor.state.cursor_line = 0;
        editor.set_cursor_col("foo,  bar".len());

        editor.move_word_backwards();
        assert_eq!(editor.get_cursor(), (0, 6));
        editor.move_word_backwards();
        assert_eq!(editor.get_cursor(), (0, 3));
        editor.move_word_backwards();
        assert_eq!(editor.get_cursor(), (0, 0));

        editor.move_word_forwards();
        assert_eq!(editor.get_cursor(), (0, 3));
        editor.move_word_forwards();
        assert_eq!(editor.get_cursor(), (0, 4));
        editor.move_word_forwards();
        assert_eq!(editor.get_cursor(), (0, "foo,  bar".len()));

        editor.set_text("foo,bar");
        editor.state.cursor_line = 0;
        editor.set_cursor_col(4);
        editor.delete_word_backwards();
        assert_eq!(editor.get_text(), "foobar");
        assert_eq!(editor.get_cursor(), (0, 3));

        editor.set_text("left\nright");
        editor.state.cursor_line = 1;
        editor.set_cursor_col(0);
        editor.delete_word_backwards();
        assert_eq!(editor.get_text(), "leftright");
        assert_eq!(editor.get_cursor(), (0, 4));

        editor.set_text("left\nright");
        editor.state.cursor_line = 0;
        editor.set_cursor_col(4);
        editor.delete_word_forwards();
        assert_eq!(editor.get_text(), "leftright");
        assert_eq!(editor.get_cursor(), (0, 4));
    }

    #[test]
    fn editor_yank_pop_rotates_kill_ring_entries() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.add_to_kill_ring("one", false);
        editor.last_action = None;
        editor.add_to_kill_ring("two", false);

        editor.yank();
        assert_eq!(editor.get_text(), "two");

        editor.yank_pop();
        assert_eq!(editor.get_text(), "one");

        editor.yank_pop();
        assert_eq!(editor.get_text(), "two");
    }

    #[test]
    fn editor_undo_breaks_coalescing_after_cursor_move() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        for ch in "abc".chars() {
            send(&mut editor, &ch.to_string());
        }
        send(&mut editor, "\x1b[D");
        send(&mut editor, "X");
        assert_eq!(editor.get_text(), "abXc");

        send(&mut editor, "\x1f"); // ctrl+-
        assert_eq!(editor.get_text(), "abc");

        send(&mut editor, "\x1f"); // ctrl+-
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn editor_large_paste_inserts_marker_and_expands() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        let lines = (0..11).map(|idx| format!("line{idx}")).collect::<Vec<_>>();
        let paste = lines.join("\n");
        let input = format!("\x1b[200~{paste}\x1b[201~");
        send(&mut editor, &input);
        let text = editor.get_text();
        assert!(text.contains("[paste #1 +11 lines]"));
        assert_eq!(editor.get_expanded_text(), paste);
    }

    #[test]
    fn editor_large_paste_in_literal_mode_inserts_full_text() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions {
                paste_mode: Some(EditorPasteMode::Literal),
                ..EditorOptions::default()
            },
        );
        let lines = (0..11).map(|idx| format!("line{idx}")).collect::<Vec<_>>();
        let paste = lines.join("\n");
        let input = format!("\x1b[200~{paste}\x1b[201~");
        send(&mut editor, &input);
        let text = editor.get_text();
        assert!(text.contains('\n'));
        assert!(!text.contains("[paste #"));
        assert_eq!(text, paste);
    }

    #[test]
    fn editor_autocomplete_tab_applies_completion() {
        let command = SlashCommand {
            name: "help".to_string(),
            description: None,
            get_argument_completions: None,
        };
        let provider = CombinedAutocompleteProvider::new(
            vec![CommandEntry::Command(command)],
            PathBuf::from("."),
            None,
        );
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.set_autocomplete_provider(Box::new(provider));

        let submitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let submitted_ref = submitted.clone();
        editor.set_on_submit(Some(Box::new(move |text| {
            submitted_ref.borrow_mut().push(text);
        })));

        send(&mut editor, "/");
        assert!(editor.autocomplete_state.is_some());

        send(&mut editor, "\t");
        assert_eq!(editor.get_text(), "/help ");
        assert!(editor.autocomplete_state.is_none());
        assert!(submitted.borrow().is_empty());
    }

    #[test]
    fn editor_autocomplete_enter_submits_slash_command() {
        let command = SlashCommand {
            name: "help".to_string(),
            description: None,
            get_argument_completions: None,
        };
        let provider = CombinedAutocompleteProvider::new(
            vec![CommandEntry::Command(command)],
            PathBuf::from("."),
            None,
        );
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.set_autocomplete_provider(Box::new(provider));

        let submitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let submitted_ref = submitted.clone();
        editor.set_on_submit(Some(Box::new(move |text| {
            submitted_ref.borrow_mut().push(text);
        })));

        send(&mut editor, "/");
        send(&mut editor, "\r");

        assert_eq!(submitted.borrow().as_slice(), &["/help"]);
        assert_eq!(editor.get_text(), "");
    }

    struct AsyncAutocompleteProvider;

    impl AutocompleteProvider for AsyncAutocompleteProvider {
        fn get_suggestions(
            &self,
            _lines: &[String],
            _cursor_line: usize,
            _cursor_col: usize,
        ) -> Option<AutocompleteSuggestions> {
            None
        }

        fn get_suggestions_async(
            &self,
            _lines: Vec<String>,
            _cursor_line: usize,
            _cursor_col: usize,
            _signal: Option<crate::core::autocomplete::AbortSignal>,
            on_update: Option<crate::core::autocomplete::SuggestionUpdate>,
        ) -> Option<std::thread::JoinHandle<Option<AutocompleteSuggestions>>> {
            Some(thread::spawn(move || {
                if let Some(update) = on_update {
                    let suggestions = AutocompleteSuggestions {
                        items: vec![AutocompleteItem {
                            value: "@alpha".to_string(),
                            label: "alpha".to_string(),
                            description: None,
                        }],
                        prefix: "@".to_string(),
                    };
                    update(suggestions);
                }
                None
            }))
        }

        fn apply_completion(
            &self,
            lines: &[String],
            cursor_line: usize,
            cursor_col: usize,
            _item: &AutocompleteItem,
            _prefix: &str,
        ) -> CompletionResult {
            CompletionResult {
                lines: lines.to_vec(),
                cursor_line,
                cursor_col,
            }
        }
    }

    #[test]
    fn editor_async_autocomplete_updates_apply() {
        let mut editor = Editor::new(
            theme(),
            default_editor_keybindings_handle(),
            EditorOptions::default(),
        );
        editor.set_autocomplete_provider(Box::new(AsyncAutocompleteProvider));

        send(&mut editor, "@");
        thread::sleep(Duration::from_millis(20));
        editor.poll_autocomplete_async();

        assert!(editor.autocomplete_state.is_some());
        let selected = editor
            .autocomplete_list
            .as_ref()
            .and_then(|list| list.get_selected_item())
            .expect("expected autocomplete list");
        assert_eq!(selected.value, "@alpha");
    }
}
