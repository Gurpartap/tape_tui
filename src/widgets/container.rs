//! Simple container widget (Phase 8).

use crate::core::component::Component;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
pub struct Container {
    children: Vec<Rc<RefCell<Box<dyn Component>>>>,
}

impl Container {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_child(&mut self, component: Rc<RefCell<Box<dyn Component>>>) {
        self.children.push(component);
    }

    pub fn remove_child(&mut self, component: &Rc<RefCell<Box<dyn Component>>>) -> bool {
        if let Some(index) = self
            .children
            .iter()
            .position(|child| Rc::ptr_eq(child, component))
        {
            self.children.remove(index);
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }
}

impl Component for Container {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for child in self.children.iter() {
            lines.extend(child.borrow_mut().render(width));
        }
        lines
    }

    fn invalidate(&mut self) {
        for child in self.children.iter() {
            child.borrow_mut().invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Container;
    use crate::core::component::Component;
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

    #[test]
    fn container_concatenates_children() {
        let mut container = Container::new();
        let first: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(StaticComponent {
                lines: vec!["one".to_string()],
            })));
        let second: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(StaticComponent {
                lines: vec!["two".to_string(), "three".to_string()],
            })));
        container.add_child(Rc::clone(&first));
        container.add_child(Rc::clone(&second));

        let result = container.render(10);
        assert_eq!(result, vec!["one", "two", "three"]);
    }

    #[test]
    fn remove_child_by_reference() {
        let mut container = Container::new();
        let first: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(StaticComponent {
                lines: vec!["one".to_string()],
            })));
        let second: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(StaticComponent {
                lines: vec!["two".to_string()],
            })));
        container.add_child(Rc::clone(&first));
        container.add_child(Rc::clone(&second));

        assert!(container.remove_child(&first));
        let result = container.render(10);
        assert_eq!(result, vec!["two"]);
        assert!(!container.remove_child(&first));
    }
}
