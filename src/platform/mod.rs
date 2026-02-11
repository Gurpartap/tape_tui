//! Platform-specific terminal integrations.

pub mod process_terminal;
pub mod stdin_buffer;

pub use process_terminal::{
    install_panic_hook, install_signal_handlers, PanicHookGuard, ProcessTerminal, SignalHookGuard,
};
