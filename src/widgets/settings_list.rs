//! SettingsList widget (Phase 21).

use std::cell::RefCell;
use std::rc::Rc;

use crate::core::component::Component;
use crate::core::fuzzy::fuzzy_filter;
use crate::core::keybindings::{get_editor_keybindings, EditorAction};
use crate::core::text::slice::wrap_text_with_ansi;
use crate::core::text::utils::truncate_to_width;
use crate::core::text::width::visible_width;
use crate::widgets::input::Input;

pub type SubmenuDone = Box<dyn FnMut(Option<String>)>;
pub type SubmenuFactory = Box<dyn FnMut(String, SubmenuDone) -> Box<dyn Component>>;

pub struct SettingItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub current_value: String,
    pub values: Option<Vec<String>>,
    pub submenu: Option<SubmenuFactory>,
}

impl SettingItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>, current_value: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            current_value: current_value.into(),
            values: None,
            submenu: None,
        }
    }
}

pub struct SettingsListTheme {
    pub label: Box<dyn Fn(&str, bool) -> String>,
    pub value: Box<dyn Fn(&str, bool) -> String>,
    pub description: Box<dyn Fn(&str) -> String>,
    pub cursor: String,
    pub hint: Box<dyn Fn(&str) -> String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SettingsListOptions {
    pub enable_search: bool,
}

pub struct SettingsList {
    items: Vec<SettingItem>,
    filtered_indices: Vec<usize>,
    theme: SettingsListTheme,
    selected_index: usize,
    max_visible: usize,
    on_change: Box<dyn FnMut(String, String)>,
    on_cancel: Box<dyn FnMut()>,
    search_input: Option<Input>,
    search_enabled: bool,
    submenu_component: Option<Box<dyn Component>>,
    submenu_display_index: Option<usize>,
    submenu_item_index: Option<usize>,
    submenu_result: Option<Rc<RefCell<Option<Option<String>>>>>,
}

impl SettingsList {
    pub fn new(
        items: Vec<SettingItem>,
        max_visible: usize,
        theme: SettingsListTheme,
        on_change: Box<dyn FnMut(String, String)>,
        on_cancel: Box<dyn FnMut()>,
        options: Option<SettingsListOptions>,
    ) -> Self {
        let options = options.unwrap_or_default();
        let search_enabled = options.enable_search;
        let filtered_indices: Vec<usize> = (0..items.len()).collect();
        let search_input = if search_enabled { Some(Input::new()) } else { None };

        Self {
            items,
            filtered_indices,
            theme,
            selected_index: 0,
            max_visible,
            on_change,
            on_cancel,
            search_input,
            search_enabled,
            submenu_component: None,
            submenu_display_index: None,
            submenu_item_index: None,
            submenu_result: None,
        }
    }

    pub fn update_value(&mut self, id: &str, new_value: &str) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.current_value = new_value.to_string();
        }
    }

    fn display_len(&self) -> usize {
        if self.search_enabled {
            self.filtered_indices.len()
        } else {
            self.items.len()
        }
    }

    fn display_item_index(&self, display_index: usize) -> Option<usize> {
        if self.search_enabled {
            self.filtered_indices.get(display_index).copied()
        } else if display_index < self.items.len() {
            Some(display_index)
        } else {
            None
        }
    }

    fn clamp_selected_index(&mut self) {
        let len = self.display_len();
        if len == 0 {
            self.selected_index = 0;
        } else if self.selected_index >= len {
            self.selected_index = len - 1;
        }
    }

    fn render_main_list(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();

        if self.search_enabled {
            if let Some(search_input) = self.search_input.as_mut() {
                lines.extend(search_input.render(width));
                lines.push(String::new());
            }
        }

        if self.items.is_empty() {
            lines.push((self.theme.hint)("  No settings available"));
            if self.search_enabled {
                self.add_hint_line(&mut lines, width);
            }
            return lines;
        }

        let display_len = self.display_len();
        if display_len == 0 {
            let hint = (self.theme.hint)("  No matching settings");
            lines.push(truncate_to_width(&hint, width, "...", false));
            self.add_hint_line(&mut lines, width);
            return lines;
        }

        self.clamp_selected_index();

        let max_visible = self.max_visible.max(1).min(display_len);
        let half = max_visible / 2;
        let start_index = if display_len <= max_visible {
            0
        } else {
            let candidate = self.selected_index.saturating_sub(half);
            let max_start = display_len - max_visible;
            candidate.min(max_start)
        };
        let end_index = (start_index + max_visible).min(display_len);

        let max_label_width = self
            .items
            .iter()
            .map(|item| visible_width(&item.label))
            .max()
            .unwrap_or(0)
            .min(30);

        for display_index in start_index..end_index {
            let Some(item_index) = self.display_item_index(display_index) else {
                continue;
            };
            let item = &self.items[item_index];
            let is_selected = display_index == self.selected_index;
            let prefix = if is_selected { self.theme.cursor.as_str() } else { "  " };
            let prefix_width = visible_width(prefix);

            let label_padding = max_label_width.saturating_sub(visible_width(&item.label));
            let label_padded = format!("{}{}", item.label, " ".repeat(label_padding));
            let label_text = (self.theme.label)(&label_padded, is_selected);

            let separator = "  ";
            let used_width = prefix_width + max_label_width + visible_width(separator);
            let value_max_width = width.saturating_sub(used_width + 2);
            let truncated_value = truncate_to_width(&item.current_value, value_max_width, "", false);
            let value_text = (self.theme.value)(&truncated_value, is_selected);

            let combined = format!("{prefix}{label_text}{separator}{value_text}");
            lines.push(truncate_to_width(&combined, width, "...", false));
        }

        if start_index > 0 || end_index < display_len {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, display_len);
            let truncated = truncate_to_width(&scroll_text, width.saturating_sub(2), "", false);
            lines.push((self.theme.hint)(&truncated));
        }

        if let Some(item_index) = self.display_item_index(self.selected_index) {
            if let Some(description) = self.items[item_index].description.as_deref() {
                lines.push(String::new());
                let wrapped = wrap_text_with_ansi(description, width.saturating_sub(4));
                for line in wrapped {
                    lines.push((self.theme.description)(&format!("  {line}")));
                }
            }
        }

        self.add_hint_line(&mut lines, width);
        lines
    }

    fn activate_item(&mut self) {
        let display_index = self.selected_index;
        let Some(item_index) = self.display_item_index(display_index) else {
            return;
        };

        let maybe_submenu = {
            let item = &mut self.items[item_index];
            if let Some(submenu) = item.submenu.as_mut() {
                let current_value = item.current_value.clone();
                let result_slot: Rc<RefCell<Option<Option<String>>>> = Rc::new(RefCell::new(None));
                let result_slot_clone = result_slot.clone();
                let done: SubmenuDone = Box::new(move |selected_value| {
                    *result_slot_clone.borrow_mut() = Some(selected_value);
                });
                let component = submenu(current_value, done);
                Some((component, result_slot))
            } else {
                None
            }
        };

        if let Some((component, result_slot)) = maybe_submenu {
            self.submenu_component = Some(component);
            self.submenu_display_index = Some(display_index);
            self.submenu_item_index = Some(item_index);
            self.submenu_result = Some(result_slot);
            return;
        }

        let (id, new_value) = {
            let item = &self.items[item_index];
            let Some(values) = item.values.as_ref() else {
                return;
            };
            if values.is_empty() {
                return;
            }
            let current_index = values
                .iter()
                .position(|value| value == &item.current_value)
                .unwrap_or(usize::MAX);
            let next_index = if current_index == usize::MAX {
                0
            } else {
                (current_index + 1) % values.len()
            };
            (item.id.clone(), values[next_index].clone())
        };

        self.items[item_index].current_value = new_value.clone();
        (self.on_change)(id, new_value);
    }

    fn apply_filter(&mut self, query: &str) {
        let indices: Vec<usize> = (0..self.items.len()).collect();
        let filtered = {
            let items = &self.items;
            fuzzy_filter(&indices, query, |index| items[*index].label.as_str())
        };
        self.filtered_indices = filtered;
        self.selected_index = 0;
    }

    fn close_submenu(&mut self) {
        self.submenu_component = None;
        self.submenu_result = None;
        if let Some(display_index) = self.submenu_display_index.take() {
            self.selected_index = display_index;
        }
        self.submenu_item_index = None;
    }

    fn apply_submenu_result(&mut self) {
        let Some(result_slot) = self.submenu_result.as_ref() else {
            return;
        };
        let result = {
            let mut slot = result_slot.borrow_mut();
            slot.take()
        };
        let Some(result) = result else {
            return;
        };

        if let Some(selected_value) = result {
            if let Some(item_index) = self.submenu_item_index {
                if let Some(item) = self.items.get_mut(item_index) {
                    item.current_value = selected_value.clone();
                    (self.on_change)(item.id.clone(), selected_value);
                }
            }
        }

        self.close_submenu();
    }

    fn add_hint_line(&self, lines: &mut Vec<String>, width: usize) {
        lines.push(String::new());
        let hint_text = if self.search_enabled {
            "  Type to search · Enter/Space to change · Esc to cancel"
        } else {
            "  Enter/Space to change · Esc to cancel"
        };
        let hint_line = (self.theme.hint)(hint_text);
        lines.push(truncate_to_width(&hint_line, width, "...", false));
    }
}

impl Component for SettingsList {
    fn render(&mut self, width: usize) -> Vec<String> {
        if let Some(component) = self.submenu_component.as_mut() {
            return component.render(width);
        }
        self.render_main_list(width)
    }

    fn handle_input(&mut self, data: &str) {
        if let Some(component) = self.submenu_component.as_mut() {
            component.handle_input(data);
            self.apply_submenu_result();
            return;
        }

        let kb = get_editor_keybindings();
        let (select_up, select_down, select_confirm, select_cancel) = {
            let kb = kb.lock().expect("editor keybindings lock poisoned");
            (
                kb.matches(data, EditorAction::SelectUp),
                kb.matches(data, EditorAction::SelectDown),
                kb.matches(data, EditorAction::SelectConfirm),
                kb.matches(data, EditorAction::SelectCancel),
            )
        };

        let display_len = self.display_len();
        if select_up {
            if display_len == 0 {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                display_len - 1
            } else {
                self.selected_index - 1
            };
        } else if select_down {
            if display_len == 0 {
                return;
            }
            self.selected_index = if self.selected_index == display_len - 1 {
                0
            } else {
                self.selected_index + 1
            };
        } else if select_confirm || data == " " {
            self.activate_item();
        } else if select_cancel {
            (self.on_cancel)();
        } else if self.search_enabled {
            let query = if let Some(search_input) = self.search_input.as_mut() {
                let sanitized: String = data.chars().filter(|ch| *ch != ' ').collect();
                if sanitized.is_empty() {
                    return;
                }
                search_input.handle_input(&sanitized);
                Some(search_input.get_value().to_string())
            } else {
                None
            };

            if let Some(query) = query {
                self.apply_filter(&query);
            }
        }
    }

    fn invalidate(&mut self) {
        if let Some(component) = self.submenu_component.as_mut() {
            component.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SettingItem, SettingsList, SettingsListOptions, SettingsListTheme, SubmenuDone};
    use crate::core::component::Component;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn theme() -> SettingsListTheme {
        SettingsListTheme {
            label: Box::new(|text, _selected| text.to_string()),
            value: Box::new(|text, _selected| text.to_string()),
            description: Box::new(|text| text.to_string()),
            cursor: "→ ".to_string(),
            hint: Box::new(|text| text.to_string()),
        }
    }

    struct TestSubmenu {
        done: Option<SubmenuDone>,
    }

    impl TestSubmenu {
        fn new(done: SubmenuDone) -> Self {
            Self { done: Some(done) }
        }
    }

    impl Component for TestSubmenu {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec!["submenu".to_string()]
        }

        fn handle_input(&mut self, _data: &str) {
            if let Some(mut done) = self.done.take() {
                done(Some("updated".to_string()));
            }
        }
    }

    #[test]
    fn settings_list_search_and_navigation() {
        let items = vec![
            SettingItem::new("a", "alpha", "one"),
            SettingItem::new("b", "beta", "two"),
            SettingItem::new("c", "gamma", "three"),
        ];

        let on_change = Box::new(|_id: String, _value: String| {});
        let on_cancel = Box::new(|| {});
        let mut list = SettingsList::new(
            items,
            2,
            theme(),
            on_change,
            on_cancel,
            Some(SettingsListOptions { enable_search: true }),
        );

        list.handle_input("be");
        assert_eq!(list.filtered_indices.len(), 1);
        assert_eq!(list.display_item_index(list.selected_index), Some(1));

        list.handle_input("\x1b[B");
        assert_eq!(list.selected_index, 0);
    }

    #[test]
    fn settings_list_cycles_values_and_submenu_updates() {
        let mut item = SettingItem::new("mode", "Mode", "off");
        item.values = Some(vec!["off".to_string(), "on".to_string()]);

        let mut submenu_item = SettingItem::new("submenu", "Sub", "init");
        submenu_item.submenu = Some(Box::new(|_current, done| Box::new(TestSubmenu::new(done))));

        let items = vec![item, submenu_item];

        let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let changes_ref = changes.clone();
        let on_change = Box::new(move |id: String, value: String| {
            changes_ref.borrow_mut().push((id, value));
        });
        let on_cancel = Box::new(|| {});

        let mut list = SettingsList::new(items, 2, theme(), on_change, on_cancel, None);

        list.handle_input("\r");
        assert_eq!(changes.borrow().as_slice(), &[("mode".to_string(), "on".to_string())]);

        list.handle_input("\x1b[B");
        list.handle_input("\r");
        list.handle_input("x");

        assert_eq!(changes.borrow().len(), 2);
        assert_eq!(changes.borrow()[1], ("submenu".to_string(), "updated".to_string()));
    }
}
