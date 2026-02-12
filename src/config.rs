//! Environment configuration.

use std::env;

#[derive(Debug, Clone)]
pub struct EnvConfig {
    pub hardware_cursor: bool,
    pub clear_on_shrink: bool,
    pub tui_write_log: Option<String>,
    pub tui_debug: bool,
    pub debug_redraw: bool,
}

impl EnvConfig {
    pub fn from_env() -> Self {
        Self {
            hardware_cursor: env_flag("TAPE_HARDWARE_CURSOR"),
            clear_on_shrink: env_flag("TAPE_CLEAR_ON_SHRINK"),
            tui_write_log: env_string_opt("tape_tui_WRITE_LOG"),
            tui_debug: env_flag("tape_tui_DEBUG"),
            debug_redraw: env_flag("TAPE_DEBUG_REDRAW"),
        }
    }
}

fn env_flag(key: &str) -> bool {
    env::var(key).map(|value| value == "1").unwrap_or(false)
}

fn env_string_opt(key: &str) -> Option<String> {
    env::var(key).ok().and_then(|value| {
        if value.trim().is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::EnvConfig;
    use std::env;
    use std::sync::{Mutex, OnceLock};

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                env::set_var(self.key, value);
            } else {
                env::remove_var(self.key);
            }
        }
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    fn set_env_guard(key: &'static str, value: Option<&str>) -> EnvGuard {
        let previous = env::var(key).ok();
        if let Some(value) = value {
            env::set_var(key, value);
        } else {
            env::remove_var(key);
        }
        EnvGuard { key, previous }
    }

    #[test]
    fn env_defaults_are_false() {
        let _lock = env_lock();
        let _g1 = set_env_guard("TAPE_HARDWARE_CURSOR", None);
        let _g2 = set_env_guard("TAPE_CLEAR_ON_SHRINK", None);
        let _g3 = set_env_guard("tape_tui_WRITE_LOG", None);
        let _g4 = set_env_guard("tape_tui_DEBUG", None);
        let _g5 = set_env_guard("TAPE_DEBUG_REDRAW", None);

        let config = EnvConfig::from_env();
        assert!(!config.hardware_cursor);
        assert!(!config.clear_on_shrink);
        assert!(config.tui_write_log.is_none());
        assert!(!config.tui_debug);
        assert!(!config.debug_redraw);
    }

    #[test]
    fn env_flags_set_to_one_enable() {
        let _lock = env_lock();
        let _g1 = set_env_guard("TAPE_HARDWARE_CURSOR", Some("1"));
        let _g2 = set_env_guard("TAPE_CLEAR_ON_SHRINK", Some("1"));
        let _g3 = set_env_guard("tape_tui_WRITE_LOG", Some("/tmp/tape.log"));
        let _g4 = set_env_guard("tape_tui_DEBUG", Some("1"));
        let _g5 = set_env_guard("TAPE_DEBUG_REDRAW", Some("1"));

        let config = EnvConfig::from_env();
        assert!(config.hardware_cursor);
        assert!(config.clear_on_shrink);
        assert_eq!(config.tui_write_log.as_deref(), Some("/tmp/tape.log"));
        assert!(config.tui_debug);
        assert!(config.debug_redraw);
    }

    #[test]
    fn empty_write_log_is_ignored() {
        let _lock = env_lock();
        let _g1 = set_env_guard("tape_tui_WRITE_LOG", Some(""));
        let config = EnvConfig::from_env();
        assert!(config.tui_write_log.is_none());
    }
}
