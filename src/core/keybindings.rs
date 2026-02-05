//! Editor keybindings (Phase 12).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::LazyLock;

use crate::core::input::matches_key;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditorAction {
    CursorUp,
    CursorDown,
    CursorLeft,
    CursorRight,
    CursorWordLeft,
    CursorWordRight,
    CursorLineStart,
    CursorLineEnd,
    JumpForward,
    JumpBackward,
    PageUp,
    PageDown,
    DeleteCharBackward,
    DeleteCharForward,
    DeleteWordBackward,
    DeleteWordForward,
    DeleteToLineStart,
    DeleteToLineEnd,
    NewLine,
    Submit,
    Tab,
    SelectUp,
    SelectDown,
    SelectPageUp,
    SelectPageDown,
    SelectConfirm,
    SelectCancel,
    Copy,
    Yank,
    YankPop,
    Undo,
    ExpandTools,
    ToggleSessionPath,
    ToggleSessionSort,
    RenameSession,
    DeleteSession,
    DeleteSessionNoninvasive,
}

pub type KeyId = String;

#[derive(Debug, Clone)]
pub enum KeyBinding {
    Single(KeyId),
    Multiple(Vec<KeyId>),
}

impl From<&str> for KeyBinding {
    fn from(value: &str) -> Self {
        KeyBinding::Single(value.to_string())
    }
}

impl From<String> for KeyBinding {
    fn from(value: String) -> Self {
        KeyBinding::Single(value)
    }
}

impl From<Vec<&str>> for KeyBinding {
    fn from(value: Vec<&str>) -> Self {
        KeyBinding::Multiple(value.into_iter().map(|item| item.to_string()).collect())
    }
}

impl From<Vec<String>> for KeyBinding {
    fn from(value: Vec<String>) -> Self {
        KeyBinding::Multiple(value)
    }
}

#[derive(Debug, Clone, Default)]
pub struct EditorKeybindingsConfig {
    entries: HashMap<EditorAction, KeyBinding>,
}

impl EditorKeybindingsConfig {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn set<K: Into<KeyBinding>>(&mut self, action: EditorAction, keys: K) {
        self.entries.insert(action, keys.into());
    }
}

pub static DEFAULT_EDITOR_KEYBINDINGS: LazyLock<HashMap<EditorAction, Vec<KeyId>>> = LazyLock::new(|| {
    use EditorAction::*;

    let mut map = HashMap::new();
    map.insert(CursorUp, vec!["up".to_string()]);
    map.insert(CursorDown, vec!["down".to_string()]);
    map.insert(CursorLeft, vec!["left".to_string(), "ctrl+b".to_string()]);
    map.insert(CursorRight, vec!["right".to_string(), "ctrl+f".to_string()]);
    map.insert(
        CursorWordLeft,
        vec!["alt+left".to_string(), "ctrl+left".to_string(), "alt+b".to_string()],
    );
    map.insert(
        CursorWordRight,
        vec!["alt+right".to_string(), "ctrl+right".to_string(), "alt+f".to_string()],
    );
    map.insert(CursorLineStart, vec!["home".to_string(), "ctrl+a".to_string()]);
    map.insert(CursorLineEnd, vec!["end".to_string(), "ctrl+e".to_string()]);
    map.insert(JumpForward, vec!["ctrl+]".to_string()]);
    map.insert(JumpBackward, vec!["ctrl+alt+]".to_string()]);
    map.insert(PageUp, vec!["pageUp".to_string()]);
    map.insert(PageDown, vec!["pageDown".to_string()]);
    map.insert(DeleteCharBackward, vec!["backspace".to_string()]);
    map.insert(DeleteCharForward, vec!["delete".to_string(), "ctrl+d".to_string()]);
    map.insert(DeleteWordBackward, vec!["ctrl+w".to_string(), "alt+backspace".to_string()]);
    map.insert(DeleteWordForward, vec!["alt+d".to_string(), "alt+delete".to_string()]);
    map.insert(DeleteToLineStart, vec!["ctrl+u".to_string()]);
    map.insert(DeleteToLineEnd, vec!["ctrl+k".to_string()]);
    map.insert(NewLine, vec!["shift+enter".to_string()]);
    map.insert(Submit, vec!["enter".to_string()]);
    map.insert(Tab, vec!["tab".to_string()]);
    map.insert(SelectUp, vec!["up".to_string()]);
    map.insert(SelectDown, vec!["down".to_string()]);
    map.insert(SelectPageUp, vec!["pageUp".to_string()]);
    map.insert(SelectPageDown, vec!["pageDown".to_string()]);
    map.insert(SelectConfirm, vec!["enter".to_string()]);
    map.insert(SelectCancel, vec!["escape".to_string(), "ctrl+c".to_string()]);
    map.insert(Copy, vec!["ctrl+c".to_string()]);
    map.insert(Yank, vec!["ctrl+y".to_string()]);
    map.insert(YankPop, vec!["alt+y".to_string()]);
    map.insert(Undo, vec!["ctrl+-".to_string()]);
    map.insert(ExpandTools, vec!["ctrl+o".to_string()]);
    map.insert(ToggleSessionPath, vec!["ctrl+p".to_string()]);
    map.insert(ToggleSessionSort, vec!["ctrl+s".to_string()]);
    map.insert(RenameSession, vec!["ctrl+r".to_string()]);
    map.insert(DeleteSession, vec!["ctrl+d".to_string()]);
    map.insert(DeleteSessionNoninvasive, vec!["ctrl+backspace".to_string()]);

    map
});

pub struct EditorKeybindingsManager {
    action_to_keys: HashMap<EditorAction, Vec<KeyId>>,
}

impl EditorKeybindingsManager {
    pub fn new(config: EditorKeybindingsConfig) -> Self {
        let mut manager = Self {
            action_to_keys: HashMap::new(),
        };
        manager.build_maps(&config);
        manager
    }

    fn build_maps(&mut self, config: &EditorKeybindingsConfig) {
        self.action_to_keys.clear();

        for (action, keys) in DEFAULT_EDITOR_KEYBINDINGS.iter() {
            self.action_to_keys.insert(*action, keys.clone());
        }

        for (action, binding) in config.entries.iter() {
            let key_list = match binding {
                KeyBinding::Single(key) => vec![key.clone()],
                KeyBinding::Multiple(keys) => keys.clone(),
            };
            self.action_to_keys.insert(*action, key_list);
        }
    }

    pub fn matches(&self, data: &str, action: EditorAction) -> bool {
        let keys = match self.action_to_keys.get(&action) {
            Some(keys) => keys,
            None => return false,
        };
        for key in keys {
            if matches_key(data, key.as_str()) {
                return true;
            }
        }
        false
    }

    pub fn get_keys(&self, action: EditorAction) -> Vec<KeyId> {
        self.action_to_keys.get(&action).cloned().unwrap_or_default()
    }

    pub fn set_config(&mut self, config: EditorKeybindingsConfig) {
        self.build_maps(&config);
    }
}

static GLOBAL_EDITOR_KEYBINDINGS: OnceLock<Arc<Mutex<EditorKeybindingsManager>>> = OnceLock::new();

pub fn get_editor_keybindings() -> Arc<Mutex<EditorKeybindingsManager>> {
    GLOBAL_EDITOR_KEYBINDINGS
        .get_or_init(|| Arc::new(Mutex::new(EditorKeybindingsManager::new(EditorKeybindingsConfig::default()))))
        .clone()
}

pub fn set_editor_keybindings(manager: EditorKeybindingsManager) {
    let global = get_editor_keybindings();
    let mut guard = global.lock().expect("editor keybindings lock poisoned");
    *guard = manager;
}

#[cfg(test)]
mod tests {
    use super::{EditorAction, EditorKeybindingsConfig, EditorKeybindingsManager, KeyBinding};

    #[test]
    fn defaults_match_expected_keys() {
        let manager = EditorKeybindingsManager::new(EditorKeybindingsConfig::default());
        assert!(manager.matches("\x1b[A", EditorAction::CursorUp));
        assert!(manager.matches("\x1b[B", EditorAction::CursorDown));
        assert!(manager.matches("\r", EditorAction::Submit));
    }

    #[test]
    fn overrides_replace_defaults() {
        let mut config = EditorKeybindingsConfig::default();
        config.set(EditorAction::Submit, KeyBinding::Single("ctrl+x".to_string()));
        let manager = EditorKeybindingsManager::new(config);
        assert!(manager.matches("\x18", EditorAction::Submit));
        assert!(!manager.matches("\r", EditorAction::Submit));
    }
}
