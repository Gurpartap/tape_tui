//! Typed render model (Phase 1).
//!
//! This phase introduces `Span`/`Line`/`Frame` as typed containers without changing
//! rendering behavior. The rest of the pipeline continues to operate on `Vec<String>`
//! until later phases migrate call sites.

use crate::core::cursor::CursorPos;

/// A contiguous run of rendered text.
///
/// Styling (colors, attributes) will be added in later phases. For now, a span is
/// just raw bytes stored in a UTF-8 `String`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Span {
    text: String,
}

impl Span {
    pub fn new(text: String) -> Self {
        Self { text }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn into_string(self) -> String {
        self.text
    }
}

impl From<String> for Span {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

/// A single rendered line.
///
/// A line is represented as a sequence of spans to support future per-span styling
/// without changing the type shape again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    spans: Vec<Span>,
    is_image: bool,
}

impl Line {
    pub fn new(spans: Vec<Span>) -> Self {
        Self {
            spans,
            is_image: false,
        }
    }

    pub fn image(spans: Vec<Span>) -> Self {
        Self {
            spans,
            is_image: true,
        }
    }

    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    pub fn is_image(&self) -> bool {
        self.is_image
    }

    pub fn into_string(self) -> String {
        let mut out = String::new();
        for span in self.spans {
            out.push_str(span.as_str());
        }
        out
    }
}

impl From<String> for Line {
    fn from(text: String) -> Self {
        Self::new(vec![Span::new(text)])
    }
}

/// A rendered frame (collection of lines).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    lines: Vec<Line>,
    cursor: Option<CursorPos>,
}

impl Frame {
    pub fn new(lines: Vec<Line>) -> Self {
        Self {
            lines,
            cursor: None,
        }
    }

    pub fn from_rendered_lines(mut lines: Vec<String>, height: usize) -> Self {
        let cursor = crate::core::cursor::extract_cursor_marker(&mut lines, height);
        let mut frame: Frame = lines.into();
        frame.cursor = cursor;
        frame
    }

    pub fn with_cursor(mut self, cursor: Option<CursorPos>) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn cursor(&self) -> Option<CursorPos> {
        self.cursor
    }

    pub fn into_lines(self) -> Vec<Line> {
        self.lines
    }

    pub fn into_strings(self) -> Vec<String> {
        self.lines
            .into_iter()
            .map(|line| line.into_string())
            .collect()
    }
}

impl From<Vec<String>> for Frame {
    fn from(lines: Vec<String>) -> Self {
        Self::new(
            lines
                .into_iter()
                .map(|text| {
                    let is_image = crate::core::terminal_image::is_image_line(&text);
                    let spans = vec![Span::new(text)];
                    if is_image {
                        Line::image(spans)
                    } else {
                        Line::new(spans)
                    }
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_strings_round_trip_preserves_bytes_exactly() {
        let input: Vec<String> = vec![
            String::new(),
            "plain".to_string(),
            " leading and trailing ".to_string(),
            "\u{1b}[31mred\u{1b}[0m".to_string(),
            "unicode: π你好".to_string(),
        ];

        let frame: Frame = input.clone().into();
        let output = frame.into_strings();

        assert_eq!(output.len(), input.len());
        for (out, inp) in output.iter().zip(input.iter()) {
            assert_eq!(out.as_bytes(), inp.as_bytes());
        }
    }

    #[test]
    fn from_vec_strings_marks_image_lines() {
        let frame: Frame = vec![
            "plain".to_string(),
            "\x1b_Gf=100;data".to_string(),
            "\x1b]1337;File=name=test:AAAA".to_string(),
        ]
        .into();

        assert!(!frame.lines()[0].is_image());
        assert!(frame.lines()[1].is_image());
        assert!(frame.lines()[2].is_image());
    }
}
