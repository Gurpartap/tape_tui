//! Utility helpers (Phase 10).

use unicode_segmentation::UnicodeSegmentation;

use super::ansi::extract_ansi_code;
use super::width::visible_width;

const ANSI_RESET: &str = "\x1b[0m";

pub fn grapheme_segments(text: &str) -> unicode_segmentation::Graphemes<'_> {
    UnicodeSegmentation::graphemes(text, true)
}

pub fn is_whitespace_char(ch: char) -> bool {
    ch.is_whitespace()
}

pub fn is_punctuation_char(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')'
            | '{'
            | '}'
            | '['
            | ']'
            | '<'
            | '>'
            | '.'
            | ','
            | ';'
            | ':'
            | '\''
            | '"'
            | '!'
            | '?'
            | '+'
            | '-'
            | '='
            | '*'
            | '/'
            | '\\'
            | '|'
            | '&'
            | '%'
            | '^'
            | '$'
            | '#'
            | '@'
            | '~'
            | '`'
    )
}

pub fn apply_background_to_line(
    line: &str,
    width: usize,
    bg_fn: &dyn Fn(&str) -> String,
) -> String {
    let visible_len = visible_width(line);
    let padding_needed = width.saturating_sub(visible_len);
    let mut with_padding = String::with_capacity(line.len() + padding_needed);
    with_padding.push_str(line);
    if padding_needed > 0 {
        with_padding.push_str(&" ".repeat(padding_needed));
    }
    bg_fn(&with_padding)
}

pub fn truncate_to_width(text: &str, max_width: usize, ellipsis: &str, pad: bool) -> String {
    if max_width == 0 {
        return String::new();
    }

    let text_width = visible_width(text);
    if text_width <= max_width {
        if pad {
            return format!("{text}{}", " ".repeat(max_width - text_width));
        }
        return text.to_string();
    }

    let ellipsis_width = visible_width(ellipsis);
    let target_width = max_width.saturating_sub(ellipsis_width);
    if target_width == 0 {
        return ellipsis.chars().take(max_width).collect();
    }

    let mut segments: Vec<Segment> = Vec::new();
    let mut idx = 0;
    while idx < text.len() {
        if let Some(ansi) = extract_ansi_code(text, idx) {
            segments.push(Segment::Ansi(ansi.code));
            idx += ansi.length;
            continue;
        }

        let text_end = next_ansi_or_end(text, idx);
        for grapheme in grapheme_segments(&text[idx..text_end]) {
            segments.push(Segment::Grapheme(grapheme.to_string()));
        }
        idx = text_end;
    }

    let mut truncated = String::new();
    let mut current_width = 0;
    for segment in segments {
        match segment {
            Segment::Ansi(code) => truncated.push_str(&code),
            Segment::Grapheme(grapheme) => {
                let width = visible_width(&grapheme);
                if current_width + width > target_width {
                    break;
                }
                truncated.push_str(&grapheme);
                current_width += width;
            }
        }
    }

    let mut result = String::with_capacity(truncated.len() + ellipsis.len() + ANSI_RESET.len());
    result.push_str(&truncated);
    result.push_str(ANSI_RESET);
    result.push_str(ellipsis);

    if pad {
        let result_width = visible_width(&result);
        if result_width < max_width {
            result.push_str(&" ".repeat(max_width - result_width));
        }
    }

    result
}

enum Segment {
    Ansi(String),
    Grapheme(String),
}

fn next_ansi_or_end(input: &str, mut idx: usize) -> usize {
    while idx < input.len() {
        if extract_ansi_code(input, idx).is_some() {
            break;
        }
        let ch = input[idx..].chars().next().expect("missing char");
        idx += ch.len_utf8();
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::{
        apply_background_to_line, grapheme_segments, is_punctuation_char, is_whitespace_char,
        truncate_to_width,
    };
    use crate::core::text::width::visible_width;

    #[test]
    fn truncate_returns_original_when_shorter() {
        assert_eq!(truncate_to_width("hello", 6, "...", false), "hello");
    }

    #[test]
    fn truncate_adds_ellipsis_and_reset() {
        let truncated = truncate_to_width("hello", 4, "...", false);
        assert_eq!(truncated, "h\x1b[0m...");
        assert_eq!(visible_width(&truncated), 4);
    }

    #[test]
    fn truncate_preserves_ansi_prefix() {
        let truncated = truncate_to_width("\x1b[31mhello", 4, "...", false);
        assert_eq!(truncated, "\x1b[31mh\x1b[0m...");
        assert_eq!(visible_width(&truncated), 4);
    }

    #[test]
    fn truncate_keeps_ansi_before_ellipsis_when_no_grapheme_fits() {
        let truncated = truncate_to_width("\x1b[31m😀a", 2, ".", false);
        assert_eq!(truncated, "\x1b[31m\x1b[0m.");
        assert_eq!(visible_width(&truncated), 1);
    }

    #[test]
    fn truncate_pads_when_requested() {
        let padded = truncate_to_width("hi", 4, "...", true);
        assert_eq!(padded, "hi  ");
        assert_eq!(visible_width(&padded), 4);
    }

    #[test]
    fn truncate_handles_small_max_width() {
        let truncated = truncate_to_width("hello", 2, "...", false);
        assert_eq!(truncated, "..");
    }

    #[test]
    fn apply_background_pads_to_width() {
        let result = apply_background_to_line("hi", 4, &|text| format!("<{text}>"));
        assert_eq!(result, "<hi  >");
    }

    #[test]
    fn whitespace_and_punctuation_classification() {
        assert!(is_whitespace_char(' '));
        assert!(is_whitespace_char('\n'));
        assert!(!is_whitespace_char('a'));
        assert!(is_punctuation_char('.'));
        assert!(is_punctuation_char('-'));
        assert!(!is_punctuation_char('_'));
    }

    #[test]
    fn grapheme_segments_splits_clusters() {
        let clusters: Vec<&str> = grapheme_segments("a🇺🇸").collect();
        assert_eq!(clusters, vec!["a", "🇺🇸"]);
    }
}
