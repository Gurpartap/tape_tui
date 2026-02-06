//! Typed terminal output commands and a single output gate.
//!
//! Invariant: all terminal writes must flow through `OutputGate::flush(..)`.

use crate::core::terminal::Terminal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalCmd {
    /// Raw bytes/control sequences (UTF-8 string) to be written to the terminal.
    Bytes(String),
    /// Static raw bytes/control sequences (UTF-8 string) to be written to the terminal.
    BytesStatic(&'static str),

    /// Cursor visibility.
    HideCursor,
    ShowCursor,

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

    /// Flush buffered commands to the terminal.
    ///
    /// This is the single write gate: `Terminal::write(..)` must not be called
    /// from anywhere else.
    pub fn flush<T: Terminal>(&mut self, term: &mut T) {
        if self.cmds.is_empty() {
            return;
        }

        let mut out = String::new();
        for cmd in self.cmds.drain(..) {
            match cmd {
                TerminalCmd::Bytes(data) => out.push_str(&data),
                TerminalCmd::BytesStatic(data) => out.push_str(data),
                TerminalCmd::HideCursor => out.push_str("\x1b[?25l"),
                TerminalCmd::ShowCursor => out.push_str("\x1b[?25h"),
                TerminalCmd::MoveUp(n) => {
                    if n > 0 {
                        out.push_str(&format!("\x1b[{n}A"));
                    }
                }
                TerminalCmd::MoveDown(n) => {
                    if n > 0 {
                        out.push_str(&format!("\x1b[{n}B"));
                    }
                }
                TerminalCmd::ColumnAbs(n) => {
                    if n > 0 {
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

        term.write(&out);
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputGate, TerminalCmd};
    use crate::core::terminal::Terminal;

    #[derive(Default)]
    struct RecordingTerminal {
        output: String,
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
    fn flush_is_noop_when_empty() {
        let mut gate = OutputGate::new();
        let mut term = RecordingTerminal::default();

        gate.flush(&mut term);

        assert_eq!(term.output, "");
        assert_eq!(term.write_calls, 0);
    }
}
