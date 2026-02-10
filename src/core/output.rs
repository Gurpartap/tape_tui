//! Typed terminal output commands and a single output gate.
//!
//! Invariant: all terminal writes must flow through `OutputGate::flush(..)`.

use crate::core::terminal::Terminal;

// When a frame is large, coalescing all output into a new String doubles peak
// memory usage (payload + coalesced copy). Stream large flushes in chunks to
// avoid this.
const OUTPUT_GATE_STREAM_THRESHOLD_BYTES: usize = 64 * 1024;
const OUTPUT_GATE_STREAM_CHUNK_BYTES: usize = 16 * 1024;

fn decimal_len(mut n: usize) -> usize {
    let mut len = 1;
    while n >= 10 {
        n /= 10;
        len += 1;
    }
    len
}

pub(crate) fn osc_title_sequence(title: &str) -> String {
    let mut seq = String::with_capacity(title.len() + 5);
    seq.push_str("\x1b]0;");
    seq.push_str(title);
    seq.push('\x07');
    seq
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalCmd {
    /// Raw bytes/control sequences (UTF-8 string) to be written to the terminal.
    Bytes(String),
    /// Static raw bytes/control sequences (UTF-8 string) to be written to the terminal.
    BytesStatic(&'static str),

    /// Cursor visibility.
    HideCursor,
    ShowCursor,

    /// Clear operations.
    ///
    /// TypeScript parity (exact bytes):
    /// - `ClearLine` -> `\x1b[K`
    /// - `ClearFromCursor` -> `\x1b[J`
    /// - `ClearScreen` -> `\x1b[2J\x1b[H`
    ClearLine,
    ClearFromCursor,
    ClearScreen,

    /// Cursor movement.
    ///
    /// Semantics:
    /// - `MoveUp`/`MoveDown`: move the cursor by `n` rows. `n == 0` is a no-op.
    /// - `ColumnAbs`: move the cursor to an absolute 1-based column (ANSI `CSI n G`).
    ///   `n == 0` is a no-op.
    MoveUp(usize),
    MoveDown(usize),
    ColumnAbs(usize),

    /// Protocol toggles.
    BracketedPasteEnable,
    BracketedPasteDisable,
    KittyQuery,
    KittyEnable,
    KittyDisable,

    /// Queries.
    QueryCellSize,
}

impl TerminalCmd {
    pub fn bytes(data: impl Into<String>) -> Self {
        Self::Bytes(data.into())
    }
}

#[derive(Debug, Default)]
pub struct OutputGate {
    cmds: Vec<TerminalCmd>,
}

impl OutputGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, cmd: TerminalCmd) {
        self.cmds.push(cmd);
    }

    pub fn extend<I>(&mut self, cmds: I)
    where
        I: IntoIterator<Item = TerminalCmd>,
    {
        self.cmds.extend(cmds);
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    pub fn clear(&mut self) {
        self.cmds.clear();
    }

    fn encoded_len(cmd: &TerminalCmd) -> usize {
        match cmd {
            TerminalCmd::Bytes(data) => data.len(),
            TerminalCmd::BytesStatic(data) => data.len(),
            TerminalCmd::HideCursor => "\x1b[?25l".len(),
            TerminalCmd::ShowCursor => "\x1b[?25h".len(),
            TerminalCmd::ClearLine => "\x1b[K".len(),
            TerminalCmd::ClearFromCursor => "\x1b[J".len(),
            TerminalCmd::ClearScreen => "\x1b[2J\x1b[H".len(),
            TerminalCmd::MoveUp(n) | TerminalCmd::MoveDown(n) | TerminalCmd::ColumnAbs(n) => {
                if *n == 0 {
                    0
                } else {
                    // "\x1b[" + digits + suffix
                    3 + decimal_len(*n)
                }
            }
            TerminalCmd::BracketedPasteEnable => "\x1b[?2004h".len(),
            TerminalCmd::BracketedPasteDisable => "\x1b[?2004l".len(),
            TerminalCmd::KittyQuery => "\x1b[?u".len(),
            TerminalCmd::KittyEnable => "\x1b[>7u".len(),
            TerminalCmd::KittyDisable => "\x1b[<u".len(),
            TerminalCmd::QueryCellSize => "\x1b[16t".len(),
        }
    }

    fn encode_into(out: &mut String, cmd: TerminalCmd) {
        use std::fmt::Write as _;

        match cmd {
            TerminalCmd::Bytes(data) => out.push_str(&data),
            TerminalCmd::BytesStatic(data) => out.push_str(data),
            TerminalCmd::HideCursor => out.push_str("\x1b[?25l"),
            TerminalCmd::ShowCursor => out.push_str("\x1b[?25h"),
            TerminalCmd::ClearLine => out.push_str("\x1b[K"),
            TerminalCmd::ClearFromCursor => out.push_str("\x1b[J"),
            TerminalCmd::ClearScreen => out.push_str("\x1b[2J\x1b[H"),
            TerminalCmd::MoveUp(n) => {
                if n > 0 {
                    let _ = write!(out, "\x1b[{n}A");
                }
            }
            TerminalCmd::MoveDown(n) => {
                if n > 0 {
                    let _ = write!(out, "\x1b[{n}B");
                }
            }
            TerminalCmd::ColumnAbs(n) => {
                if n > 0 {
                    let _ = write!(out, "\x1b[{n}G");
                }
            }
            TerminalCmd::BracketedPasteEnable => out.push_str("\x1b[?2004h"),
            TerminalCmd::BracketedPasteDisable => out.push_str("\x1b[?2004l"),
            TerminalCmd::KittyQuery => out.push_str("\x1b[?u"),
            TerminalCmd::KittyEnable => out.push_str("\x1b[>7u"),
            TerminalCmd::KittyDisable => out.push_str("\x1b[<u"),
            TerminalCmd::QueryCellSize => out.push_str("\x1b[16t"),
        }
    }

    fn flush_streaming<T: Terminal + ?Sized>(&mut self, term: &mut T) {
        let mut buffer = String::with_capacity(OUTPUT_GATE_STREAM_CHUNK_BYTES);

        for cmd in self.cmds.drain(..) {
            match cmd {
                TerminalCmd::Bytes(data) => {
                    if !buffer.is_empty() {
                        term.write(&buffer);
                        buffer.clear();
                    }
                    if !data.is_empty() {
                        term.write(&data);
                    }
                    continue;
                }
                TerminalCmd::BytesStatic(data) if data.len() >= OUTPUT_GATE_STREAM_CHUNK_BYTES => {
                    if !buffer.is_empty() {
                        term.write(&buffer);
                        buffer.clear();
                    }
                    term.write(data);
                    continue;
                }
                cmd => {
                    Self::encode_into(&mut buffer, cmd);
                }
            }

            if buffer.len() >= OUTPUT_GATE_STREAM_CHUNK_BYTES {
                term.write(&buffer);
                buffer.clear();
            }
        }

        if !buffer.is_empty() {
            term.write(&buffer);
        }
    }

    /// Flush buffered commands to the terminal.
    ///
    /// This is the single write gate: `Terminal::write(..)` must not be called
    /// from anywhere else.
    pub fn flush<T: Terminal + ?Sized>(&mut self, term: &mut T) {
        if self.cmds.is_empty() {
            return;
        }

        let mut total_len = 0usize;
        for cmd in &self.cmds {
            total_len = total_len.saturating_add(Self::encoded_len(cmd));
        }

        if total_len > OUTPUT_GATE_STREAM_THRESHOLD_BYTES {
            self.flush_streaming(term);
            return;
        }

        let mut out = String::with_capacity(total_len);
        for cmd in self.cmds.drain(..) {
            Self::encode_into(&mut out, cmd);
        }

        if !out.is_empty() {
            term.write(&out);
        }
    }
}

/// Terminal helper methods implemented in terms of `OutputGate`.
///
/// These are intentionally out-of-band from the frame/diff render pipeline, but still flow through
/// `OutputGate` so the crate-wide "single write gate" invariant holds.
pub trait TerminalTitleExt: Terminal {
    /// Set the terminal window/tab title.
    ///
    /// TypeScript parity: `Terminal.setTitle(title)` writes `OSC 0;title BEL`.
    fn set_title(&mut self, title: &str) {
        let mut gate = OutputGate::new();

        gate.push(TerminalCmd::Bytes(osc_title_sequence(title)));
        gate.flush(self);
    }
}

impl<T: Terminal + ?Sized> TerminalTitleExt for T {}

#[cfg(test)]
mod tests {
    use super::{OutputGate, TerminalCmd, TerminalTitleExt};
    use crate::core::terminal::Terminal;

    #[derive(Default)]
    struct RecordingTerminal {
        output: String,
        writes: Vec<String>,
        write_calls: usize,
    }

    impl Terminal for RecordingTerminal {
        fn start(
            &mut self,
            _on_input: Box<dyn FnMut(String) + Send>,
            _on_resize: Box<dyn FnMut() + Send>,
        ) -> std::io::Result<()> {
            Ok(())
        }
        fn stop(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
        fn write(&mut self, data: &str) {
            self.write_calls += 1;
            self.writes.push(data.to_string());
            self.output.push_str(data);
        }
        fn columns(&self) -> u16 {
            80
        }
        fn rows(&self) -> u16 {
            24
        }
    }

    fn encode_old_per_cmd_writes(cmds: &[TerminalCmd]) -> String {
        let mut out = String::new();
        for cmd in cmds {
            match cmd {
                TerminalCmd::Bytes(data) => out.push_str(data),
                TerminalCmd::BytesStatic(data) => out.push_str(data),
                TerminalCmd::HideCursor => out.push_str("\x1b[?25l"),
                TerminalCmd::ShowCursor => out.push_str("\x1b[?25h"),
                TerminalCmd::ClearLine => out.push_str("\x1b[K"),
                TerminalCmd::ClearFromCursor => out.push_str("\x1b[J"),
                TerminalCmd::ClearScreen => out.push_str("\x1b[2J\x1b[H"),
                TerminalCmd::MoveUp(n) => {
                    if *n > 0 {
                        out.push_str(&format!("\x1b[{n}A"));
                    }
                }
                TerminalCmd::MoveDown(n) => {
                    if *n > 0 {
                        out.push_str(&format!("\x1b[{n}B"));
                    }
                }
                TerminalCmd::ColumnAbs(n) => {
                    if *n > 0 {
                        out.push_str(&format!("\x1b[{n}G"));
                    }
                }
                TerminalCmd::BracketedPasteEnable => out.push_str("\x1b[?2004h"),
                TerminalCmd::BracketedPasteDisable => out.push_str("\x1b[?2004l"),
                TerminalCmd::KittyQuery => out.push_str("\x1b[?u"),
                TerminalCmd::KittyEnable => out.push_str("\x1b[>7u"),
                TerminalCmd::KittyDisable => out.push_str("\x1b[<u"),
                TerminalCmd::QueryCellSize => out.push_str("\x1b[16t"),
            }
        }
        out
    }

    #[test]
    fn flush_coalesces_writes_and_preserves_bytes() {
        let cmds = vec![
            TerminalCmd::HideCursor,
            TerminalCmd::BytesStatic("hello"),
            TerminalCmd::Bytes(" world".to_string()),
            TerminalCmd::MoveDown(2),
            TerminalCmd::ColumnAbs(4),
            TerminalCmd::BracketedPasteEnable,
            TerminalCmd::KittyQuery,
            TerminalCmd::QueryCellSize,
            TerminalCmd::BracketedPasteDisable,
            TerminalCmd::KittyEnable,
            TerminalCmd::KittyDisable,
            TerminalCmd::ShowCursor,
        ];

        let expected = encode_old_per_cmd_writes(&cmds);

        let mut gate = OutputGate::new();
        gate.extend(cmds);

        let mut term = RecordingTerminal::default();
        gate.flush(&mut term);

        assert_eq!(term.output, expected);
        assert_eq!(term.write_calls, 1);
    }

    #[test]
    fn flush_streams_large_payloads_without_coalescing() {
        let big = "x".repeat(super::OUTPUT_GATE_STREAM_THRESHOLD_BYTES + 1);

        let mut expected = String::new();
        expected.push_str("\x1b[?25l");
        expected.push_str(&big);
        expected.push_str("\x1b[?25h");

        let mut gate = OutputGate::new();
        gate.extend([
            TerminalCmd::HideCursor,
            TerminalCmd::Bytes(big),
            TerminalCmd::ShowCursor,
        ]);

        let mut term = RecordingTerminal::default();
        gate.flush(&mut term);

        assert_eq!(term.output, expected);
        assert!(
            term.write_calls > 1,
            "expected streaming path for large payloads"
        );
    }

    #[test]
    fn flush_streaming_preserves_byte_order_across_chunk_boundaries_for_mixed_cmds() {
        let big_static: &'static str = include_str!("../runtime/tui.rs");
        assert!(
            big_static.len() >= super::OUTPUT_GATE_STREAM_CHUNK_BYTES,
            "expected include_str! payload to force BytesStatic streaming branch"
        );

        // Ensure we take the streaming path even if this file shrinks.
        let extra_len = super::OUTPUT_GATE_STREAM_THRESHOLD_BYTES
            .saturating_sub(big_static.len())
            .saturating_add(1);
        let extra = "x".repeat(extra_len);

        // Fill the streaming buffer to exactly one chunk using control commands.
        let moves_for_chunk = super::OUTPUT_GATE_STREAM_CHUNK_BYTES / "\x1b[1A".len();
        assert_eq!(
            moves_for_chunk * "\x1b[1A".len(),
            super::OUTPUT_GATE_STREAM_CHUNK_BYTES,
            "expected an integral number of MoveUp(1) encodes per chunk"
        );

        let direct_bytes = "DIRECT_BYTES";

        let mut cmds = Vec::new();
        cmds.extend(std::iter::repeat_n(TerminalCmd::MoveUp(1), moves_for_chunk));
        cmds.extend([
            TerminalCmd::BytesStatic("static-1"),
            TerminalCmd::HideCursor,
            TerminalCmd::Bytes(direct_bytes.to_string()),
            TerminalCmd::MoveDown(2),
            TerminalCmd::BytesStatic(big_static),
            TerminalCmd::Bytes(extra),
            TerminalCmd::ShowCursor,
        ]);

        let expected = encode_old_per_cmd_writes(&cmds);

        let mut gate = OutputGate::new();
        gate.extend(cmds);

        let mut term = RecordingTerminal::default();
        gate.flush(&mut term);

        assert_eq!(term.output, expected);
        assert!(
            term.write_calls > 1,
            "expected streaming path to produce multiple writes"
        );

        let chunk = term
            .writes
            .iter()
            .find(|w| w.len() == super::OUTPUT_GATE_STREAM_CHUNK_BYTES)
            .expect("expected at least one chunk-sized write");
        assert!(
            chunk
                .as_bytes()
                .chunks("\x1b[1A".len())
                .all(|c| c == b"\x1b[1A"),
            "expected chunk-sized write to be composed of repeated MoveUp(1) encodes"
        );

        assert!(
            term.writes.iter().any(|w| w == direct_bytes),
            "expected Bytes cmd to be written directly as its own write"
        );
        assert!(
            term.writes.iter().any(|w| w == big_static),
            "expected large BytesStatic cmd to be written directly"
        );
    }

    #[test]
    fn cursor_cmds_encode_to_ansi_sequences() {
        let mut gate = OutputGate::new();
        gate.extend([
            TerminalCmd::MoveUp(2),
            TerminalCmd::MoveDown(3),
            TerminalCmd::ColumnAbs(4),
        ]);

        let mut term = RecordingTerminal::default();
        gate.flush(&mut term);

        assert_eq!(term.output, "\x1b[2A\x1b[3B\x1b[4G");
        assert_eq!(term.write_calls, 1);
    }

    #[test]
    fn clear_cmds_encode_to_ansi_sequences() {
        let mut gate = OutputGate::new();
        gate.extend([
            TerminalCmd::ClearLine,
            TerminalCmd::ClearFromCursor,
            TerminalCmd::ClearScreen,
        ]);

        let mut term = RecordingTerminal::default();
        gate.flush(&mut term);

        let expected = "\x1b[K\x1b[J\x1b[2J\x1b[H";
        assert_eq!(term.output, expected);
        assert_eq!(term.write_calls, 1, "expected a single coalesced write");
        assert_eq!(term.writes.len(), 1);
        assert_eq!(term.writes[0], expected);
    }

    #[test]
    fn flush_is_noop_when_empty() {
        let mut gate = OutputGate::new();
        let mut term = RecordingTerminal::default();

        gate.flush(&mut term);

        assert_eq!(term.output, "");
        assert_eq!(term.write_calls, 0);
    }

    #[test]
    fn terminal_title_ext_writes_osc_0_title_bel() {
        let mut term = RecordingTerminal::default();
        term.set_title("pi - test");
        assert_eq!(term.output, "\x1b]0;pi - test\x07");
        assert_eq!(term.write_calls, 1);
    }

    #[test]
    fn terminal_title_ext_allows_empty_title() {
        let mut term = RecordingTerminal::default();
        term.set_title("");
        assert_eq!(term.output, "\x1b]0;\x07");
        assert_eq!(term.write_calls, 1);
    }

    #[test]
    fn terminal_title_ext_works_via_trait_object() {
        let mut term = RecordingTerminal::default();
        {
            let term_obj: &mut dyn Terminal = &mut term;
            term_obj.set_title("pi");
        }
        assert_eq!(term.output, "\x1b]0;pi\x07");
        assert_eq!(term.write_calls, 1);
    }
}
