//! Input widget (Phase 19).

use crate::core::component::{Component, Focusable};
use crate::core::keybindings::{get_editor_keybindings, EditorAction};
use crate::render::utils::{grapheme_segments, is_punctuation_char, is_whitespace_char};
use crate::render::width::visible_width;

const CURSOR_MARKER: &str = "\x1b_pi:c\x07";
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// Single-line input component with horizontal scrolling.
pub struct Input {
    value: String,
    cursor: usize,
    focused: bool,
    prompt: String,
    on_submit: Option<Box<dyn FnMut(String)>>,
    on_escape: Option<Box<dyn FnMut()>>,
    paste_buffer: String,
    is_in_paste: bool,
}

impl Input {
    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            focused: false,
            prompt: "> ".to_string(),
            on_submit: None,
            on_escape: None,
            paste_buffer: String::new(),
            is_in_paste: false,
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
        let cleaned = pasted_text
            .replace("\r\n", "")
            .replace('\r', "")
            .replace('\n', "");
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

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Input {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.clamp_cursor();

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
                let start = find_valid_start(&self.value, self.value.len().saturating_sub(scroll_width));
                let text = self.value[start..].to_string();
                let cursor = self.cursor.saturating_sub(start);
                (text, cursor)
            } else {
                let start = find_valid_start(&self.value, self.cursor.saturating_sub(half_width));
                let end = find_valid_end(&self.value, start.saturating_add(scroll_width).min(self.value.len()));
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

        let (at_cursor, after_cursor) = if let Some(grapheme) = cursor_grapheme {
            let after_start = cursor_display + grapheme.len();
            let after_cursor = &visible_text[after_start..];
            (grapheme, after_cursor)
        } else {
            (" ", "")
        };

        let marker = if self.focused { CURSOR_MARKER } else { "" };
        let cursor_char = format!("\x1b[7m{at_cursor}\x1b[27m");
        let mut text_with_cursor = String::with_capacity(visible_text.len() + marker.len() + cursor_char.len());
        text_with_cursor.push_str(before_cursor);
        text_with_cursor.push_str(marker);
        text_with_cursor.push_str(&cursor_char);
        text_with_cursor.push_str(after_cursor);

        let visual_length = visible_width(&text_with_cursor);
        let padding = " ".repeat(available_width.saturating_sub(visual_length));
        let line = format!("{prompt}{text_with_cursor}{padding}");

        vec![line]
    }

    fn handle_input(&mut self, data: &str) {
        self.clamp_cursor();

        let mut data = data.to_string();

        if data.contains(PASTE_START) {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            data = data.replacen(PASTE_START, "", 1);
        }

        if self.is_in_paste {
            self.paste_buffer.push_str(&data);
            if let Some(end_index) = self.paste_buffer.find(PASTE_END) {
                let paste_content = self.paste_buffer[..end_index].to_string();
                self.handle_paste(&paste_content);
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

        if kb.matches(&data, EditorAction::SelectCancel) {
            if let Some(handler) = self.on_escape.as_mut() {
                handler();
            }
            return;
        }

        if kb.matches(&data, EditorAction::Submit) || data == "\n" {
            if let Some(handler) = self.on_submit.as_mut() {
                handler(self.value.clone());
            }
            return;
        }

        if kb.matches(&data, EditorAction::DeleteCharBackward) {
            if self.cursor > 0 {
                let before_cursor = &self.value[..self.cursor];
                let last = grapheme_segments(before_cursor).last();
                let grapheme_len = last.map(|segment| segment.len()).unwrap_or(1);
                let start = self.cursor.saturating_sub(grapheme_len);
                self.value.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            return;
        }

        if kb.matches(&data, EditorAction::DeleteCharForward) {
            if self.cursor < self.value.len() {
                let after_cursor = &self.value[self.cursor..];
                let first = grapheme_segments(after_cursor).next();
                let grapheme_len = first.map(|segment| segment.len()).unwrap_or(1);
                let end = (self.cursor + grapheme_len).min(self.value.len());
                self.value.replace_range(self.cursor..end, "");
            }
            return;
        }

        if kb.matches(&data, EditorAction::DeleteWordBackward) {
            self.delete_word_backwards();
            return;
        }

        if kb.matches(&data, EditorAction::DeleteToLineStart) {
            self.value = self.value[self.cursor..].to_string();
            self.cursor = 0;
            return;
        }

        if kb.matches(&data, EditorAction::DeleteToLineEnd) {
            self.value = self.value[..self.cursor].to_string();
            return;
        }

        if kb.matches(&data, EditorAction::CursorLeft) {
            if self.cursor > 0 {
                let before_cursor = &self.value[..self.cursor];
                let last = grapheme_segments(before_cursor).last();
                let grapheme_len = last.map(|segment| segment.len()).unwrap_or(1);
                self.cursor = self.cursor.saturating_sub(grapheme_len);
            }
            return;
        }

        if kb.matches(&data, EditorAction::CursorRight) {
            if self.cursor < self.value.len() {
                let after_cursor = &self.value[self.cursor..];
                let first = grapheme_segments(after_cursor).next();
                let grapheme_len = first.map(|segment| segment.len()).unwrap_or(1);
                self.cursor = (self.cursor + grapheme_len).min(self.value.len());
            }
            return;
        }

        if kb.matches(&data, EditorAction::CursorLineStart) {
            self.cursor = 0;
            return;
        }

        if kb.matches(&data, EditorAction::CursorLineEnd) {
            self.cursor = self.value.len();
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

        let has_control_chars = data.chars().any(|ch| {
            let code = ch as u32;
            code < 32 || code == 0x7f || (code >= 0x80 && code <= 0x9f)
        });
        if !has_control_chars {
            self.insert_text(&data);
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

    #[test]
    fn input_edits_and_moves_cursor() {
        let mut input = Input::new();
        input.handle_input("h");
        input.handle_input("e");
        input.handle_input("l");
        input.handle_input("l");
        input.handle_input("o");
        assert_eq!(input.get_value(), "hello");
        assert_eq!(input.cursor, 5);

        input.handle_input("\x1b[D");
        input.handle_input("\x1b[D");
        assert_eq!(input.cursor, 3);

        input.handle_input("p");
        assert_eq!(input.get_value(), "helplo");
        assert_eq!(input.cursor, 4);

        input.handle_input("\x7f");
        assert_eq!(input.get_value(), "hello");
        assert_eq!(input.cursor, 3);

        input.handle_input("\x1b[C");
        input.handle_input("\x1b[C");
        assert_eq!(input.cursor, 5);
    }

    #[test]
    fn input_paste_and_delete_word() {
        let mut input = Input::new();
        input.handle_input("\x1b[200~hello\nworld\x1b[201~");
        assert_eq!(input.get_value(), "helloworld");

        input.handle_input(" ");
        input.handle_input("there");
        input.handle_input("\x17");
        assert_eq!(input.get_value(), "helloworld ");
        assert_eq!(input.cursor, "helloworld ".len());
    }

    #[test]
    fn input_has_prompt_by_default() {
        let mut input = Input::new();
        let lines = input.render(10);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("> "));
    }
}
