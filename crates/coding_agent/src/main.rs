use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use coding_agent::app::{system_instructions_from_env, App};
use coding_agent::provider::RunMessage;
use coding_agent::providers;
use coding_agent::runtime::RuntimeController;
use coding_agent::tui::AppComponent;
use session_store::{SessionSeed, SessionStore};
use tape_tui::{prewarm_markdown_highlighting, ProcessTerminal, TUI};

const USAGE: &str =
    "Usage:\n  coding_agent\n  coding_agent --continue\n  coding_agent --session <session-filepath>";

#[derive(Debug, Clone, PartialEq, Eq)]
enum StartupMode {
    NewSession,
    ContinueLatest,
    ContinuePath(PathBuf),
}

struct StartupSession {
    persistence: StartupSessionPersistence,
    startup_session_id: String,
    replayed_messages: Vec<RunMessage>,
}

enum StartupSessionPersistence {
    Deferred(SessionSeed),
    Active(SessionStore),
}

fn main() {
    if let Err(error) = run() {
        if error.kind() == io::ErrorKind::InvalidInput {
            eprintln!("{}", format_cli_parse_error(&error.to_string()));
            std::process::exit(2);
        }

        eprintln!("✖ {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let _ = std::thread::Builder::new()
        .name("markdown-highlight-prewarm".to_string())
        .spawn(prewarm_markdown_highlighting);

    let startup_mode = parse_startup_mode(std::env::args().skip(1))?;
    let cwd = std::env::current_dir().map_err(io::Error::other)?;
    let startup = load_startup_session(&cwd, startup_mode).map_err(io::Error::other)?;

    let system_instructions = system_instructions_from_env();
    let mut app_state = App::with_system_instructions(Some(system_instructions));
    if !startup.replayed_messages.is_empty() {
        app_state.restore_conversation(startup.replayed_messages);
    }
    let app = Arc::new(Mutex::new(app_state));

    let terminal = ProcessTerminal::new();
    let mut tui = TUI::new(terminal);
    let runtime_handle = tui.runtime_handle();

    let provider = providers::provider_from_env_with_session_id(Some(&startup.startup_session_id))
        .map_err(io::Error::other)?;
    let provider_profile = provider.profile();

    let host = match startup.persistence {
        StartupSessionPersistence::Deferred(seed) => {
            RuntimeController::new_with_deferred_session_seed(
                Arc::clone(&app),
                runtime_handle,
                provider,
                seed,
            )
        }
        StartupSessionPersistence::Active(session_store) => {
            RuntimeController::new_with_session_store(
                Arc::clone(&app),
                runtime_handle,
                provider,
                session_store,
            )
        }
    };
    let root_component = tui.register_component(AppComponent::new(
        Arc::clone(&app),
        Arc::clone(&host),
        provider_profile,
    ));
    tui.set_root(vec![root_component]);
    tui.set_focus(root_component);
    tui.set_low_latency_coalescing(false);

    tui.start()?;

    while !lock_unpoisoned(&app).should_exit {
        tui.run_blocking_once();
    }

    tui.stop()
}

fn format_cli_parse_error(error: &str) -> String {
    let (summary, usage) = match error.split_once("\nUsage:\n") {
        Some((summary, usage_tail)) => (summary.trim(), format!("Usage:\n{usage_tail}")),
        None => (error.trim(), USAGE.to_string()),
    };

    format!(
        "✖ Invalid command line arguments\n\n  {summary}\n\n{usage}",
        summary = summary,
        usage = usage
    )
}

fn parse_startup_mode(args: impl IntoIterator<Item = String>) -> io::Result<StartupMode> {
    let mut mode: Option<StartupMode> = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--continue" => {
                if mode.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Cannot combine --continue with other session flags\n{USAGE}"),
                    ));
                }

                mode = Some(StartupMode::ContinueLatest);
            }
            "--session" => {
                let session_path = args.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Missing required value for --session\n{USAGE}"),
                    )
                })?;

                if mode.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Cannot combine --session with other session flags\n{USAGE}"),
                    ));
                }

                mode = Some(StartupMode::ContinuePath(PathBuf::from(session_path)));
            }
            unknown => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unknown argument: {unknown}\n{USAGE}"),
                ));
            }
        }
    }

    Ok(mode.unwrap_or(StartupMode::NewSession))
}

fn load_startup_session(cwd: &Path, startup_mode: StartupMode) -> Result<StartupSession, String> {
    match startup_mode {
        StartupMode::NewSession => {
            let seed = SessionSeed::new(cwd).map_err(|error| error.to_string())?;
            let startup_session_id = seed.session_id.clone();
            Ok(StartupSession {
                persistence: StartupSessionPersistence::Deferred(seed),
                startup_session_id,
                replayed_messages: Vec::new(),
            })
        }
        StartupMode::ContinueLatest => {
            let latest_session_path =
                SessionStore::latest_session_path(cwd).map_err(|error| error.to_string())?;
            let session_store =
                SessionStore::open(&latest_session_path).map_err(|error| error.to_string())?;
            let replayed_messages = session_store
                .replay_leaf(None)
                .map_err(|error| error.to_string())?;
            let startup_session_id = session_store.session_id().to_string();

            Ok(StartupSession {
                persistence: StartupSessionPersistence::Active(session_store),
                startup_session_id,
                replayed_messages,
            })
        }
        StartupMode::ContinuePath(path) => {
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            let session_store = SessionStore::open(&path).map_err(|error| error.to_string())?;
            let replayed_messages = session_store
                .replay_leaf(None)
                .map_err(|error| error.to_string())?;
            let startup_session_id = session_store.session_id().to_string();

            Ok(StartupSession {
                persistence: StartupSessionPersistence::Active(session_store),
                startup_session_id,
                replayed_messages,
            })
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use session_store::{session_root, SessionEntry, SessionEntryKind};

    use super::*;

    #[test]
    fn parse_startup_mode_rejects_unknown_flags() {
        let error = parse_startup_mode(["--bogus".to_string()])
            .expect_err("unknown flag must fail with usage");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("Unknown argument: --bogus"));
        assert!(error.to_string().contains(USAGE));
    }

    #[test]
    fn parse_startup_mode_reports_unexpected_extra_arg_after_continue() {
        let error = parse_startup_mode(["--continue".to_string(), "extra".to_string()])
            .expect_err("unexpected extra arg after --continue should fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("Unknown argument: extra"));
        assert!(error.to_string().contains(USAGE));
    }

    #[test]
    fn parse_startup_mode_requires_session_path_value() {
        let error =
            parse_startup_mode(["--session".to_string()]).expect_err("missing session path");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error
            .to_string()
            .contains("Missing required value for --session"));
        assert!(error.to_string().contains(USAGE));
    }

    #[test]
    fn parse_startup_mode_rejects_combined_continue_and_session_flags() {
        let error = parse_startup_mode([
            "--continue".to_string(),
            "--session".to_string(),
            "session.jsonl".to_string(),
        ])
        .expect_err("conflicting flags must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error
            .to_string()
            .contains("Cannot combine --session with other session flags"));
        assert!(error.to_string().contains(USAGE));
    }

    #[test]
    fn parse_startup_mode_supports_explicit_session_file() {
        let mode =
            parse_startup_mode(["--session".to_string(), "path/to/session.jsonl".to_string()])
                .expect("--session with path should parse");

        assert_eq!(
            mode,
            StartupMode::ContinuePath(PathBuf::from("path/to/session.jsonl"))
        );
    }

    #[test]
    fn continue_mode_loads_replay_and_session_id_from_latest_header() {
        let cwd = tempfile::tempdir().expect("tempdir should be created");
        let mut store = SessionStore::create_new(cwd.path()).expect("session should be created");
        let expected_session_id = store.session_id().to_string();

        store
            .append(SessionEntry::new(
                "entry-00000000000000000001",
                None::<String>,
                "2026-02-14T00:00:01Z",
                SessionEntryKind::UserText {
                    text: "hello".to_string(),
                },
            ))
            .expect("seed entry should append");

        let startup = load_startup_session(cwd.path(), StartupMode::ContinueLatest)
            .expect("continue startup should load latest session");

        assert_eq!(startup.startup_session_id, expected_session_id);
        assert_eq!(
            startup.replayed_messages,
            vec![RunMessage::UserText {
                text: "hello".to_string()
            }]
        );
    }

    #[test]
    fn default_mode_returns_deferred_seed_without_creating_session_root() {
        let cwd = tempfile::tempdir().expect("tempdir should be created");
        let startup = load_startup_session(cwd.path(), StartupMode::NewSession)
            .expect("new-session startup should succeed");
        let StartupSession {
            persistence,
            startup_session_id,
            replayed_messages,
        } = startup;

        let sessions_root = session_root(cwd.path());
        assert!(
            !sessions_root.exists(),
            "default startup must not eagerly materialize session root"
        );
        assert!(replayed_messages.is_empty());

        match persistence {
            StartupSessionPersistence::Deferred(seed) => {
                assert_eq!(startup_session_id, seed.session_id);
                assert_eq!(seed.cwd, cwd.path().to_path_buf());
            }
            StartupSessionPersistence::Active(_) => {
                panic!("default startup should produce deferred persistence")
            }
        }
    }

    #[test]
    fn continue_mode_fails_closed_on_malformed_latest_session() {
        let cwd = tempfile::tempdir().expect("tempdir should be created");
        let sessions_root = session_root(cwd.path());
        fs::create_dir_all(&sessions_root).expect("session root should be created");
        let malformed = sessions_root.join("broken.jsonl");
        fs::write(&malformed, "{not json\n").expect("malformed file should be written");

        let error = match load_startup_session(cwd.path(), StartupMode::ContinueLatest) {
            Ok(_) => panic!("malformed latest session must hard-fail startup"),
            Err(error) => error,
        };
        assert!(error.contains("failed to parse JSON"));
    }

    #[test]
    fn explicit_session_mode_loads_replay_and_session_id_from_file_path() {
        let cwd = tempfile::tempdir().expect("tempdir should be created");
        let mut store = SessionStore::create_new(cwd.path()).expect("session should be created");
        let session_path = store.path().to_path_buf();
        let expected_session_id = store.session_id().to_string();

        store
            .append(SessionEntry::new(
                "entry-00000000000000000001",
                None::<String>,
                "2026-02-14T00:00:01Z",
                SessionEntryKind::UserText {
                    text: "explicit".to_string(),
                },
            ))
            .expect("seed entry should append");

        let startup = load_startup_session(cwd.path(), StartupMode::ContinuePath(session_path))
            .expect("explicit session startup should load session");

        assert_eq!(startup.startup_session_id, expected_session_id);
        assert_eq!(
            startup.replayed_messages,
            vec![RunMessage::UserText {
                text: "explicit".to_string()
            }]
        );
    }

    #[test]
    fn explicit_session_mode_supports_relative_file_path_from_cwd() {
        let cwd = tempfile::tempdir().expect("tempdir should be created");
        let mut store = SessionStore::create_new(cwd.path()).expect("session should be created");
        let session_path = store.path().to_path_buf();
        let relative_path = session_path
            .strip_prefix(cwd.path())
            .expect("session path should be under cwd")
            .to_path_buf();
        let expected_session_id = store.session_id().to_string();

        store
            .append(SessionEntry::new(
                "entry-00000000000000000001",
                None::<String>,
                "2026-02-14T00:00:01Z",
                SessionEntryKind::UserText {
                    text: "relative".to_string(),
                },
            ))
            .expect("seed entry should append");

        let startup = load_startup_session(cwd.path(), StartupMode::ContinuePath(relative_path))
            .expect("relative explicit session startup should load session");

        assert_eq!(startup.startup_session_id, expected_session_id);
        assert_eq!(
            startup.replayed_messages,
            vec![RunMessage::UserText {
                text: "relative".to_string()
            }]
        );
    }

    #[test]
    fn format_cli_parse_error_wraps_summary_and_usage_consistently() {
        let formatted = format_cli_parse_error(
            "Missing required value for --session\nUsage:\n  coding_agent\n  coding_agent --continue\n  coding_agent --session <session-filepath>",
        );

        assert!(formatted.starts_with("✖ Invalid command line arguments"));
        assert!(formatted.contains("Missing required value for --session"));
        assert!(formatted.contains("Usage:"));
        assert!(formatted.contains("coding_agent --session <session-filepath>"));
    }

    #[test]
    fn format_cli_parse_error_falls_back_to_default_usage_when_missing() {
        let formatted = format_cli_parse_error("Unknown argument: asd");

        assert!(formatted.starts_with("✖ Invalid command line arguments"));
        assert!(formatted.contains("Unknown argument: asd"));
        assert!(formatted.contains(USAGE));
    }
}
