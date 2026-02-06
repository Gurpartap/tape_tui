//! Structured input events produced by the runtime.

use crate::core::input::{parse_key, parse_key_event_type, parse_text, KeyEventType};

/// Input event delivered to components.
///
/// Notes:
/// - `raw` is the exact byte sequence received from the terminal (UTF-8 decoded) when applicable.
/// - `key_id` is a best-effort normalized identifier for matching keybindings.
/// - Text and paste events carry decoded text so widgets don't have to parse escape sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key {
        raw: String,
        key_id: String,
        event_type: KeyEventType,
    },
    Text {
        raw: String,
        text: String,
        event_type: KeyEventType,
    },
    Paste {
        raw: String,
        text: String,
    },
    Resize {
        columns: u16,
        rows: u16,
    },
    UnknownRaw {
        raw: String,
    },
}

pub fn parse_input_events(data: &str, kitty_active: bool) -> Vec<InputEvent> {
    if data.is_empty() {
        return Vec::new();
    }

    const PASTE_START: &str = "\x1b[200~";
    const PASTE_END: &str = "\x1b[201~";

    fn parse_non_paste(data: &str, kitty_active: bool) -> Vec<InputEvent> {
        if data.is_empty() {
            return Vec::new();
        }

        let event_type = parse_key_event_type(data);

        if let Some(text) = parse_text(data, kitty_active) {
            if event_type == KeyEventType::Release {
                return Vec::new();
            }
            return vec![InputEvent::Text {
                raw: data.to_string(),
                text,
                event_type,
            }];
        }

        if let Some(key_id) = parse_key(data, kitty_active) {
            return vec![InputEvent::Key {
                raw: data.to_string(),
                key_id,
                event_type,
            }];
        }

        vec![InputEvent::UnknownRaw {
            raw: data.to_string(),
        }]
    }

    let mut events = Vec::new();
    let mut remaining = data;
    loop {
        let Some(start) = remaining.find(PASTE_START) else {
            events.extend(parse_non_paste(remaining, kitty_active));
            break;
        };

        let before = &remaining[..start];
        events.extend(parse_non_paste(before, kitty_active));

        let after_start = &remaining[start + PASTE_START.len()..];
        let Some(end_rel) = after_start.find(PASTE_END) else {
            events.push(InputEvent::UnknownRaw {
                raw: remaining.to_string(),
            });
            break;
        };

        let paste_text = &after_start[..end_rel];
        let raw_end = start + PASTE_START.len() + end_rel + PASTE_END.len();
        events.push(InputEvent::Paste {
            raw: remaining[start..raw_end].to_string(),
            text: paste_text.to_string(),
        });

        remaining = &after_start[end_rel + PASTE_END.len()..];
        if remaining.is_empty() {
            break;
        }
    }

    events
}

#[cfg(test)]
mod tests {
    use super::{parse_input_events, InputEvent};
    use crate::core::input::KeyEventType;

    #[test]
    fn space_is_text_not_key() {
        let events = parse_input_events(" ", false);
        assert_eq!(
            events,
            vec![InputEvent::Text {
                raw: " ".to_string(),
                text: " ".to_string(),
                event_type: KeyEventType::Press,
            }]
        );
    }

    #[test]
    fn printable_utf8_is_text() {
        let events = parse_input_events("be", false);
        assert_eq!(
            events,
            vec![InputEvent::Text {
                raw: "be".to_string(),
                text: "be".to_string(),
                event_type: KeyEventType::Press,
            }]
        );
    }

    #[test]
    fn control_keys_become_key_events() {
        assert_eq!(
            parse_input_events("\r", false),
            vec![InputEvent::Key {
                raw: "\r".to_string(),
                key_id: "enter".to_string(),
                event_type: KeyEventType::Press,
            }]
        );
        assert_eq!(
            parse_input_events("\x1b", false),
            vec![InputEvent::Key {
                raw: "\x1b".to_string(),
                key_id: "escape".to_string(),
                event_type: KeyEventType::Press,
            }]
        );
        assert_eq!(
            parse_input_events("\x1b[A", false),
            vec![InputEvent::Key {
                raw: "\x1b[A".to_string(),
                key_id: "up".to_string(),
                event_type: KeyEventType::Press,
            }]
        );
    }

    #[test]
    fn bracketed_paste_is_parsed_and_can_be_mixed() {
        let events = parse_input_events("a\x1b[200~b\x1b[201~c", false);
        assert_eq!(
            events,
            vec![
                InputEvent::Text {
                    raw: "a".to_string(),
                    text: "a".to_string(),
                    event_type: KeyEventType::Press,
                },
                InputEvent::Paste {
                    raw: "\x1b[200~b\x1b[201~".to_string(),
                    text: "b".to_string(),
                },
                InputEvent::Text {
                    raw: "c".to_string(),
                    text: "c".to_string(),
                    event_type: KeyEventType::Press,
                },
            ]
        );
    }
}
