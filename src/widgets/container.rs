//! Simple container widget (Phase 8).

use crate::core::component::Component;

#[derive(Default)]
pub struct Container {
    children: Vec<Box<dyn Component>>,
}

impl Container {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.children.push(component);
    }

    pub fn remove_child(&mut self, index: usize) -> Option<Box<dyn Component>> {
        if index < self.children.len() {
            Some(self.children.remove(index))
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }
}

impl Component for Container {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for child in self.children.iter_mut() {
            lines.extend(child.render(width));
        }
        lines
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

    struct StaticComponent {
        lines: Vec<String>,
    }

    impl Component for StaticComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.lines.clone()
        }
    }

    #[test]
    fn container_concatenates_children() {
        let mut container = Container::new();
        container.add_child(Box::new(StaticComponent {
            lines: vec!["one".to_string()],
        }));
        container.add_child(Box::new(StaticComponent {
            lines: vec!["two".to_string(), "three".to_string()],
        }));

        let result = container.render(10);
        assert_eq!(result, vec!["one", "two", "three"]);
    }
}
