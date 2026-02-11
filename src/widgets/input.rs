//! Input widget.

use crate::core::component::{Component, Focusable};
use crate::core::cursor::CursorPos;
use crate::core::input_event::InputEvent;
use crate::core::keybindings::{EditorAction, EditorKeybindingsHandle};
use crate::core::text::utils::{grapheme_segments, is_punctuation_char, is_whitespace_char};
use crate::core::text::width::visible_width;

/// Single-line input component with horizontal scrolling.
pub struct Input {
    value: String,
    cursor: usize,
    focused: bool,
    last_cursor_pos: Option<CursorPos>,
    prompt: String,
    keybindings: EditorKeybindingsHandle,
    on_submit: Option<Box<dyn FnMut(String)>>,
    on_escape: Option<Box<dyn FnMut()>>,
}

impl Input {
    pub fn new(keybindings: EditorKeybindingsHandle) -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            focused: false,
            last_cursor_pos: None,
            prompt: "> ".to_string(),
            keybindings,
            on_submit: None,
            on_escape: None,
        }
    }

    pub fn get_value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.cursor.min(self.value.len());
        self.clamp_cursor();
    }

    pub fn set_prompt(&mut self, prompt: impl Into<String>) {
        self.prompt = prompt.into();
    }

    pub fn set_on_submit(&mut self, handler: Option<Box<dyn FnMut(String)>>) {
        self.on_submit = handler;
    }

    pub fn set_on_escape(&mut self, handler: Option<Box<dyn FnMut()>>) {
        self.on_escape = handler;
    }

    fn clamp_cursor(&mut self) {
        if self.cursor > self.value.len() {
            self.cursor = self.value.len();
        }
        while self.cursor > 0 && !self.value.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
    }

    fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut next = String::with_capacity(self.value.len() + text.len());
        next.push_str(&self.value[..self.cursor]);
        next.push_str(text);
        next.push_str(&self.value[self.cursor..]);
        self.value = next;
        self.cursor += text.len();
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        let cleaned = pasted_text.replace(['\r', '\n'], "");
        self.insert_text(&cleaned);
    }

    fn is_whitespace_segment(segment: &str) -> bool {
        segment.chars().any(is_whitespace_char)
    }

    fn is_punctuation_segment(segment: &str) -> bool {
        segment.chars().any(is_punctuation_char)
    }

    fn delete_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let old_cursor = self.cursor;
        self.move_word_backwards();
        let delete_from = self.cursor;
        self.cursor = old_cursor;
        self.value.replace_range(delete_from..self.cursor, "");
        self.cursor = delete_from;
    }

    fn move_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let text_before_cursor = &self.value[..self.cursor];
        let mut graphemes: Vec<&str> = grapheme_segments(text_before_cursor).collect();

        while let Some(last) = graphemes.last() {
            if Self::is_whitespace_segment(last) {
                self.cursor = self.cursor.saturating_sub(last.len());
                graphemes.pop();
            } else {
                break;
            }
        }

        if let Some(last) = graphemes.last() {
            if Self::is_punctuation_segment(last) {
                while let Some(last) = graphemes.last() {
                    if Self::is_punctuation_segment(last) {
                        self.cursor = self.cursor.saturating_sub(last.len());
                        graphemes.pop();
                    } else {
                        break;
                    }
                }
            } else {
                while let Some(last) = graphemes.last() {
                    if !Self::is_whitespace_segment(last) && !Self::is_punctuation_segment(last) {
                        self.cursor = self.cursor.saturating_sub(last.len());
                        graphemes.pop();
                    } else {
                        break;
                    }
                }
            }
        }
    }

    fn move_word_forwards(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }
        let text_after_cursor = &self.value[self.cursor..];
        let mut iter = grapheme_segments(text_after_cursor);
        let mut next = iter.next();

        while let Some(seg) = next {
            if Self::is_whitespace_segment(seg) {
                self.cursor += seg.len();
                next = iter.next();
            } else {
                break;
            }
        }

        if let Some(seg) = next {
            if Self::is_punctuation_segment(seg) {
                let mut current = Some(seg);
                while let Some(seg) = current {
                    if Self::is_punctuation_segment(seg) {
                        self.cursor += seg.len();
                        current = iter.next();
                    } else {
                        break;
                    }
                }
            } else {
                let mut current = Some(seg);
                while let Some(seg) = current {
                    if !Self::is_whitespace_segment(seg) && !Self::is_punctuation_segment(seg) {
                        self.cursor += seg.len();
                        current = iter.next();
                    } else {
                        break;
                    }
                }
            }
        }
    }
}

impl Component for Input {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.clamp_cursor();
        self.last_cursor_pos = None;

        let prompt = &self.prompt;
        let available_width = width.saturating_sub(prompt.len());
        if available_width == 0 {
            return vec![prompt.to_string()];
        }

        let (visible_text, cursor_display) = if self.value.len() < available_width {
            (self.value.clone(), self.cursor)
        } else {
            let scroll_width = if self.cursor == self.value.len() {
                available_width.saturating_sub(1)
            } else {
                available_width
            };
            let half_width = scroll_width / 2;

            let find_valid_start = |value: &str, mut start: usize| {
                while start < value.len() && !value.is_char_boundary(start) {
                    start += 1;
                }
                start
            };

            let find_valid_end = |value: &str, mut end: usize| {
                while end > 0 && !value.is_char_boundary(end) {
                    end -= 1;
                }
                end
            };

            if self.cursor < half_width {
                let end = find_valid_end(&self.value, scroll_width.min(self.value.len()));
                let text = self.value[..end].to_string();
                let cursor = self.cursor.min(text.len());
                (text, cursor)
            } else if self.cursor > self.value.len().saturating_sub(half_width) {
                let start =
                    find_valid_start(&self.value, self.value.len().saturating_sub(scroll_width));
                let text = self.value[start..].to_string();
                let cursor = self.cursor.saturating_sub(start);
                (text, cursor)
            } else {
                let start = find_valid_start(&self.value, self.cursor.saturating_sub(half_width));
                let end = find_valid_end(
                    &self.value,
                    start.saturating_add(scroll_width).min(self.value.len()),
                );
                let text = self.value[start..end].to_string();
                let mut cursor = self.cursor.saturating_sub(start);
                if !text.is_char_boundary(cursor) {
                    while cursor > 0 && !text.is_char_boundary(cursor) {
                        cursor -= 1;
                    }
                }
                (text, cursor)
            }
        };

        let cursor_display = cursor_display.min(visible_text.len());
        let before_cursor = &visible_text[..cursor_display];
        let after_slice = &visible_text[cursor_display..];
        let mut graphemes = grapheme_segments(after_slice);
        let cursor_grapheme = graphemes.next();

        self.last_cursor_pos = if self.focused {
            let col = visible_width(prompt).saturating_add(visible_width(before_cursor));
            Some(CursorPos { row: 0, col })
        } else {
            None
        };

        let (at_cursor, after_cursor) = if let Some(grapheme) = cursor_grapheme {
            let after_start = cursor_display + grapheme.len();
            let after_cursor = &visible_text[after_start..];
            (grapheme, after_cursor)
        } else {
            (" ", "")
        };

        let cursor_char = format!("\x1b[7m{at_cursor}\x1b[27m");
        let mut text_with_cursor = String::with_capacity(visible_text.len() + cursor_char.len());
        text_with_cursor.push_str(before_cursor);
        text_with_cursor.push_str(&cursor_char);
        text_with_cursor.push_str(after_cursor);

        let visual_length = visible_width(&text_with_cursor);
        let padding = " ".repeat(available_width.saturating_sub(visual_length));
        let line = format!("{prompt}{text_with_cursor}{padding}");

        vec![line]
    }

    fn cursor_pos(&self) -> Option<CursorPos> {
        self.last_cursor_pos
    }

    fn handle_event(&mut self, event: &InputEvent) {
        self.clamp_cursor();

        let key_id = match event {
            InputEvent::Text { text, .. } => {
                self.insert_text(text);
                return;
            }
            InputEvent::Paste { text, .. } => {
                self.handle_paste(text);
                return;
            }
            InputEvent::Key { key_id, .. } => Some(key_id.as_str()),
            _ => return,
        };

        let (
            is_cancel,
            is_submit,
            is_delete_backward,
            is_delete_forward,
            is_delete_word_backward,
            is_delete_to_line_start,
            is_delete_to_line_end,
            is_left,
            is_right,
            is_line_start,
            is_line_end,
            is_word_left,
            is_word_right,
        ) = {
            let kb = self
                .keybindings
                .lock()
                .expect("editor keybindings lock poisoned");
            (
                kb.matches(key_id, EditorAction::SelectCancel),
                kb.matches(key_id, EditorAction::Submit),
                kb.matches(key_id, EditorAction::DeleteCharBackward),
                kb.matches(key_id, EditorAction::DeleteCharForward),
                kb.matches(key_id, EditorAction::DeleteWordBackward),
                kb.matches(key_id, EditorAction::DeleteToLineStart),
                kb.matches(key_id, EditorAction::DeleteToLineEnd),
                kb.matches(key_id, EditorAction::CursorLeft),
                kb.matches(key_id, EditorAction::CursorRight),
                kb.matches(key_id, EditorAction::CursorLineStart),
                kb.matches(key_id, EditorAction::CursorLineEnd),
                kb.matches(key_id, EditorAction::CursorWordLeft),
                kb.matches(key_id, EditorAction::CursorWordRight),
            )
        };

        if is_cancel {
            if let Some(handler) = self.on_escape.as_mut() {
                handler();
            }
            return;
        }

        if is_submit || key_id == Some("shift+enter") {
            if let Some(handler) = self.on_submit.as_mut() {
                handler(self.value.clone());
            }
            return;
        }

        if is_delete_backward {
            if self.cursor > 0 {
                let before_cursor = &self.value[..self.cursor];
                let last = grapheme_segments(before_cursor).next_back();
                let grapheme_len = last.map(|segment| segment.len()).unwrap_or(1);
                let start = self.cursor.saturating_sub(grapheme_len);
                self.value.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            return;
        }

        if is_delete_forward {
            if self.cursor < self.value.len() {
                let after_cursor = &self.value[self.cursor..];
                let first = grapheme_segments(after_cursor).next();
                let grapheme_len = first.map(|segment| segment.len()).unwrap_or(1);
                let end = (self.cursor + grapheme_len).min(self.value.len());
                self.value.replace_range(self.cursor..end, "");
            }
            return;
        }

        if is_delete_word_backward {
            self.delete_word_backwards();
            return;
        }

        if is_delete_to_line_start {
            self.value = self.value[self.cursor..].to_string();
            self.cursor = 0;
            return;
        }

        if is_delete_to_line_end {
            self.value = self.value[..self.cursor].to_string();
            return;
        }

        if is_left {
            if self.cursor > 0 {
                let before_cursor = &self.value[..self.cursor];
                let last = grapheme_segments(before_cursor).next_back();
                let grapheme_len = last.map(|segment| segment.len()).unwrap_or(1);
                self.cursor = self.cursor.saturating_sub(grapheme_len);
            }
            return;
        }

        if is_right {
            if self.cursor < self.value.len() {
                let after_cursor = &self.value[self.cursor..];
                let first = grapheme_segments(after_cursor).next();
                let grapheme_len = first.map(|segment| segment.len()).unwrap_or(1);
                self.cursor = (self.cursor + grapheme_len).min(self.value.len());
            }
            return;
        }

        if is_line_start {
            self.cursor = 0;
            return;
        }

        if is_line_end {
            self.cursor = self.value.len();
            return;
        }

        if is_word_left {
            self.move_word_backwards();
            return;
        }

        if is_word_right {
            self.move_word_forwards();
        }
    }

    fn invalidate(&mut self) {
        // No cached state to invalidate.
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for Input {
    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }
}

#[cfg(test)]
mod tests {
    use super::Input;
    use crate::core::component::Component;
    use crate::core::input_event::parse_input_events;
    use crate::default_editor_keybindings_handle;

    fn send(input: &mut Input, data: &str) {
        for event in parse_input_events(data, false) {
            input.handle_event(&event);
        }
    }

    #[test]
    fn input_edits_and_moves_cursor() {
        let mut input = Input::new(default_editor_keybindings_handle());
        send(&mut input, "h");
        send(&mut input, "e");
        send(&mut input, "l");
        send(&mut input, "l");
        send(&mut input, "o");
        assert_eq!(input.get_value(), "hello");
        assert_eq!(input.cursor, 5);

        send(&mut input, "\x1b[D");
        send(&mut input, "\x1b[D");
        assert_eq!(input.cursor, 3);

        send(&mut input, "p");
        assert_eq!(input.get_value(), "helplo");
        assert_eq!(input.cursor, 4);

        send(&mut input, "\x7f");
        assert_eq!(input.get_value(), "hello");
        assert_eq!(input.cursor, 3);

        send(&mut input, "\x1b[C");
        send(&mut input, "\x1b[C");
        assert_eq!(input.cursor, 5);
    }

    #[test]
    fn input_paste_and_delete_word() {
        let mut input = Input::new(default_editor_keybindings_handle());
        send(&mut input, "\x1b[200~hello\nworld\x1b[201~");
        assert_eq!(input.get_value(), "helloworld");

        send(&mut input, " ");
        send(&mut input, "there");
        send(&mut input, "\x17");
        assert_eq!(input.get_value(), "helloworld ");
        assert_eq!(input.cursor, "helloworld ".len());
    }

    #[test]
    fn input_has_prompt_by_default() {
        let mut input = Input::new(default_editor_keybindings_handle());
        let lines = input.render(10);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("> "));
    }
}
