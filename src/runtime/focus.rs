//! Focus management + overlays (Phase 5/7).

use std::cell::RefCell;
use std::rc::Rc;

use crate::core::component::Component;

#[derive(Default)]
pub struct FocusState {
    focused: Option<Rc<RefCell<Box<dyn Component>>>>,
}

impl FocusState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_focus(&mut self, target: Option<Rc<RefCell<Box<dyn Component>>>>) {
        if let (Some(prev), Some(next)) = (self.focused.as_ref(), target.as_ref()) {
            if Rc::ptr_eq(prev, next) {
                return;
            }
        }

        if let Some(prev) = self.focused.take() {
            let mut component = prev.borrow_mut();
            if let Some(focusable) = component.as_focusable() {
                focusable.set_focused(false);
            }
        }

        if let Some(next) = target {
            {
                let mut component = next.borrow_mut();
                if let Some(focusable) = component.as_focusable() {
                    focusable.set_focused(true);
                }
            }
            self.focused = Some(next);
        }
    }

    pub fn clear(&mut self) {
        self.set_focus(None);
    }

    pub fn focused(&self) -> Option<Rc<RefCell<Box<dyn Component>>>> {
        self.focused.as_ref().map(Rc::clone)
    }
}

#[cfg(test)]
mod tests {
    use super::FocusState;
    use crate::core::component::{Component, Focusable};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct TestComponent {
        focused: Rc<RefCell<bool>>,
    }

    impl Component for TestComponent {
        fn render(&mut self, _width: usize) -> Vec<String> {
            Vec::new()
        }

        fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
            Some(self)
        }
    }

    impl Focusable for TestComponent {
        fn set_focused(&mut self, focused: bool) {
            *self.focused.borrow_mut() = focused;
        }

        fn is_focused(&self) -> bool {
            *self.focused.borrow()
        }
    }

    #[test]
    fn focus_toggles_flags() {
        let mut focus = FocusState::new();
        let first_flag = Rc::new(RefCell::new(false));
        let second_flag = Rc::new(RefCell::new(false));
        let first_component = TestComponent {
            focused: Rc::clone(&first_flag),
        };
        let second_component = TestComponent {
            focused: Rc::clone(&second_flag),
        };
        let first_handle: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(first_component)));
        let second_handle: Rc<RefCell<Box<dyn Component>>> =
            Rc::new(RefCell::new(Box::new(second_component)));

        focus.set_focus(Some(Rc::clone(&first_handle)));
        assert!(*first_flag.borrow());
        assert!(!*second_flag.borrow());

        focus.set_focus(Some(Rc::clone(&second_handle)));
        assert!(!*first_flag.borrow());
        assert!(*second_flag.borrow());

        focus.clear();
        assert!(!*first_flag.borrow());
        assert!(!*second_flag.borrow());
    }
}
