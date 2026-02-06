//! Width-aware slicing utilities (Phase 3).

use unicode_segmentation::UnicodeSegmentation;

use super::ansi::{extract_ansi_code, AnsiCodeTracker};
use super::width::{grapheme_width, visible_width};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceResult {
    pub text: String,
    pub width: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segments {
    pub before: String,
    pub before_width: usize,
    pub after: String,
    pub after_width: usize,
}

pub fn slice_by_column(line: &str, start_col: usize, length: usize, strict: bool) -> String {
    slice_with_width(line, start_col, length, strict).text
}

pub fn slice_with_width(line: &str, start_col: usize, length: usize, strict: bool) -> SliceResult {
    if length == 0 {
        return SliceResult {
            text: String::new(),
            width: 0,
        };
    }

    let end_col = start_col.saturating_add(length);
    let mut result = String::new();
    let mut result_width = 0;
    let mut current_col = 0;
    let mut idx = 0;
    let mut pending_ansi = String::new();

    while idx < line.len() && current_col < end_col {
        if let Some(ansi) = extract_ansi_code(line, idx) {
            if current_col >= start_col && current_col < end_col {
                result.push_str(&ansi.code);
            } else if current_col < start_col {
                pending_ansi.push_str(&ansi.code);
            }
            idx += ansi.length;
            continue;
        }

        let text_end = next_ansi_or_end(line, idx);
        for grapheme in line[idx..text_end].graphemes(true) {
            let width = grapheme_width(grapheme);
            let in_range = current_col >= start_col && current_col < end_col;
            let fits = !strict || current_col + width <= end_col;

            if in_range && fits {
                if !pending_ansi.is_empty() {
                    result.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                result.push_str(grapheme);
                result_width += width;
            }

            current_col += width;
            if current_col >= end_col {
                break;
            }
        }
        idx = text_end;
    }

    SliceResult {
        text: result,
        width: result_width,
    }
}

pub fn extract_segments(
    line: &str,
    before_end: usize,
    after_start: usize,
    after_len: usize,
    strict_after: bool,
) -> Segments {
    let mut before = String::new();
    let mut after = String::new();
    let mut before_width = 0;
    let mut after_width = 0;

    let mut tracker = AnsiCodeTracker::default();
    let mut current_col = 0;
    let mut idx = 0;
    let mut pending_ansi_before = String::new();
    let mut after_started = false;
    let after_end = after_start.saturating_add(after_len);

    while idx < line.len() {
        if let Some(ansi) = extract_ansi_code(line, idx) {
            tracker.process(&ansi.code);
            if current_col < before_end {
                pending_ansi_before.push_str(&ansi.code);
            } else if current_col >= after_start && current_col < after_end && after_started {
                after.push_str(&ansi.code);
            }
            idx += ansi.length;
            continue;
        }

        let text_end = next_ansi_or_end(line, idx);
        for grapheme in line[idx..text_end].graphemes(true) {
            let width = grapheme_width(grapheme);

            if current_col < before_end {
                if !pending_ansi_before.is_empty() {
                    before.push_str(&pending_ansi_before);
                    pending_ansi_before.clear();
                }
                before.push_str(grapheme);
                before_width += width;
            } else if current_col >= after_start && current_col < after_end && after_len > 0 {
                let fits = !strict_after || current_col + width <= after_end;
                if fits {
                    if !after_started {
                        after.push_str(&tracker.active_codes());
                        after_started = true;
                    }
                    after.push_str(grapheme);
                    after_width += width;
                }
            }

            current_col += width;
            if after_len == 0 {
                if current_col >= before_end {
                    break;
                }
            } else if current_col >= after_end {
                break;
            }
        }

        idx = text_end;
        if after_len == 0 {
            if current_col >= before_end {
                break;
            }
        } else if current_col >= after_end {
            break;
        }
    }

    Segments {
        before,
        before_width,
        after,
        after_width,
    }
}

pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    if width == 0 {
        return vec![String::new()];
    }

    let mut result = Vec::new();
    let mut tracker = AnsiCodeTracker::default();

    for input_line in text.split('\n') {
        let prefix = if result.is_empty() {
            String::new()
        } else {
            tracker.active_codes()
        };
        let line = format!("{}{}", prefix, input_line);
        let mut wrapped = wrap_single_line(&line, width);
        result.append(&mut wrapped);
        update_tracker_from_text(input_line, &mut tracker);
    }

    if result.is_empty() {
        vec![String::new()]
    } else {
        result
            .into_iter()
            .map(|line| line.trim_end().to_string())
            .collect()
    }
}

fn wrap_single_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }

    let line_width = visible_width(line);
    if line_width <= width {
        return vec![line.to_string()];
    }

    let tokens = split_into_tokens_with_ansi(line);
    let mut tracker = AnsiCodeTracker::default();
    let mut wrapped = Vec::new();

    let mut current_line = String::new();
    let mut current_width = 0;

    for token in tokens {
        let token_width = visible_width(&token);
        let is_whitespace = token.trim().is_empty();

        if token_width > width && !is_whitespace {
            if !current_line.is_empty() {
                let mut line_to_wrap = current_line.trim_end().to_string();
                let reset = tracker.line_end_reset();
                if !reset.is_empty() {
                    line_to_wrap.push_str(&reset);
                }
                wrapped.push(line_to_wrap);
                current_line.clear();
                current_width = 0;
            }

            let broken = break_long_word(&token, width, &mut tracker);
            if let Some((last, rest)) = broken.split_last() {
                wrapped.extend_from_slice(rest);
                current_line = last.clone();
                current_width = visible_width(&current_line);
            }
            continue;
        }

        let total_needed = current_width + token_width;
        if total_needed > width && current_width > 0 {
            let mut line_to_wrap = current_line.trim_end().to_string();
            let reset = tracker.line_end_reset();
            if !reset.is_empty() {
                line_to_wrap.push_str(&reset);
            }
            wrapped.push(line_to_wrap);

            if is_whitespace {
                current_line = tracker.active_codes();
                current_width = 0;
            } else {
                current_line = tracker.active_codes();
                current_line.push_str(&token);
                current_width = token_width;
            }
        } else {
            current_line.push_str(&token);
            current_width += token_width;
        }

        update_tracker_from_text(&token, &mut tracker);
    }

    if !current_line.is_empty() {
        wrapped.push(current_line);
    }

    wrapped
}

fn split_into_tokens_with_ansi(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut pending_ansi = String::new();
    let mut in_whitespace = false;
    let mut idx = 0;

    while idx < text.len() {
        if let Some(ansi) = extract_ansi_code(text, idx) {
            pending_ansi.push_str(&ansi.code);
            idx += ansi.length;
            continue;
        }

        let ch = text[idx..].chars().next().expect("missing char");
        let is_space = ch == ' ';

        if is_space != in_whitespace && !current.is_empty() {
            tokens.push(current);
            current = String::new();
        }

        if !pending_ansi.is_empty() {
            current.push_str(&pending_ansi);
            pending_ansi.clear();
        }

        in_whitespace = is_space;
        current.push(ch);
        idx += ch.len_utf8();
    }

    if !pending_ansi.is_empty() {
        current.push_str(&pending_ansi);
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn break_long_word(word: &str, width: usize, tracker: &mut AnsiCodeTracker) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = tracker.active_codes();
    let mut current_width = 0;
    let mut idx = 0;

    while idx < word.len() {
        if let Some(ansi) = extract_ansi_code(word, idx) {
            current_line.push_str(&ansi.code);
            tracker.process(&ansi.code);
            idx += ansi.length;
            continue;
        }

        let text_end = next_ansi_or_end(word, idx);
        for grapheme in word[idx..text_end].graphemes(true) {
            let width_g = grapheme_width(grapheme);
            if current_width + width_g > width {
                let reset = tracker.line_end_reset();
                if !reset.is_empty() {
                    current_line.push_str(&reset);
                }
                lines.push(current_line);
                current_line = tracker.active_codes();
                current_width = 0;
            }

            current_line.push_str(grapheme);
            current_width += width_g;
        }
        idx = text_end;
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn update_tracker_from_text(text: &str, tracker: &mut AnsiCodeTracker) {
    let mut idx = 0;
    while idx < text.len() {
        if let Some(ansi) = extract_ansi_code(text, idx) {
            tracker.process(&ansi.code);
            idx += ansi.length;
        } else {
            let ch = text[idx..].chars().next().expect("missing char");
            idx += ch.len_utf8();
        }
    }
}

fn next_ansi_or_end(line: &str, mut idx: usize) -> usize {
    while idx < line.len() {
        if extract_ansi_code(line, idx).is_some() {
            break;
        }
        let ch = line[idx..].chars().next().expect("missing char");
        idx += ch.len_utf8();
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::{extract_segments, slice_by_column, wrap_text_with_ansi};

    #[test]
    fn strict_slicing_drops_boundary_wide_chars() {
        let line = "a😀b";
        let sliced = slice_by_column(line, 1, 1, true);
        assert_eq!(sliced, "");
    }

    #[test]
    fn extract_segments_inherits_styles() {
        let line = "\x1b[31mredblue";
        let segments = extract_segments(line, 3, 3, 4, false);
        assert_eq!(segments.before, "\x1b[31mred");
        assert_eq!(segments.before_width, 3);
        assert_eq!(segments.after, "\x1b[31mblue");
        assert_eq!(segments.after_width, 4);
    }

    #[test]
    fn underline_reset_inserted_on_wrap() {
        let line = "\x1b[4mword word";
        let wrapped = wrap_text_with_ansi(line, 4);
        assert!(wrapped.len() >= 2);
        assert!(wrapped[0].ends_with("\x1b[24m"));
        assert!(!wrapped.last().unwrap().ends_with("\x1b[24m"));
    }

    #[test]
    fn word_wrap_splits_on_spaces() {
        let wrapped = wrap_text_with_ansi("word word", 4);
        assert_eq!(wrapped, vec!["word", "word"]);
    }

    #[test]
    fn ansi_styles_preserved_across_wraps() {
        let wrapped = wrap_text_with_ansi("\x1b[31mword word", 4);
        assert_eq!(wrapped.len(), 2);
        assert!(wrapped[0].starts_with("\x1b[31m"));
        assert!(wrapped[1].starts_with("\x1b[31m"));
    }

    #[test]
    fn no_leading_whitespace_on_wrap() {
        let wrapped = wrap_text_with_ansi("word  word", 4);
        assert_eq!(wrapped.len(), 2);
        assert!(!wrapped[1].starts_with(' '));
    }
}
