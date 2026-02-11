//! Text widget.

use crate::core::component::Component;
use crate::core::text::slice::wrap_text_with_ansi;
use crate::core::text::utils::apply_background_to_line;
use crate::core::text::width::visible_width;
use crate::Frame;

pub type TextBgFn = Box<dyn Fn(&str) -> String>;

pub struct Text {
    text: String,
    padding_x: usize,
    padding_y: usize,
    custom_bg_fn: Option<TextBgFn>,
    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_frame: Option<Frame>,
}

impl Text {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            padding_x: 1,
            padding_y: 1,
            custom_bg_fn: None,
            cached_text: None,
            cached_width: None,
            cached_frame: None,
        }
    }

    pub fn with_padding(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
            custom_bg_fn: None,
            cached_text: None,
            cached_width: None,
            cached_frame: None,
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.invalidate();
    }

    pub fn set_padding(&mut self, padding_x: usize, padding_y: usize) {
        self.padding_x = padding_x;
        self.padding_y = padding_y;
        self.invalidate();
    }

    pub fn set_custom_bg_fn(&mut self, custom_bg_fn: Option<TextBgFn>) {
        self.custom_bg_fn = custom_bg_fn;
        self.invalidate();
    }

    fn render_frame(&mut self, width: usize) -> Frame {
        if let Some(cached) = self.cached_frame.as_ref() {
            if self.cached_text.as_deref() == Some(&self.text) && self.cached_width == Some(width) {
                return cached.clone();
            }
        }

        if self.text.trim().is_empty() {
            let frame = Frame::new(Vec::new());
            self.cached_text = Some(self.text.clone());
            self.cached_width = Some(width);
            self.cached_frame = Some(frame.clone());
            return frame;
        }

        let normalized = self.text.replace('\t', "   ");
        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let wrapped = wrap_text_with_ansi(&normalized, content_width);

        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        let mut content_lines = Vec::new();

        for line in wrapped {
            let line_with_margins = format!("{left_margin}{line}{right_margin}");
            if let Some(bg_fn) = self.custom_bg_fn.as_ref() {
                content_lines.push(apply_background_to_line(&line_with_margins, width, bg_fn));
            } else {
                let visible_len = visible_width(&line_with_margins);
                let padding_needed = width.saturating_sub(visible_len);
                content_lines.push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
            }
        }

        let empty_line = " ".repeat(width);
        let mut empty_lines = Vec::new();
        for _ in 0..self.padding_y {
            if let Some(bg_fn) = self.custom_bg_fn.as_ref() {
                empty_lines.push(apply_background_to_line(&empty_line, width, bg_fn));
            } else {
                empty_lines.push(empty_line.clone());
            }
        }

        let mut result = Vec::new();
        result.extend(empty_lines.iter().cloned());
        result.extend(content_lines);
        result.extend(empty_lines);

        let frame: Frame = result.into();

        self.cached_text = Some(self.text.clone());
        self.cached_width = Some(width);
        self.cached_frame = Some(frame.clone());

        frame
    }
}

impl Component for Text {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.render_frame(width).into_strings()
    }

    fn invalidate(&mut self) {
        self.cached_text = None;
        self.cached_width = None;
        self.cached_frame = None;
    }
}

#[cfg(test)]
mod tests {
    use super::Text;
    use crate::core::component::Component;
    use crate::core::text::width::visible_width;

    #[test]
    fn text_wraps_and_pads_to_width() {
        let mut text = Text::with_padding("word word", 0, 0);
        let lines = text.render(4);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "word");
        assert_eq!(lines[1], "word");
        assert!(lines.iter().all(|line| visible_width(line) <= 4));
    }

    #[test]
    fn text_typed_frame_output_round_trips_losslessly() {
        let mut text = Text::with_padding("\x1b[31mred\x1b[0m\tword", 1, 1);
        text.set_custom_bg_fn(Some(Box::new(|line| format!("<{line}>"))));

        let width = 10;
        let rendered = text.render(width);

        // Force a fresh typed render to ensure `render_frame(..).into_strings()` is byte-identical.
        text.invalidate();
        let from_frame = text.render_frame(width).into_strings();

        assert_eq!(from_frame.len(), rendered.len());
        for (frame_line, render_line) in from_frame.iter().zip(rendered.iter()) {
            assert_eq!(frame_line.as_bytes(), render_line.as_bytes());
        }
    }
}
