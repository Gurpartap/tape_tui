//! Stdin escape-sequence buffering.

use std::time::{Duration, Instant};

const ESC: char = '\x1b';
const BRACKETED_PASTE_START: &str = "\x1b[200~";
const BRACKETED_PASTE_END: &str = "\x1b[201~";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinEvent {
    Data(String),
    Paste(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdinBufferOptions {
    /// Timeout alias matching pi-tui options (milliseconds).
    pub timeout: Option<u64>,
    pub timeout_ms: u64,
}

impl Default for StdinBufferOptions {
    fn default() -> Self {
        Self {
            timeout: None,
            timeout_ms: 10,
        }
    }
}

pub type StdinBufferEventMap = StdinEvent;

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

    pub fn with_options(options: StdinBufferOptions) -> Self {
        let timeout = options.timeout.unwrap_or(options.timeout_ms);
        Self::new(timeout)
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
        sequences.into_iter().map(StdinEvent::Data).collect()
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
                let remaining =
                    self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();

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
                let remaining =
                    self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();

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
        // Keep incomplete escape tails buffered until timeout so bytes are never
        // dropped or reordered. This can intentionally head-of-line block
        // following bytes when tails are malformed; timeout flush emits verbatim.
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
            return if data.len() >= 6 {
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
                if parts.len() == 3
                    && parts
                        .iter()
                        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
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
    use super::{StdinBuffer, StdinBufferOptions, StdinEvent};
    use std::time::{Duration, Instant};

    // Test trust map:
    // - Primary guards: batches_kitty_press_release_sequences_across_chunks,
    //   handles_old_mouse_and_ss3_splits, flush_due_never_emits_before_deadline_and_only_once_after,
    //   clear_resets_deadline_without_requiring_flush_due,
    //   malformed_tail_blocks_until_timeout_but_preserves_every_byte.
    // - Legacy smoke: flush_deadline_and_clear_edge_cases (broad sanity only; not comparator-sensitive).

    fn events_to_wire(events: &[StdinEvent]) -> String {
        let mut out = String::new();
        for event in events {
            match event {
                StdinEvent::Data(data) => out.push_str(data),
                StdinEvent::Paste(content) => {
                    out.push_str("\x1b[200~");
                    out.push_str(content);
                    out.push_str("\x1b[201~");
                }
            }
        }
        out
    }

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

        let events = buffer.flush_due(Instant::now() + Duration::from_millis(15));
        assert_eq!(events, vec![StdinEvent::Data("\x1b[".to_string())]);
    }

    #[test]
    fn emits_paste_event() {
        let mut buffer = StdinBuffer::new(10);

        let events = buffer.process(b"\x1b[200~hello\x1b[201~");
        assert_eq!(events, vec![StdinEvent::Paste("hello".to_string())]);
    }

    #[test]
    fn timeout_alias_overrides_timeout_ms() {
        let options = StdinBufferOptions {
            timeout: Some(0),
            timeout_ms: 10,
        };
        let mut buffer = StdinBuffer::with_options(options);
        let events = buffer.process(b"\x1b[");
        assert!(events.is_empty());
        let events = buffer.flush_due(Instant::now());
        assert_eq!(events, vec![StdinEvent::Data("\x1b[".to_string())]);
    }

    #[test]
    fn batches_kitty_press_release_sequences_across_chunks() {
        let mut buffer = StdinBuffer::new(10);
        let events = buffer.process(b"\x1b[97;1u\x1b[97;1:");
        assert_eq!(events, vec![StdinEvent::Data("\x1b[97;1u".to_string())]);

        let events = buffer.process(b"3u");
        assert_eq!(events, vec![StdinEvent::Data("\x1b[97;1:3u".to_string())]);
    }

    #[test]
    fn handles_old_mouse_and_ss3_splits() {
        let mut buffer = StdinBuffer::new(10);

        let events = buffer.process(b"\x1b[M!!");
        assert!(events.is_empty());
        let events = buffer.process(b"!");
        assert_eq!(events, vec![StdinEvent::Data("\x1b[M!!!".to_string())]);

        let events = buffer.process(b"\x1bO");
        assert!(events.is_empty());
        let events = buffer.process(b"A");
        assert_eq!(events, vec![StdinEvent::Data("\x1bOA".to_string())]);
    }

    #[test]
    // Legacy smoke test: keep for broad flow sanity, but pair with:
    // - flush_due_never_emits_before_deadline_and_only_once_after
    // - clear_resets_deadline_without_requiring_flush_due
    fn flush_deadline_and_clear_edge_cases() {
        let mut buffer = StdinBuffer::new(25);

        let events = buffer.process(b"\x1b[");
        assert!(events.is_empty());
        let now = Instant::now();
        assert!(
            buffer.next_timeout_ms(now, 1000) <= 25,
            "timeout should reflect configured flush window"
        );

        let events = buffer.flush_due(now + Duration::from_millis(5));
        assert!(events.is_empty());
        assert_eq!(buffer.buffer(), "\x1b[");

        buffer.clear();
        assert!(buffer.buffer().is_empty());
        let events = buffer.flush_due(now + Duration::from_millis(100));
        assert!(events.is_empty());
        assert_eq!(buffer.next_timeout_ms(now, 77), 77);
    }

    #[test]
    fn flush_due_never_emits_before_deadline_and_only_once_after() {
        let mut buffer = StdinBuffer::new(25);

        let events = buffer.process(b"\x1b[<35");
        assert!(events.is_empty());

        let early = buffer.flush_due(Instant::now());
        assert!(
            early.is_empty(),
            "incomplete sequence must not flush before deadline"
        );

        let flushed = buffer.flush_due(Instant::now() + Duration::from_millis(50));
        assert_eq!(flushed, vec![StdinEvent::Data("\x1b[<35".to_string())]);

        let flushed_again = buffer.flush_due(Instant::now() + Duration::from_millis(100));
        assert!(
            flushed_again.is_empty(),
            "flush after deadline should be idempotent"
        );
    }

    #[test]
    fn clear_resets_deadline_without_requiring_flush_due() {
        let mut buffer = StdinBuffer::new(25);

        let events = buffer.process(b"\x1b[");
        assert!(events.is_empty());

        buffer.clear();
        assert_eq!(
            buffer.next_timeout_ms(Instant::now(), 77),
            77,
            "clear must reset pending deadline immediately"
        );
    }

    #[test]
    fn mixed_chunks_preserve_order_without_drop_or_duplicate() {
        let mut buffer = StdinBuffer::new(10);
        let mut events = Vec::new();

        events.extend(buffer.process(b"a"));
        events.extend(buffer.process(b"\x1b[200~xy"));
        events.extend(buffer.process(b"\x1b[201~\x1b[97u\x1b[97;1:"));
        events.extend(buffer.process(b"3ub"));

        let expected = vec![
            StdinEvent::Data("a".to_string()),
            StdinEvent::Paste("xy".to_string()),
            StdinEvent::Data("\x1b[97u".to_string()),
            StdinEvent::Data("\x1b[97;1:3u".to_string()),
            StdinEvent::Data("b".to_string()),
        ];
        assert_eq!(events, expected);

        let wire = events_to_wire(&events);
        assert_eq!(wire, "a\x1b[200~xy\x1b[201~\x1b[97u\x1b[97;1:3ub");

        let nothing_more = buffer.flush_due(Instant::now() + Duration::from_millis(100));
        assert!(nothing_more.is_empty(), "unexpected extra buffered data");
    }

    #[test]
    fn malformed_tail_blocks_until_timeout_but_preserves_every_byte() {
        let mut buffer = StdinBuffer::new(10);
        let input = "a\x1b[<35;1;xm\x1b[AZ";

        let mut events = buffer.process(input.as_bytes());
        assert_eq!(events, vec![StdinEvent::Data("a".to_string())]);

        events.extend(buffer.flush_due(Instant::now() + Duration::from_millis(25)));
        assert_eq!(events_to_wire(&events), input);

        let count_after_first_flush = events.len();
        events.extend(buffer.flush_due(Instant::now() + Duration::from_millis(50)));
        assert_eq!(
            events.len(),
            count_after_first_flush,
            "second timeout flush must not duplicate prior bytes"
        );
    }
}
