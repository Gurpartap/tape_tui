//! Typed render model (Phase 1).
//!
//! This phase introduces `Span`/`Line`/`Frame` as typed containers without changing
//! rendering behavior. The rest of the pipeline continues to operate on `Vec<String>`
//! until later phases migrate call sites.

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
}

impl Line {
    pub fn new(spans: Vec<Span>) -> Self {
        Self { spans }
    }

    pub fn spans(&self) -> &[Span] {
        &self.spans
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
}

impl Frame {
    pub fn new(lines: Vec<Line>) -> Self {
        Self { lines }
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
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
        Self::new(lines.into_iter().map(Line::from).collect())
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
}
