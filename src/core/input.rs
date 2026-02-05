//! Key parsing and input types (Phase 2).

use std::sync::atomic::{AtomicBool, Ordering};

static KITTY_PROTOCOL_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_kitty_protocol_active(active: bool) {
    KITTY_PROTOCOL_ACTIVE.store(active, Ordering::SeqCst);
}

pub fn is_kitty_protocol_active() -> bool {
    KITTY_PROTOCOL_ACTIVE.load(Ordering::SeqCst)
}

const MOD_SHIFT: u8 = 1;
const MOD_ALT: u8 = 2;
const MOD_CTRL: u8 = 4;
const LOCK_MASK: u8 = 64 + 128;

const CODEPOINT_ESCAPE: i32 = 27;
const CODEPOINT_TAB: i32 = 9;
const CODEPOINT_ENTER: i32 = 13;
const CODEPOINT_SPACE: i32 = 32;
const CODEPOINT_BACKSPACE: i32 = 127;
const CODEPOINT_KP_ENTER: i32 = 57414;

const ARROW_UP: i32 = -1;
const ARROW_DOWN: i32 = -2;
const ARROW_RIGHT: i32 = -3;
const ARROW_LEFT: i32 = -4;

const KEY_DELETE: i32 = -10;
const KEY_INSERT: i32 = -11;
const KEY_PAGE_UP: i32 = -12;
const KEY_PAGE_DOWN: i32 = -13;
const KEY_HOME: i32 = -14;
const KEY_END: i32 = -15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyEventType {
    Press,
    Repeat,
    Release,
}

#[derive(Debug, Clone, Copy)]
struct ParsedKittySequence {
    codepoint: i32,
    #[allow(dead_code)]
    shifted_key: Option<i32>,
    base_layout_key: Option<i32>,
    modifier: u8,
    #[allow(dead_code)]
    event_type: KeyEventType,
}

pub fn is_key_release(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }

    data.contains(":3u")
        || data.contains(":3~")
        || data.contains(":3A")
        || data.contains(":3B")
        || data.contains(":3C")
        || data.contains(":3D")
        || data.contains(":3H")
        || data.contains(":3F")
}

pub fn matches_key(data: &str, key_id: &str) -> bool {
    let parsed = parse_key_id(key_id);
    let Some(parsed) = parsed else {
        return false;
    };

    let modifier = parsed.modifier();
    let kitty_active = is_kitty_protocol_active();

    match parsed.key.as_str() {
        "escape" | "esc" => {
            if modifier != 0 {
                return false;
            }
            data == "\x1b" || matches_kitty_sequence(data, CODEPOINT_ESCAPE, 0)
        }
        "space" => {
            if !kitty_active {
                if parsed.ctrl && !parsed.alt && !parsed.shift && data == "\x00" {
                    return true;
                }
                if parsed.alt && !parsed.ctrl && !parsed.shift && data == "\x1b " {
                    return true;
                }
            }
            if modifier == 0 {
                return data == " " || matches_kitty_sequence(data, CODEPOINT_SPACE, 0);
            }
            matches_kitty_sequence(data, CODEPOINT_SPACE, modifier)
        }
        "tab" => {
            if parsed.shift && !parsed.ctrl && !parsed.alt {
                return data == "\x1b[Z" || matches_kitty_sequence(data, CODEPOINT_TAB, MOD_SHIFT);
            }
            if modifier == 0 {
                return data == "\t" || matches_kitty_sequence(data, CODEPOINT_TAB, 0);
            }
            matches_kitty_sequence(data, CODEPOINT_TAB, modifier)
        }
        "enter" | "return" => {
            if parsed.shift && !parsed.ctrl && !parsed.alt {
                if matches_kitty_sequence(data, CODEPOINT_ENTER, MOD_SHIFT)
                    || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, MOD_SHIFT)
                {
                    return true;
                }
                if matches_modify_other_keys(data, CODEPOINT_ENTER, MOD_SHIFT) {
                    return true;
                }
                if kitty_active {
                    return data == "\x1b\r" || data == "\n";
                }
                return false;
            }
            if parsed.alt && !parsed.ctrl && !parsed.shift {
                if matches_kitty_sequence(data, CODEPOINT_ENTER, MOD_ALT)
                    || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, MOD_ALT)
                {
                    return true;
                }
                if matches_modify_other_keys(data, CODEPOINT_ENTER, MOD_ALT) {
                    return true;
                }
                if !kitty_active {
                    return data == "\x1b\r";
                }
                return false;
            }
            if modifier == 0 {
                return data == "\r"
                    || (!kitty_active && data == "\n")
                    || data == "\x1bOM"
                    || matches_kitty_sequence(data, CODEPOINT_ENTER, 0)
                    || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, 0);
            }
            matches_kitty_sequence(data, CODEPOINT_ENTER, modifier)
                || matches_kitty_sequence(data, CODEPOINT_KP_ENTER, modifier)
        }
        "backspace" => {
            if parsed.alt && !parsed.ctrl && !parsed.shift {
                if data == "\x1b\x7f" || data == "\x1b\x08" {
                    return true;
                }
                return matches_kitty_sequence(data, CODEPOINT_BACKSPACE, MOD_ALT);
            }
            if modifier == 0 {
                return data == "\x7f"
                    || data == "\x08"
                    || matches_kitty_sequence(data, CODEPOINT_BACKSPACE, 0);
            }
            matches_kitty_sequence(data, CODEPOINT_BACKSPACE, modifier)
        }
        "insert" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_INSERT)
                    || matches_kitty_sequence(data, KEY_INSERT, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Insert, modifier) {
                return true;
            }
            matches_kitty_sequence(data, KEY_INSERT, modifier)
        }
        "delete" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_DELETE)
                    || matches_kitty_sequence(data, KEY_DELETE, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Delete, modifier) {
                return true;
            }
            matches_kitty_sequence(data, KEY_DELETE, modifier)
        }
        "clear" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_CLEAR);
            }
            matches_legacy_modifier_sequence(data, LegacyModifierKey::Clear, modifier)
        }
        "home" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_HOME)
                    || matches_kitty_sequence(data, KEY_HOME, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Home, modifier) {
                return true;
            }
            matches_kitty_sequence(data, KEY_HOME, modifier)
        }
        "end" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_END)
                    || matches_kitty_sequence(data, KEY_END, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::End, modifier) {
                return true;
            }
            matches_kitty_sequence(data, KEY_END, modifier)
        }
        "pageup" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_PAGE_UP)
                    || matches_kitty_sequence(data, KEY_PAGE_UP, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::PageUp, modifier) {
                return true;
            }
            matches_kitty_sequence(data, KEY_PAGE_UP, modifier)
        }
        "pagedown" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_PAGE_DOWN)
                    || matches_kitty_sequence(data, KEY_PAGE_DOWN, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::PageDown, modifier) {
                return true;
            }
            matches_kitty_sequence(data, KEY_PAGE_DOWN, modifier)
        }
        "up" => {
            if parsed.alt && !parsed.ctrl && !parsed.shift {
                return data == "\x1bp" || matches_kitty_sequence(data, ARROW_UP, MOD_ALT);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_UP)
                    || matches_kitty_sequence(data, ARROW_UP, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Up, modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_UP, modifier)
        }
        "down" => {
            if parsed.alt && !parsed.ctrl && !parsed.shift {
                return data == "\x1bn" || matches_kitty_sequence(data, ARROW_DOWN, MOD_ALT);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_DOWN)
                    || matches_kitty_sequence(data, ARROW_DOWN, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Down, modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_DOWN, modifier)
        }
        "left" => {
            if parsed.alt && !parsed.ctrl && !parsed.shift {
                return data == "\x1b[1;3D"
                    || (!kitty_active && data == "\x1bB")
                    || data == "\x1bb"
                    || matches_kitty_sequence(data, ARROW_LEFT, MOD_ALT);
            }
            if parsed.ctrl && !parsed.alt && !parsed.shift {
                return data == "\x1b[1;5D"
                    || matches_legacy_modifier_sequence(data, LegacyModifierKey::Left, MOD_CTRL)
                    || matches_kitty_sequence(data, ARROW_LEFT, MOD_CTRL);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_LEFT)
                    || matches_kitty_sequence(data, ARROW_LEFT, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Left, modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_LEFT, modifier)
        }
        "right" => {
            if parsed.alt && !parsed.ctrl && !parsed.shift {
                return data == "\x1b[1;3C"
                    || (!kitty_active && data == "\x1bF")
                    || data == "\x1bf"
                    || matches_kitty_sequence(data, ARROW_RIGHT, MOD_ALT);
            }
            if parsed.ctrl && !parsed.alt && !parsed.shift {
                return data == "\x1b[1;5C"
                    || matches_legacy_modifier_sequence(data, LegacyModifierKey::Right, MOD_CTRL)
                    || matches_kitty_sequence(data, ARROW_RIGHT, MOD_CTRL);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, &LEGACY_RIGHT)
                    || matches_kitty_sequence(data, ARROW_RIGHT, 0);
            }
            if matches_legacy_modifier_sequence(data, LegacyModifierKey::Right, modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_RIGHT, modifier)
        }
        "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => {
            if modifier != 0 {
                return false;
            }
            matches_legacy_function_sequence(data, parsed.key.as_str())
        }
        _ => {
            if let Some(ch) = parsed.single_char() {
                if is_letter(ch) || is_symbol_key(ch) {
                    let codepoint = ch as i32;
                    let raw_ctrl = raw_ctrl_char(ch);

                    if parsed.ctrl && parsed.alt && !parsed.shift && !kitty_active {
                        if let Some(raw_ctrl) = raw_ctrl {
                            return data == format!("\x1b{}", raw_ctrl);
                        }
                    }

                    if parsed.alt && !parsed.ctrl && !parsed.shift && !kitty_active && is_letter(ch) {
                        if data == format!("\x1b{}", ch) {
                            return true;
                        }
                    }

                    if parsed.ctrl && !parsed.shift && !parsed.alt {
                        if let Some(raw_ctrl) = raw_ctrl {
                            if data == raw_ctrl.to_string() {
                                return true;
                            }
                        }
                        return matches_kitty_sequence(data, codepoint, MOD_CTRL);
                    }

                    if parsed.ctrl && parsed.shift && !parsed.alt {
                        return matches_kitty_sequence(data, codepoint, MOD_SHIFT + MOD_CTRL);
                    }

                    if parsed.shift && !parsed.ctrl && !parsed.alt {
                        if data == ch.to_ascii_uppercase().to_string() {
                            return true;
                        }
                        return matches_kitty_sequence(data, codepoint, MOD_SHIFT);
                    }

                    if modifier != 0 {
                        return matches_kitty_sequence(data, codepoint, modifier);
                    }

                    return data == ch.to_string() || matches_kitty_sequence(data, codepoint, 0);
                }
            }

            false
        }
    }
}

pub fn parse_key(data: &str) -> Option<String> {
    if let Some(kitty) = parse_kitty_sequence(data) {
        let modifier = kitty.modifier & !LOCK_MASK;
        let mut mods = Vec::new();
        if modifier & MOD_SHIFT != 0 {
            mods.push("shift");
        }
        if modifier & MOD_CTRL != 0 {
            mods.push("ctrl");
        }
        if modifier & MOD_ALT != 0 {
            mods.push("alt");
        }

        let codepoint = kitty.codepoint;
        let is_latin_letter = codepoint >= 97 && codepoint <= 122;
        let is_known_symbol = codepoint >= 0 && codepoint <= 127 && is_symbol_key(codepoint as u8 as char);
        let effective_codepoint = if is_latin_letter || is_known_symbol {
            codepoint
        } else {
            kitty.base_layout_key.unwrap_or(codepoint)
        };

        let key_name = match effective_codepoint {
            CODEPOINT_ESCAPE => Some("escape".to_string()),
            CODEPOINT_TAB => Some("tab".to_string()),
            CODEPOINT_ENTER | CODEPOINT_KP_ENTER => Some("enter".to_string()),
            CODEPOINT_SPACE => Some("space".to_string()),
            CODEPOINT_BACKSPACE => Some("backspace".to_string()),
            KEY_DELETE => Some("delete".to_string()),
            KEY_INSERT => Some("insert".to_string()),
            KEY_HOME => Some("home".to_string()),
            KEY_END => Some("end".to_string()),
            KEY_PAGE_UP => Some("pageUp".to_string()),
            KEY_PAGE_DOWN => Some("pageDown".to_string()),
            ARROW_UP => Some("up".to_string()),
            ARROW_DOWN => Some("down".to_string()),
            ARROW_LEFT => Some("left".to_string()),
            ARROW_RIGHT => Some("right".to_string()),
            cp if cp >= 97 && cp <= 122 => Some((cp as u8 as char).to_string()),
            cp if cp >= 0 && cp <= 127 && is_symbol_key(cp as u8 as char) => {
                Some((cp as u8 as char).to_string())
            }
            _ => None,
        };

        if let Some(key_name) = key_name {
            if mods.is_empty() {
                return Some(key_name);
            }
            let mut combined = mods.join("+");
            combined.push('+');
            combined.push_str(&key_name);
            return Some(combined);
        }
    }

    if is_kitty_protocol_active() {
        if data == "\x1b\r" || data == "\n" {
            return Some("shift+enter".to_string());
        }
    }

    if let Some(key_id) = legacy_sequence_key_id(data) {
        return Some(key_id.to_string());
    }

    let kitty_active = is_kitty_protocol_active();

    if data == "\x1b" {
        return Some("escape".to_string());
    }
    if data == "\x1c" {
        return Some("ctrl+\\".to_string());
    }
    if data == "\x1d" {
        return Some("ctrl+]".to_string());
    }
    if data == "\x1f" {
        return Some("ctrl+-".to_string());
    }
    if data == "\x1b\x1b" {
        return Some("ctrl+alt+[".to_string());
    }
    if data == "\x1b\x1c" {
        return Some("ctrl+alt+\\".to_string());
    }
    if data == "\x1b\x1d" {
        return Some("ctrl+alt+]".to_string());
    }
    if data == "\x1b\x1f" {
        return Some("ctrl+alt+-".to_string());
    }
    if data == "\t" {
        return Some("tab".to_string());
    }
    if data == "\r" || (!kitty_active && data == "\n") || data == "\x1bOM" {
        return Some("enter".to_string());
    }
    if data == "\x00" {
        return Some("ctrl+space".to_string());
    }
    if data == " " {
        return Some("space".to_string());
    }
    if data == "\x7f" || data == "\x08" {
        return Some("backspace".to_string());
    }
    if data == "\x1b[Z" {
        return Some("shift+tab".to_string());
    }
    if !kitty_active && data == "\x1b\r" {
        return Some("alt+enter".to_string());
    }
    if !kitty_active && data == "\x1b " {
        return Some("alt+space".to_string());
    }
    if data == "\x1b\x7f" || data == "\x1b\x08" {
        return Some("alt+backspace".to_string());
    }
    if !kitty_active && data == "\x1bB" {
        return Some("alt+left".to_string());
    }
    if !kitty_active && data == "\x1bF" {
        return Some("alt+right".to_string());
    }
    if !kitty_active && data.len() == 2 && data.starts_with("\x1b") {
        let code = data.as_bytes()[1];
        if (1..=26).contains(&code) {
            let ch = (code + 96) as char;
            return Some(format!("ctrl+alt+{}", ch));
        }
        if (97..=122).contains(&code) {
            let ch = code as char;
            return Some(format!("alt+{}", ch));
        }
    }
    if data == "\x1b[A" {
        return Some("up".to_string());
    }
    if data == "\x1b[B" {
        return Some("down".to_string());
    }
    if data == "\x1b[C" {
        return Some("right".to_string());
    }
    if data == "\x1b[D" {
        return Some("left".to_string());
    }
    if data == "\x1b[H" || data == "\x1bOH" {
        return Some("home".to_string());
    }
    if data == "\x1b[F" || data == "\x1bOF" {
        return Some("end".to_string());
    }
    if data == "\x1b[3~" {
        return Some("delete".to_string());
    }
    if data == "\x1b[5~" {
        return Some("pageUp".to_string());
    }
    if data == "\x1b[6~" {
        return Some("pageDown".to_string());
    }

    if data.len() == 1 {
        let code = data.as_bytes()[0];
        if (1..=26).contains(&code) {
            let ch = (code + 96) as char;
            return Some(format!("ctrl+{}", ch));
        }
        if (32..=126).contains(&code) {
            return Some(data.to_string());
        }
    }

    None
}

struct ParsedKeyId {
    key: String,
    ctrl: bool,
    shift: bool,
    alt: bool,
}

impl ParsedKeyId {
    fn modifier(&self) -> u8 {
        let mut modifier = 0;
        if self.shift {
            modifier |= MOD_SHIFT;
        }
        if self.alt {
            modifier |= MOD_ALT;
        }
        if self.ctrl {
            modifier |= MOD_CTRL;
        }
        modifier
    }

    fn single_char(&self) -> Option<char> {
        let mut chars = self.key.chars();
        let ch = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        Some(ch)
    }
}

fn parse_key_id(key_id: &str) -> Option<ParsedKeyId> {
    let lowered = key_id.to_lowercase();
    let parts: Vec<&str> = lowered.split('+').collect();
    let key = parts.last()?.to_string();
    if key.is_empty() {
        return None;
    }
    Some(ParsedKeyId {
        key,
        ctrl: parts.iter().any(|part| *part == "ctrl"),
        shift: parts.iter().any(|part| *part == "shift"),
        alt: parts.iter().any(|part| *part == "alt"),
    })
}

fn raw_ctrl_char(key: char) -> Option<char> {
    let lower = key.to_ascii_lowercase();
    if is_letter(lower) || matches!(lower, '[' | '\\' | ']' | '_') {
        let code = lower as u8;
        return Some((code & 0x1f) as char);
    }
    if lower == '-' {
        return Some(31 as char);
    }
    None
}

fn is_letter(ch: char) -> bool {
    matches!(ch, 'a'..='z')
}

fn is_symbol_key(ch: char) -> bool {
    matches!(
        ch,
        '`' | '-' | '=' | '[' | ']' | '\\' | ';' | '\'' | ',' | '.' | '/' | '!' | '@' | '#'
            | '$' | '%' | '^' | '&' | '*' | '(' | ')' | '_' | '+' | '|' | '~' | '{' | '}'
            | ':' | '<' | '>' | '?'
    )
}

fn parse_event_type(event_type: Option<&str>) -> KeyEventType {
    match event_type.and_then(|value| value.parse::<u8>().ok()) {
        Some(2) => KeyEventType::Repeat,
        Some(3) => KeyEventType::Release,
        _ => KeyEventType::Press,
    }
}

fn parse_kitty_sequence(data: &str) -> Option<ParsedKittySequence> {
    let stripped = data.strip_prefix("\x1b[")?;

    if let Some(body) = stripped.strip_suffix('u') {
        let (code_part, mod_part) = match body.split_once(';') {
            Some((left, right)) => (left, Some(right)),
            None => (body, None),
        };

        let mut code_iter = code_part.split(':');
        let codepoint = code_iter.next()?.parse::<i32>().ok()?;
        let shifted = code_iter.next();
        let base = code_iter.next();
        if code_iter.next().is_some() {
            return None;
        }

        let shifted_key = shifted.and_then(|value| if value.is_empty() { None } else { value.parse().ok() });
        let base_layout_key = base.and_then(|value| value.parse().ok());

        let (modifier, event_type) = if let Some(mod_part) = mod_part {
            let (mod_value, event_value) = match mod_part.split_once(':') {
                Some((left, right)) => (left, Some(right)),
                None => (mod_part, None),
            };
            let mod_value = mod_value.parse::<u8>().unwrap_or(1);
            (mod_value.saturating_sub(1), parse_event_type(event_value))
        } else {
            (0, KeyEventType::Press)
        };

        return Some(ParsedKittySequence {
            codepoint,
            shifted_key,
            base_layout_key,
            modifier,
            event_type,
        });
    }

    if let Some(body) = stripped.strip_suffix('~') {
        let mut parts = body.split(';');
        let num_part = parts.next()?;
        let mod_part = parts.next();
        if parts.next().is_some() {
            return None;
        }
        let key_num = num_part.parse::<i32>().ok()?;
        let (modifier, event_type) = if let Some(mod_part) = mod_part {
            let (mod_value, event_value) = match mod_part.split_once(':') {
                Some((left, right)) => (left, Some(right)),
                None => (mod_part, None),
            };
            let mod_value = mod_value.parse::<u8>().unwrap_or(1);
            (mod_value.saturating_sub(1), parse_event_type(event_value))
        } else {
            (0, KeyEventType::Press)
        };

        let codepoint = match key_num {
            2 => KEY_INSERT,
            3 => KEY_DELETE,
            5 => KEY_PAGE_UP,
            6 => KEY_PAGE_DOWN,
            7 => KEY_HOME,
            8 => KEY_END,
            _ => return None,
        };

        return Some(ParsedKittySequence {
            codepoint,
            shifted_key: None,
            base_layout_key: None,
            modifier,
            event_type,
        });
    }

    if let Some(stripped) = stripped.strip_prefix("1;") {
        if stripped.len() >= 2 {
            let (mod_part, tail) = stripped.split_at(stripped.len() - 1);
            let final_char = tail.chars().next()?;
            if matches!(final_char, 'A' | 'B' | 'C' | 'D' | 'H' | 'F') {
                let (mod_value, event_type) = match mod_part.split_once(':') {
                    Some((left, right)) => (left, Some(right)),
                    None => (mod_part, None),
                };
                let mod_value = mod_value.parse::<u8>().unwrap_or(1);
                let modifier = mod_value.saturating_sub(1);
                let event_type = parse_event_type(event_type);

                let codepoint = match final_char {
                    'A' => ARROW_UP,
                    'B' => ARROW_DOWN,
                    'C' => ARROW_RIGHT,
                    'D' => ARROW_LEFT,
                    'H' => KEY_HOME,
                    'F' => KEY_END,
                    _ => return None,
                };

                return Some(ParsedKittySequence {
                    codepoint,
                    shifted_key: None,
                    base_layout_key: None,
                    modifier,
                    event_type,
                });
            }
        }
    }

    None
}

fn matches_kitty_sequence(data: &str, expected_codepoint: i32, expected_modifier: u8) -> bool {
    let parsed = match parse_kitty_sequence(data) {
        Some(parsed) => parsed,
        None => return false,
    };

    let actual_mod = parsed.modifier & !LOCK_MASK;
    let expected_mod = expected_modifier & !LOCK_MASK;
    if actual_mod != expected_mod {
        return false;
    }

    if parsed.codepoint == expected_codepoint {
        return true;
    }

    if parsed.base_layout_key == Some(expected_codepoint) {
        let cp = parsed.codepoint;
        let is_latin_letter = cp >= 97 && cp <= 122;
        let is_known_symbol = cp >= 0 && cp <= 127 && is_symbol_key(cp as u8 as char);
        if !is_latin_letter && !is_known_symbol {
            return true;
        }
    }

    false
}

fn matches_modify_other_keys(data: &str, expected_keycode: i32, expected_modifier: u8) -> bool {
    let body = match data.strip_prefix("\x1b[27;") {
        Some(body) => body,
        None => return false,
    };
    let body = match body.strip_suffix('~') {
        Some(body) => body,
        None => return false,
    };

    let mut parts = body.split(';');
    let mod_part = match parts.next() {
        Some(part) => part,
        None => return false,
    };
    let key_part = match parts.next() {
        Some(part) => part,
        None => return false,
    };
    if parts.next().is_some() {
        return false;
    }

    let mod_value = match mod_part.parse::<u8>() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let keycode = match key_part.parse::<i32>() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let actual_mod = mod_value.saturating_sub(1);

    keycode == expected_keycode && actual_mod == expected_modifier
}

fn matches_legacy_sequence(data: &str, sequences: &[&str]) -> bool {
    sequences.iter().any(|seq| *seq == data)
}

#[derive(Clone, Copy)]
enum LegacyModifierKey {
    Up,
    Down,
    Right,
    Left,
    Clear,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Home,
    End,
}

fn matches_legacy_modifier_sequence(data: &str, key: LegacyModifierKey, modifier: u8) -> bool {
    if modifier == MOD_SHIFT {
        return matches_legacy_sequence(data, legacy_shift_sequences(key));
    }
    if modifier == MOD_CTRL {
        return matches_legacy_sequence(data, legacy_ctrl_sequences(key));
    }
    false
}

fn legacy_shift_sequences(key: LegacyModifierKey) -> &'static [&'static str] {
    match key {
        LegacyModifierKey::Up => &LEGACY_SHIFT_UP,
        LegacyModifierKey::Down => &LEGACY_SHIFT_DOWN,
        LegacyModifierKey::Right => &LEGACY_SHIFT_RIGHT,
        LegacyModifierKey::Left => &LEGACY_SHIFT_LEFT,
        LegacyModifierKey::Clear => &LEGACY_SHIFT_CLEAR,
        LegacyModifierKey::Insert => &LEGACY_SHIFT_INSERT,
        LegacyModifierKey::Delete => &LEGACY_SHIFT_DELETE,
        LegacyModifierKey::PageUp => &LEGACY_SHIFT_PAGE_UP,
        LegacyModifierKey::PageDown => &LEGACY_SHIFT_PAGE_DOWN,
        LegacyModifierKey::Home => &LEGACY_SHIFT_HOME,
        LegacyModifierKey::End => &LEGACY_SHIFT_END,
    }
}

fn legacy_ctrl_sequences(key: LegacyModifierKey) -> &'static [&'static str] {
    match key {
        LegacyModifierKey::Up => &LEGACY_CTRL_UP,
        LegacyModifierKey::Down => &LEGACY_CTRL_DOWN,
        LegacyModifierKey::Right => &LEGACY_CTRL_RIGHT,
        LegacyModifierKey::Left => &LEGACY_CTRL_LEFT,
        LegacyModifierKey::Clear => &LEGACY_CTRL_CLEAR,
        LegacyModifierKey::Insert => &LEGACY_CTRL_INSERT,
        LegacyModifierKey::Delete => &LEGACY_CTRL_DELETE,
        LegacyModifierKey::PageUp => &LEGACY_CTRL_PAGE_UP,
        LegacyModifierKey::PageDown => &LEGACY_CTRL_PAGE_DOWN,
        LegacyModifierKey::Home => &LEGACY_CTRL_HOME,
        LegacyModifierKey::End => &LEGACY_CTRL_END,
    }
}

fn matches_legacy_function_sequence(data: &str, key: &str) -> bool {
    match key {
        "f1" => matches_legacy_sequence(data, &LEGACY_F1),
        "f2" => matches_legacy_sequence(data, &LEGACY_F2),
        "f3" => matches_legacy_sequence(data, &LEGACY_F3),
        "f4" => matches_legacy_sequence(data, &LEGACY_F4),
        "f5" => matches_legacy_sequence(data, &LEGACY_F5),
        "f6" => matches_legacy_sequence(data, &LEGACY_F6),
        "f7" => matches_legacy_sequence(data, &LEGACY_F7),
        "f8" => matches_legacy_sequence(data, &LEGACY_F8),
        "f9" => matches_legacy_sequence(data, &LEGACY_F9),
        "f10" => matches_legacy_sequence(data, &LEGACY_F10),
        "f11" => matches_legacy_sequence(data, &LEGACY_F11),
        "f12" => matches_legacy_sequence(data, &LEGACY_F12),
        _ => false,
    }
}

fn legacy_sequence_key_id(data: &str) -> Option<&'static str> {
    match data {
        "\x1bOA" => Some("up"),
        "\x1bOB" => Some("down"),
        "\x1bOC" => Some("right"),
        "\x1bOD" => Some("left"),
        "\x1bOH" => Some("home"),
        "\x1bOF" => Some("end"),
        "\x1b[E" | "\x1bOE" => Some("clear"),
        "\x1bOe" => Some("ctrl+clear"),
        "\x1b[e" => Some("shift+clear"),
        "\x1b[2~" => Some("insert"),
        "\x1b[2$" => Some("shift+insert"),
        "\x1b[2^" => Some("ctrl+insert"),
        "\x1b[3$" => Some("shift+delete"),
        "\x1b[3^" => Some("ctrl+delete"),
        "\x1b[[5~" => Some("pageUp"),
        "\x1b[[6~" => Some("pageDown"),
        "\x1b[a" => Some("shift+up"),
        "\x1b[b" => Some("shift+down"),
        "\x1b[c" => Some("shift+right"),
        "\x1b[d" => Some("shift+left"),
        "\x1bOa" => Some("ctrl+up"),
        "\x1bOb" => Some("ctrl+down"),
        "\x1bOc" => Some("ctrl+right"),
        "\x1bOd" => Some("ctrl+left"),
        "\x1b[5$" => Some("shift+pageUp"),
        "\x1b[6$" => Some("shift+pageDown"),
        "\x1b[7$" => Some("shift+home"),
        "\x1b[8$" => Some("shift+end"),
        "\x1b[5^" => Some("ctrl+pageUp"),
        "\x1b[6^" => Some("ctrl+pageDown"),
        "\x1b[7^" => Some("ctrl+home"),
        "\x1b[8^" => Some("ctrl+end"),
        "\x1bOP" => Some("f1"),
        "\x1bOQ" => Some("f2"),
        "\x1bOR" => Some("f3"),
        "\x1bOS" => Some("f4"),
        "\x1b[11~" => Some("f1"),
        "\x1b[12~" => Some("f2"),
        "\x1b[13~" => Some("f3"),
        "\x1b[14~" => Some("f4"),
        "\x1b[[A" => Some("f1"),
        "\x1b[[B" => Some("f2"),
        "\x1b[[C" => Some("f3"),
        "\x1b[[D" => Some("f4"),
        "\x1b[[E" => Some("f5"),
        "\x1b[15~" => Some("f5"),
        "\x1b[17~" => Some("f6"),
        "\x1b[18~" => Some("f7"),
        "\x1b[19~" => Some("f8"),
        "\x1b[20~" => Some("f9"),
        "\x1b[21~" => Some("f10"),
        "\x1b[23~" => Some("f11"),
        "\x1b[24~" => Some("f12"),
        "\x1bb" => Some("alt+left"),
        "\x1bf" => Some("alt+right"),
        "\x1bp" => Some("alt+up"),
        "\x1bn" => Some("alt+down"),
        _ => None,
    }
}

const LEGACY_UP: [&str; 2] = ["\x1b[A", "\x1bOA"];
const LEGACY_DOWN: [&str; 2] = ["\x1b[B", "\x1bOB"];
const LEGACY_RIGHT: [&str; 2] = ["\x1b[C", "\x1bOC"];
const LEGACY_LEFT: [&str; 2] = ["\x1b[D", "\x1bOD"];
const LEGACY_HOME: [&str; 4] = ["\x1b[H", "\x1bOH", "\x1b[1~", "\x1b[7~"];
const LEGACY_END: [&str; 4] = ["\x1b[F", "\x1bOF", "\x1b[4~", "\x1b[8~"];
const LEGACY_INSERT: [&str; 1] = ["\x1b[2~"];
const LEGACY_DELETE: [&str; 1] = ["\x1b[3~"];
const LEGACY_PAGE_UP: [&str; 2] = ["\x1b[5~", "\x1b[[5~"];
const LEGACY_PAGE_DOWN: [&str; 2] = ["\x1b[6~", "\x1b[[6~"];
const LEGACY_CLEAR: [&str; 2] = ["\x1b[E", "\x1bOE"];
const LEGACY_F1: [&str; 3] = ["\x1bOP", "\x1b[11~", "\x1b[[A"];
const LEGACY_F2: [&str; 3] = ["\x1bOQ", "\x1b[12~", "\x1b[[B"];
const LEGACY_F3: [&str; 3] = ["\x1bOR", "\x1b[13~", "\x1b[[C"];
const LEGACY_F4: [&str; 3] = ["\x1bOS", "\x1b[14~", "\x1b[[D"];
const LEGACY_F5: [&str; 2] = ["\x1b[15~", "\x1b[[E"];
const LEGACY_F6: [&str; 1] = ["\x1b[17~"];
const LEGACY_F7: [&str; 1] = ["\x1b[18~"];
const LEGACY_F8: [&str; 1] = ["\x1b[19~"];
const LEGACY_F9: [&str; 1] = ["\x1b[20~"];
const LEGACY_F10: [&str; 1] = ["\x1b[21~"];
const LEGACY_F11: [&str; 1] = ["\x1b[23~"];
const LEGACY_F12: [&str; 1] = ["\x1b[24~"];

const LEGACY_SHIFT_UP: [&str; 1] = ["\x1b[a"];
const LEGACY_SHIFT_DOWN: [&str; 1] = ["\x1b[b"];
const LEGACY_SHIFT_RIGHT: [&str; 1] = ["\x1b[c"];
const LEGACY_SHIFT_LEFT: [&str; 1] = ["\x1b[d"];
const LEGACY_SHIFT_CLEAR: [&str; 1] = ["\x1b[e"];
const LEGACY_SHIFT_INSERT: [&str; 1] = ["\x1b[2$"];
const LEGACY_SHIFT_DELETE: [&str; 1] = ["\x1b[3$"];
const LEGACY_SHIFT_PAGE_UP: [&str; 1] = ["\x1b[5$"];
const LEGACY_SHIFT_PAGE_DOWN: [&str; 1] = ["\x1b[6$"];
const LEGACY_SHIFT_HOME: [&str; 1] = ["\x1b[7$"];
const LEGACY_SHIFT_END: [&str; 1] = ["\x1b[8$"];

const LEGACY_CTRL_UP: [&str; 1] = ["\x1bOa"];
const LEGACY_CTRL_DOWN: [&str; 1] = ["\x1bOb"];
const LEGACY_CTRL_RIGHT: [&str; 1] = ["\x1bOc"];
const LEGACY_CTRL_LEFT: [&str; 1] = ["\x1bOd"];
const LEGACY_CTRL_CLEAR: [&str; 1] = ["\x1bOe"];
const LEGACY_CTRL_INSERT: [&str; 1] = ["\x1b[2^"];
const LEGACY_CTRL_DELETE: [&str; 1] = ["\x1b[3^"];
const LEGACY_CTRL_PAGE_UP: [&str; 1] = ["\x1b[5^"];
const LEGACY_CTRL_PAGE_DOWN: [&str; 1] = ["\x1b[6^"];
const LEGACY_CTRL_HOME: [&str; 1] = ["\x1b[7^"];
const LEGACY_CTRL_END: [&str; 1] = ["\x1b[8^"];

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::{is_key_release, matches_key, parse_key, set_kitty_protocol_active};

    fn test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn kitty_shift_enter_vs_alt_enter() {
        let _guard = test_lock().lock().expect("test lock poisoned");
        set_kitty_protocol_active(true);
        assert_eq!(parse_key("\x1b\r"), Some("shift+enter".to_string()));
        assert_eq!(parse_key("\n"), Some("shift+enter".to_string()));

        set_kitty_protocol_active(false);
        assert_eq!(parse_key("\x1b\r"), Some("alt+enter".to_string()));
    }

    #[test]
    fn modify_other_keys_matches_when_kitty_inactive() {
        let _guard = test_lock().lock().expect("test lock poisoned");
        set_kitty_protocol_active(false);
        assert!(matches_key("\x1b[27;2;13~", "shift+enter"));
    }

    #[test]
    fn base_layout_fallback_for_non_latin_only() {
        let _guard = test_lock().lock().expect("test lock poisoned");
        set_kitty_protocol_active(true);
        assert_eq!(parse_key("\x1b[1089::99;5u"), Some("ctrl+c".to_string()));
        assert_eq!(parse_key("\x1b[99::118;5u"), Some("ctrl+c".to_string()));
    }

    #[test]
    fn key_release_ignores_paste() {
        let _guard = test_lock().lock().expect("test lock poisoned");
        assert!(!is_key_release("\x1b[200~90:62:3F\x1b[201~"));
        assert!(is_key_release("\x1b[65;1:3u"));
    }
}
