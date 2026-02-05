//! Stdin escape-sequence buffering (Phase 2).

use std::time::{Duration, Instant};

const ESC: char = '\x1b';
const BRACKETED_PASTE_START: &str = "\x1b[200~";
const BRACKETED_PASTE_END: &str = "\x1b[201~";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinEvent {
    Data(String),
    Paste(String),
}

#[derive(Debug)]
enum SequenceStatus {
    Complete,
    Incomplete,
    NotEscape,
}

#[derive(Debug)]
struct SequenceSplit {
    sequences: Vec<String>,
    remainder: String,
}

/// Buffers stdin input and emits complete sequences.
pub struct StdinBuffer {
    buffer: String,
    timeout_ms: u64,
    paste_mode: bool,
    paste_buffer: String,
    flush_deadline: Option<Instant>,
}

impl StdinBuffer {
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            buffer: String::new(),
            timeout_ms,
            paste_mode: false,
            paste_buffer: String::new(),
            flush_deadline: None,
        }
    }

    pub fn process(&mut self, data: &[u8]) -> Vec<StdinEvent> {
        self.flush_deadline = None;

        let str_data = if data.len() == 1 && data[0] > 127 {
            let byte = data[0] - 128;
            let mut converted = String::from("\x1b");
            converted.push(byte as char);
            converted
        } else {
            String::from_utf8_lossy(data).to_string()
        };

        if str_data.is_empty() && self.buffer.is_empty() {
            return vec![StdinEvent::Data(String::new())];
        }

        self.process_str(&str_data)
    }

    pub fn flush_due(&mut self, now: Instant) -> Vec<StdinEvent> {
        if self.buffer.is_empty() {
            self.flush_deadline = None;
            return Vec::new();
        }

        if let Some(deadline) = self.flush_deadline {
            if now >= deadline {
                self.flush_deadline = None;
                return self.flush_events();
            }
        }

        Vec::new()
    }

    pub fn next_timeout_ms(&self, now: Instant, default_ms: i32) -> i32 {
        if let Some(deadline) = self.flush_deadline {
            let remaining = deadline.saturating_duration_since(now);
            let ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            return ms.min(default_ms).max(0);
        }
        default_ms
    }

    pub fn flush_events(&mut self) -> Vec<StdinEvent> {
        let sequences = self.flush();
        sequences
            .into_iter()
            .map(StdinEvent::Data)
            .collect()
    }

    pub fn flush(&mut self) -> Vec<String> {
        self.flush_deadline = None;
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let sequences = vec![self.buffer.clone()];
        self.buffer.clear();
        sequences
    }

    pub fn clear(&mut self) {
        self.flush_deadline = None;
        self.buffer.clear();
        self.paste_mode = false;
        self.paste_buffer.clear();
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    fn process_str(&mut self, data: &str) -> Vec<StdinEvent> {
        let mut events = Vec::new();
        self.buffer.push_str(data);

        if self.paste_mode {
            self.paste_buffer.push_str(&self.buffer);
            self.buffer.clear();

            if let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) {
                let pasted = self.paste_buffer[..end_index].to_string();
                let remaining = self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();

                self.paste_mode = false;
                self.paste_buffer.clear();

                events.push(StdinEvent::Paste(pasted));

                if !remaining.is_empty() {
                    events.extend(self.process_str(&remaining));
                }
            }

            return events;
        }

        if let Some(start_index) = self.buffer.find(BRACKETED_PASTE_START) {
            if start_index > 0 {
                let before = &self.buffer[..start_index];
                let result = extract_complete_sequences(before);
                for sequence in result.sequences {
                    events.push(StdinEvent::Data(sequence));
                }
            }

            self.buffer = self.buffer[start_index + BRACKETED_PASTE_START.len()..].to_string();
            self.paste_mode = true;
            self.paste_buffer.push_str(&self.buffer);
            self.buffer.clear();

            if let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) {
                let pasted = self.paste_buffer[..end_index].to_string();
                let remaining = self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();

                self.paste_mode = false;
                self.paste_buffer.clear();

                events.push(StdinEvent::Paste(pasted));

                if !remaining.is_empty() {
                    events.extend(self.process_str(&remaining));
                }
            }

            return events;
        }

        let result = extract_complete_sequences(&self.buffer);
        self.buffer = result.remainder;
        for sequence in result.sequences {
            events.push(StdinEvent::Data(sequence));
        }

        if !self.buffer.is_empty() {
            self.flush_deadline = Some(Instant::now() + Duration::from_millis(self.timeout_ms));
        }

        events
    }
}

fn extract_complete_sequences(buffer: &str) -> SequenceSplit {
    let mut sequences = Vec::new();
    let mut pos = 0;
    let bytes = buffer.as_bytes();

    while pos < bytes.len() {
        if bytes[pos] == ESC as u8 {
            let mut seq_end = pos + 1;
            let mut completed = false;

            while seq_end <= bytes.len() {
                let candidate = &buffer[pos..seq_end];
                match is_complete_sequence(candidate) {
                    SequenceStatus::Complete => {
                        sequences.push(candidate.to_string());
                        pos = seq_end;
                        completed = true;
                        break;
                    }
                    SequenceStatus::Incomplete => {
                        seq_end += 1;
                    }
                    SequenceStatus::NotEscape => {
                        sequences.push(candidate.to_string());
                        pos = seq_end;
                        completed = true;
                        break;
                    }
                }
            }

            if !completed {
                return SequenceSplit {
                    sequences,
                    remainder: buffer[pos..].to_string(),
                };
            }
        } else {
            let ch = buffer[pos..].chars().next().expect("buffer char missing");
            sequences.push(ch.to_string());
            pos += ch.len_utf8();
        }
    }

    SequenceSplit {
        sequences,
        remainder: String::new(),
    }
}

fn is_complete_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with(ESC) {
        return SequenceStatus::NotEscape;
    }

    if data.len() == 1 {
        return SequenceStatus::Incomplete;
    }

    let after = &data[1..];

    if after.starts_with('[') {
        if after.starts_with("[M") {
            return if data.as_bytes().len() >= 6 {
                SequenceStatus::Complete
            } else {
                SequenceStatus::Incomplete
            };
        }
        return is_complete_csi_sequence(data);
    }

    if after.starts_with(']') {
        return is_complete_osc_sequence(data);
    }

    if after.starts_with('P') {
        return is_complete_dcs_sequence(data);
    }

    if after.starts_with('_') {
        return is_complete_apc_sequence(data);
    }

    if after.starts_with('O') {
        return if after.len() >= 2 {
            SequenceStatus::Complete
        } else {
            SequenceStatus::Incomplete
        };
    }

    if after.len() == 1 {
        return SequenceStatus::Complete;
    }

    SequenceStatus::Complete
}

fn is_complete_csi_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b[") {
        return SequenceStatus::Complete;
    }

    if data.len() < 3 {
        return SequenceStatus::Incomplete;
    }

    let payload = &data[2..];
    let last = payload.as_bytes().last().copied();
    let Some(last_byte) = last else {
        return SequenceStatus::Incomplete;
    };

    if (0x40..=0x7e).contains(&last_byte) {
        if payload.starts_with('<') {
            let last_char = last_byte as char;
            if last_char == 'M' || last_char == 'm' {
                let inner = &payload[1..payload.len() - 1];
                let parts: Vec<&str> = inner.split(';').collect();
                if parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
                {
                    return SequenceStatus::Complete;
                }
            }
            return SequenceStatus::Incomplete;
        }
        return SequenceStatus::Complete;
    }

    SequenceStatus::Incomplete
}

fn is_complete_osc_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b]") {
        return SequenceStatus::Complete;
    }

    if data.ends_with("\x1b\\") || data.ends_with('\x07') {
        return SequenceStatus::Complete;
    }

    SequenceStatus::Incomplete
}

fn is_complete_dcs_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1bP") {
        return SequenceStatus::Complete;
    }

    if data.ends_with("\x1b\\") {
        return SequenceStatus::Complete;
    }

    SequenceStatus::Incomplete
}

fn is_complete_apc_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b_") {
        return SequenceStatus::Complete;
    }

    if data.ends_with("\x1b\\") {
        return SequenceStatus::Complete;
    }

    SequenceStatus::Incomplete
}

#[cfg(test)]
mod tests {
    use super::{StdinBuffer, StdinEvent};
    use std::time::{Duration, Instant};

    #[test]
    fn splits_partial_sequences() {
        let mut buffer = StdinBuffer::new(10);

        let events = buffer.process(b"\x1b");
        assert!(events.is_empty());

        let events = buffer.process(b"[<35");
        assert!(events.is_empty());

        let events = buffer.process(b";20;5m");
        assert_eq!(events, vec![StdinEvent::Data("\x1b[<35;20;5m".to_string())]);
    }

    #[test]
    fn flushes_after_timeout() {
        let mut buffer = StdinBuffer::new(10);

        let events = buffer.process(b"\x1b[");
        assert!(events.is_empty());

        std::thread::sleep(Duration::from_millis(15));
        let events = buffer.flush_due(Instant::now());
        assert_eq!(events, vec![StdinEvent::Data("\x1b[".to_string())]);
    }

    #[test]
    fn emits_paste_event() {
        let mut buffer = StdinBuffer::new(10);

        let events = buffer.process(b"\x1b[200~hello\x1b[201~");
        assert_eq!(events, vec![StdinEvent::Paste("hello".to_string())]);
    }
}
