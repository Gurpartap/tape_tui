//! SelectList widget (Phase 20).

use crate::core::component::Component;
use crate::core::keybindings::{get_editor_keybindings, EditorAction};
use crate::render::utils::truncate_to_width;

fn normalize_to_single_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_break = false;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            if !last_was_break {
                out.push(' ');
            }
            last_was_break = true;
        } else {
            out.push(ch);
            last_was_break = false;
        }
    }
    out.trim().to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

impl SelectItem {
    pub fn new(value: impl Into<String>, label: impl Into<String>, description: Option<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description,
        }
    }
}

pub struct SelectListTheme {
    pub selected_prefix: Box<dyn Fn(&str) -> String>,
    pub selected_text: Box<dyn Fn(&str) -> String>,
    pub description: Box<dyn Fn(&str) -> String>,
    pub scroll_info: Box<dyn Fn(&str) -> String>,
    pub no_match: Box<dyn Fn(&str) -> String>,
}

pub struct SelectList {
    items: Vec<SelectItem>,
    filtered_items: Vec<SelectItem>,
    selected_index: usize,
    max_visible: usize,
    theme: SelectListTheme,
    on_select: Option<Box<dyn FnMut(SelectItem)>>,
    on_cancel: Option<Box<dyn FnMut()>>,
    on_selection_change: Option<Box<dyn FnMut(SelectItem)>>,
}

impl SelectList {
    pub fn new(items: Vec<SelectItem>, max_visible: usize, theme: SelectListTheme) -> Self {
        Self {
            filtered_items: items.clone(),
            items,
            selected_index: 0,
            max_visible,
            theme,
            on_select: None,
            on_cancel: None,
            on_selection_change: None,
        }
    }

    pub fn set_filter(&mut self, filter: &str) {
        let filter = filter.to_lowercase();
        self.filtered_items = self
            .items
            .iter()
            .filter(|item| item.value.to_lowercase().starts_with(&filter))
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    pub fn set_selected_index(&mut self, index: usize) {
        if self.filtered_items.is_empty() {
            self.selected_index = 0;
        } else {
            self.selected_index = index.min(self.filtered_items.len() - 1);
        }
    }

    pub fn set_on_select(&mut self, handler: Option<Box<dyn FnMut(SelectItem)>>) {
        self.on_select = handler;
    }

    pub fn set_on_cancel(&mut self, handler: Option<Box<dyn FnMut()>>) {
        self.on_cancel = handler;
    }

    pub fn set_on_selection_change(&mut self, handler: Option<Box<dyn FnMut(SelectItem)>>) {
        self.on_selection_change = handler;
    }

    pub fn get_selected_item(&self) -> Option<&SelectItem> {
        self.filtered_items.get(self.selected_index)
    }

    fn notify_selection_change(&mut self) {
        let Some(item) = self.filtered_items.get(self.selected_index) else {
            return;
        };
        if let Some(handler) = self.on_selection_change.as_mut() {
            handler(item.clone());
        }
    }

    fn render_selected(&self, width: usize, item: &SelectItem, description: Option<&str>) -> String {
        let prefix_width = 2;
        let display_value = if item.label.is_empty() { &item.value } else { &item.label };

        if let Some(description) = description {
            if width > 40 {
                let max_value_width = 30.min(width.saturating_sub(prefix_width + 4));
                let truncated_value = truncate_to_width(display_value, max_value_width, "", false);
                let spacing = " ".repeat(1.max(32usize.saturating_sub(truncated_value.len())));

                let description_start = prefix_width + truncated_value.len() + spacing.len();
                let remaining_width = width.saturating_sub(description_start + 2);
                if remaining_width > 10 {
                    let truncated_desc = truncate_to_width(description, remaining_width, "", false);
                    return (self.theme.selected_text)(&format!("→ {truncated_value}{spacing}{truncated_desc}"));
                }
            }
        }

        let max_width = width.saturating_sub(prefix_width + 2);
        let truncated_value = truncate_to_width(display_value, max_width, "", false);
        (self.theme.selected_text)(&format!("→ {truncated_value}"))
    }

    fn render_unselected(&self, width: usize, item: &SelectItem, description: Option<&str>) -> String {
        let prefix = "  ";
        let display_value = if item.label.is_empty() { &item.value } else { &item.label };

        if let Some(description) = description {
            if width > 40 {
                let max_value_width = 30.min(width.saturating_sub(prefix.len() + 4));
                let truncated_value = truncate_to_width(display_value, max_value_width, "", false);
                let spacing = " ".repeat(1.max(32usize.saturating_sub(truncated_value.len())));

                let description_start = prefix.len() + truncated_value.len() + spacing.len();
                let remaining_width = width.saturating_sub(description_start + 2);
                if remaining_width > 10 {
                    let truncated_desc = truncate_to_width(description, remaining_width, "", false);
                    let desc_text = (self.theme.description)(&format!("{spacing}{truncated_desc}"));
                    return format!("{prefix}{truncated_value}{desc_text}");
                }
            }
        }

        let max_width = width.saturating_sub(prefix.len() + 2);
        let truncated_value = truncate_to_width(display_value, max_width, "", false);
        format!("{prefix}{truncated_value}")
    }
}

impl Component for SelectList {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();

        if self.filtered_items.is_empty() {
            lines.push((self.theme.no_match)("  No matching commands"));
            return lines;
        }

        let max_visible = self.max_visible.max(1).min(self.filtered_items.len());
        let half = max_visible / 2;
        let start_index = if self.filtered_items.len() <= max_visible {
            0
        } else {
            let candidate = self.selected_index.saturating_sub(half);
            let max_start = self.filtered_items.len() - max_visible;
            candidate.min(max_start)
        };
        let end_index = (start_index + max_visible).min(self.filtered_items.len());

        for idx in start_index..end_index {
            let Some(item) = self.filtered_items.get(idx) else {
                continue;
            };
            let description = item.description.as_deref().and_then(|desc| {
                let normalized = normalize_to_single_line(desc);
                if normalized.is_empty() {
                    None
                } else {
                    Some(normalized)
                }
            });
            let description = description.as_deref();

            let line = if idx == self.selected_index {
                self.render_selected(width, item, description)
            } else {
                self.render_unselected(width, item, description)
            };
            lines.push(line);
        }

        if start_index > 0 || end_index < self.filtered_items.len() {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, self.filtered_items.len());
            let truncated = truncate_to_width(&scroll_text, width.saturating_sub(2), "", false);
            lines.push((self.theme.scroll_info)(&truncated));
        }

        lines
    }

    fn handle_input(&mut self, data: &str) {
        let kb = get_editor_keybindings();
        let kb = kb.lock().expect("editor keybindings lock poisoned");

        if kb.matches(data, EditorAction::SelectUp) {
            if self.filtered_items.is_empty() {
                return;
            }
            if self.selected_index == 0 {
                self.selected_index = self.filtered_items.len() - 1;
            } else {
                self.selected_index -= 1;
            }
            self.notify_selection_change();
        } else if kb.matches(data, EditorAction::SelectDown) {
            if self.filtered_items.is_empty() {
                return;
            }
            if self.selected_index == self.filtered_items.len() - 1 {
                self.selected_index = 0;
            } else {
                self.selected_index += 1;
            }
            self.notify_selection_change();
        } else if kb.matches(data, EditorAction::SelectConfirm) {
            if let Some(item) = self.filtered_items.get(self.selected_index) {
                if let Some(handler) = self.on_select.as_mut() {
                    handler(item.clone());
                }
            }
        } else if kb.matches(data, EditorAction::SelectCancel) {
            if let Some(handler) = self.on_cancel.as_mut() {
                handler();
            }
        }
    }

    fn invalidate(&mut self) {
        // No cached state to invalidate.
    }
}

#[cfg(test)]
mod tests {
    use super::{SelectItem, SelectList, SelectListTheme};
    use crate::core::component::Component;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn theme() -> SelectListTheme {
        SelectListTheme {
            selected_prefix: Box::new(|text| text.to_string()),
            selected_text: Box::new(|text| text.to_string()),
            description: Box::new(|text| text.to_string()),
            scroll_info: Box::new(|text| text.to_string()),
            no_match: Box::new(|text| text.to_string()),
        }
    }

    #[test]
    fn select_list_navigates_and_wraps() {
        let items = vec![
            SelectItem::new("one", "one", None),
            SelectItem::new("two", "two", None),
            SelectItem::new("three", "three", None),
        ];
        let mut list = SelectList::new(items, 2, theme());

        assert_eq!(list.get_selected_item().unwrap().value, "one");

        list.handle_input("\x1b[B");
        assert_eq!(list.get_selected_item().unwrap().value, "two");

        list.handle_input("\x1b[B");
        assert_eq!(list.get_selected_item().unwrap().value, "three");

        list.handle_input("\x1b[B");
        assert_eq!(list.get_selected_item().unwrap().value, "one");

        list.handle_input("\x1b[A");
        assert_eq!(list.get_selected_item().unwrap().value, "three");
    }

    #[test]
    fn select_list_callbacks_fire() {
        let items = vec![
            SelectItem::new("one", "one", None),
            SelectItem::new("two", "two", None),
        ];
        let mut list = SelectList::new(items, 2, theme());

        let changes: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let changes_ref = changes.clone();
        list.set_on_selection_change(Some(Box::new(move |item| {
            changes_ref.borrow_mut().push(item.value);
        })));

        let selected: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let selected_ref = selected.clone();
        list.set_on_select(Some(Box::new(move |item| {
            selected_ref.borrow_mut().push(item.value);
        })));

        let cancelled: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
        let cancelled_ref = cancelled.clone();
        list.set_on_cancel(Some(Box::new(move || {
            *cancelled_ref.borrow_mut() = true;
        })));

        list.handle_input("\x1b[B");
        assert_eq!(changes.borrow().as_slice(), &["two"]);

        list.handle_input("\r");
        assert_eq!(selected.borrow().as_slice(), &["two"]);

        list.handle_input("\x1b");
        assert!(*cancelled.borrow());
    }
}
