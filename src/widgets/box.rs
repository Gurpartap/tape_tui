//! Box widget (Phase 18).

use std::boxed::Box as StdBox;
use std::cell::RefCell;
use std::rc::Rc;

use crate::core::component::Component;
use crate::core::cursor::CursorPos;
use crate::core::text::utils::apply_background_to_line;
use crate::core::text::width::visible_width;

struct RenderCache {
    child_lines: Vec<String>,
    width: usize,
    bg_sample: Option<String>,
    lines: Vec<String>,
}

pub struct Box {
    children: Vec<Rc<RefCell<StdBox<dyn Component>>>>,
    padding_x: usize,
    padding_y: usize,
    last_cursor_pos: Option<CursorPos>,
    bg_fn: Option<StdBox<dyn Fn(&str) -> String>>,
    cache: Option<RenderCache>,
}

impl Box {
    pub fn new(
        padding_x: usize,
        padding_y: usize,
        bg_fn: Option<StdBox<dyn Fn(&str) -> String>>,
    ) -> Self {
        Self {
            children: Vec::new(),
            padding_x,
            padding_y,
            last_cursor_pos: None,
            bg_fn,
            cache: None,
        }
    }

    pub fn add_child(&mut self, component: Rc<RefCell<StdBox<dyn Component>>>) {
        self.children.push(component);
        self.invalidate_cache();
    }

    pub fn remove_child(&mut self, component: &Rc<RefCell<StdBox<dyn Component>>>) -> bool {
        if let Some(index) = self
            .children
            .iter()
            .position(|child| Rc::ptr_eq(child, component))
        {
            self.children.remove(index);
            self.invalidate_cache();
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.children.clear();
        self.invalidate_cache();
    }

    pub fn set_bg_fn(&mut self, bg_fn: Option<StdBox<dyn Fn(&str) -> String>>) {
        self.bg_fn = bg_fn;
    }

    fn invalidate_cache(&mut self) {
        self.cache = None;
    }

    fn match_cache(
        &self,
        width: usize,
        child_lines: &[String],
        bg_sample: &Option<String>,
    ) -> bool {
        let Some(cache) = &self.cache else {
            return false;
        };
        cache.width == width
            && cache.bg_sample == *bg_sample
            && cache.child_lines.len() == child_lines.len()
            && cache
                .child_lines
                .iter()
                .zip(child_lines.iter())
                .all(|(cached, current)| cached == current)
    }

    fn apply_bg(&self, line: &str, width: usize) -> String {
        let visible_len = visible_width(line);
        let pad_needed = width.saturating_sub(visible_len);
        let mut padded = String::with_capacity(line.len() + pad_needed);
        padded.push_str(line);
        if pad_needed > 0 {
            padded.push_str(&" ".repeat(pad_needed));
        }

        if let Some(bg_fn) = self.bg_fn.as_ref() {
            apply_background_to_line(&padded, width, bg_fn)
        } else {
            padded
        }
    }
}

impl Default for Box {
    fn default() -> Self {
        Self::new(1, 1, None)
    }
}

impl Component for Box {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.children.is_empty() {
            self.last_cursor_pos = None;
            return Vec::new();
        }

        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let left_pad = " ".repeat(self.padding_x);

        let mut child_lines = Vec::new();
        let mut last_cursor_pos: Option<CursorPos> = None;
        for child in self.children.iter() {
            let start_row = child_lines.len();
            let mut child = child.borrow_mut();
            let lines = child.render(content_width);
            let cursor_pos = child.cursor_pos();
            drop(child);

            if let Some(pos) = cursor_pos {
                last_cursor_pos = Some(CursorPos {
                    row: self
                        .padding_y
                        .saturating_add(start_row)
                        .saturating_add(pos.row),
                    col: self.padding_x.saturating_add(pos.col),
                });
            }

            for line in lines {
                child_lines.push(format!("{left_pad}{line}"));
            }
        }
        self.last_cursor_pos = last_cursor_pos;

        if child_lines.is_empty() {
            self.last_cursor_pos = None;
            return Vec::new();
        }

        let bg_sample = self.bg_fn.as_ref().map(|bg| bg("test"));
        if self.match_cache(width, &child_lines, &bg_sample) {
            return self.cache.as_ref().expect("missing cache").lines.clone();
        }

        let mut result = Vec::new();
        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }
        for line in child_lines.iter() {
            result.push(self.apply_bg(line, width));
        }
        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }

        self.cache = Some(RenderCache {
            child_lines,
            width,
            bg_sample,
            lines: result.clone(),
        });

        result
    }

    fn cursor_pos(&self) -> Option<CursorPos> {
        self.last_cursor_pos
    }

    fn invalidate(&mut self) {
        self.invalidate_cache();
        for child in self.children.iter() {
            child.borrow_mut().invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Box as BoxWidget;
    use crate::core::component::Component;
    use crate::core::cursor::CursorPos;
    use crate::core::text::width::visible_width;
    use std::boxed::Box as StdBox;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct StaticComponent {
        lines: Vec<String>,
    }

    impl Component for StaticComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.lines.clone()
        }
    }

    struct CursorComponent {
        lines: Vec<String>,
        cursor: Option<CursorPos>,
    }

    impl Component for CursorComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.lines.clone()
        }

        fn cursor_pos(&self) -> Option<CursorPos> {
            self.cursor
        }
    }

    #[test]
    fn box_pads_children_to_width() {
        let mut boxed = BoxWidget::new(1, 1, None);
        let child: Rc<RefCell<StdBox<dyn Component>>> =
            Rc::new(RefCell::new(StdBox::new(StaticComponent {
                lines: vec!["hi".to_string()],
            })));
        boxed.add_child(child);

        let lines = boxed.render(6);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "      ");
        assert_eq!(lines[1], " hi   ");
        assert_eq!(lines[2], "      ");
        assert!(lines.iter().all(|line| visible_width(line) == 6));
    }

    #[test]
    fn box_offsets_child_cursor_for_padding_and_prefers_last_child() {
        let mut boxed = BoxWidget::new(1, 2, None);
        let first: Rc<RefCell<StdBox<dyn Component>>> =
            Rc::new(RefCell::new(StdBox::new(CursorComponent {
                lines: vec!["one".to_string()],
                cursor: Some(CursorPos { row: 0, col: 0 }),
            })));
        let second: Rc<RefCell<StdBox<dyn Component>>> =
            Rc::new(RefCell::new(StdBox::new(CursorComponent {
                lines: vec!["two".to_string(), "three".to_string()],
                cursor: Some(CursorPos { row: 1, col: 2 }),
            })));
        boxed.add_child(first);
        boxed.add_child(second);

        let _ = boxed.render(20);
        assert_eq!(
            boxed.cursor_pos(),
            Some(CursorPos { row: 4, col: 3 }),
            "expected row=padding_y(2)+first_child_lines(1)+child_row(1)=4; col=padding_x(1)+2=3"
        );
    }
}
