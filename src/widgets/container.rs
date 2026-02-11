//! Simple container widget (Phase 8).

use crate::core::component::Component;
use crate::core::cursor::CursorPos;

#[derive(Default)]
pub struct Container {
    children: Vec<Box<dyn Component>>,
    last_cursor_pos: Option<CursorPos>,
}

impl Container {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.children.push(component);
    }

    pub fn remove_child(&mut self, index: usize) -> bool {
        if index >= self.children.len() {
            return false;
        }
        self.children.remove(index);
        true
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }
}

impl Component for Container {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.last_cursor_pos = None;
        let mut lines = Vec::new();
        for child in self.children.iter_mut() {
            let start_row = lines.len();
            let child_lines = child.render(width);
            let child_cursor = child.cursor_pos();

            lines.extend(child_lines);
            if let Some(pos) = child_cursor {
                self.last_cursor_pos = Some(CursorPos {
                    row: start_row.saturating_add(pos.row),
                    col: pos.col,
                });
            }
        }
        lines
    }

    fn cursor_pos(&self) -> Option<CursorPos> {
        self.last_cursor_pos
    }

    fn invalidate(&mut self) {
        for child in self.children.iter_mut() {
            child.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Container;
    use crate::core::component::Component;
    use crate::core::cursor::CursorPos;

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
    fn container_concatenates_children() {
        let mut container = Container::new();
        let first: Box<dyn Component> = Box::new(StaticComponent {
            lines: vec!["one".to_string()],
        });
        let second: Box<dyn Component> = Box::new(StaticComponent {
            lines: vec!["two".to_string(), "three".to_string()],
        });
        container.add_child(first);
        container.add_child(second);

        let result = container.render(10);
        assert_eq!(result, vec!["one", "two", "three"]);
    }

    #[test]
    fn container_offsets_child_cursor_and_prefers_last_child() {
        let mut container = Container::new();
        let first: Box<dyn Component> = Box::new(CursorComponent {
            lines: vec!["one".to_string()],
            cursor: Some(CursorPos { row: 0, col: 0 }),
        });
        let second: Box<dyn Component> = Box::new(CursorComponent {
            lines: vec!["two".to_string(), "three".to_string()],
            cursor: Some(CursorPos { row: 1, col: 2 }),
        });
        container.add_child(first);
        container.add_child(second);

        let result = container.render(10);
        assert_eq!(result, vec!["one", "two", "three"]);
        assert_eq!(container.cursor_pos(), Some(CursorPos { row: 2, col: 2 }));
    }

    #[test]
    fn remove_child_by_index() {
        let mut container = Container::new();
        let first: Box<dyn Component> = Box::new(StaticComponent {
            lines: vec!["one".to_string()],
        });
        let second: Box<dyn Component> = Box::new(StaticComponent {
            lines: vec!["two".to_string()],
        });
        container.add_child(first);
        container.add_child(second);

        assert!(container.remove_child(0));
        let result = container.render(10);
        assert_eq!(result, vec!["two"]);
        assert!(!container.remove_child(1));
    }
}
