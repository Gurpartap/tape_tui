//! Grapheme width and visible width helpers (Phase 3).

use emojis::get as emoji_get;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use super::ansi::extract_ansi_code;

const TAB_WIDTH: usize = 3;

pub fn grapheme_width(grapheme: &str) -> usize {
    if grapheme.is_empty() {
        return 0;
    }
    if grapheme == "\t" {
        return TAB_WIDTH;
    }

    if emoji_get(grapheme).is_some() {
        return 2;
    }

    let mut width = 0;
    for ch in grapheme.chars() {
        if ch == '\t' {
            width += TAB_WIDTH;
            continue;
        }
        width += UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    width
}

pub fn visible_width(input: &str) -> usize {
    if input.is_empty() {
        return 0;
    }

    let mut clean = String::with_capacity(input.len());
    let mut idx = 0;
    while idx < input.len() {
        if let Some(ansi) = extract_ansi_code(input, idx) {
            idx += ansi.length;
            continue;
        }

        let ch = input[idx..].chars().next().expect("missing char");
        if ch == '\t' {
            clean.push_str("   ");
        } else {
            clean.push(ch);
        }
        idx += ch.len_utf8();
    }

    let mut width = 0;
    for grapheme in clean.graphemes(true) {
        width += grapheme_width(grapheme);
    }
    width
}

#[cfg(test)]
mod tests {
    use super::visible_width;

    #[test]
    fn ansi_ignored_in_width() {
        let input = "hi\x1b[31m!!\x1b[0m";
        assert_eq!(visible_width(input), 4);
    }

    #[test]
    fn osc8_ignored_in_width() {
        let input = "\x1b]8;;https://example.com\x07link\x1b]8;;\x07";
        assert_eq!(visible_width(input), 4);
    }

    #[test]
    fn rgi_emoji_width_is_two() {
        assert_eq!(visible_width("😀"), 2);
    }
}

