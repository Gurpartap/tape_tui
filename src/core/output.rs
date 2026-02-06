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
        for cmd in self.cmds.drain(..) {
            match cmd {
                TerminalCmd::Bytes(data) => term.write(&data),
                TerminalCmd::BytesStatic(data) => term.write(data),
                TerminalCmd::HideCursor => term.write("\x1b[?25l"),
                TerminalCmd::ShowCursor => term.write("\x1b[?25h"),
                TerminalCmd::BracketedPasteEnable => term.write("\x1b[?2004h"),
                TerminalCmd::BracketedPasteDisable => term.write("\x1b[?2004l"),
                TerminalCmd::KittyQuery => term.write("\x1b[?u"),
                TerminalCmd::KittyEnable => term.write("\x1b[>7u"),
                TerminalCmd::KittyDisable => term.write("\x1b[<u"),
                TerminalCmd::QueryCellSize => term.write("\x1b[16t"),
            }
        }
    }
}
