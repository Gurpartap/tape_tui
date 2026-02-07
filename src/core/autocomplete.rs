//! Autocomplete providers and helpers (Phase 14).

use std::collections::HashSet;
use std::fs::{read_dir, symlink_metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::core::fuzzy::fuzzy_filter;

const FD_MAX_BUFFER: usize = 10 * 1024 * 1024;

fn path_delimiters() -> &'static HashSet<char> {
    static DELIMITERS: std::sync::OnceLock<HashSet<char>> = std::sync::OnceLock::new();
    DELIMITERS.get_or_init(|| {
        [" ", "\t", "\"", "'", "="]
            .iter()
            .map(|s| s.chars().next().unwrap())
            .collect()
    })
}

fn find_last_delimiter(text: &str) -> Option<usize> {
    for (idx, ch) in text.char_indices().rev() {
        if path_delimiters().contains(&ch) {
            return Some(idx);
        }
    }
    None
}

fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut quote_start = None;

    for (idx, ch) in text.char_indices() {
        if ch == '"' {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = Some(idx);
            }
        }
    }

    if in_quotes {
        quote_start
    } else {
        None
    }
}

fn is_token_start(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    text[..index]
        .chars()
        .last()
        .map(|ch| path_delimiters().contains(&ch))
        .unwrap_or(true)
}

fn extract_quoted_prefix(text: &str) -> Option<String> {
    let quote_start = find_unclosed_quote_start(text)?;

    if quote_start > 0 {
        let before = text[..quote_start].chars().last();
        if before == Some('@') {
            if !is_token_start(text, quote_start - 1) {
                return None;
            }
            return text.get(quote_start - 1..).map(|value| value.to_string());
        }
    }

    if !is_token_start(text, quote_start) {
        return None;
    }

    text.get(quote_start..).map(|value| value.to_string())
}

#[derive(Debug, Clone)]
struct ParsedPathPrefix {
    raw_prefix: String,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

fn parse_path_prefix(prefix: &str) -> ParsedPathPrefix {
    if let Some(rest) = prefix.strip_prefix("@\"") {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('"') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('@') {
        return ParsedPathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: false,
        };
    }
    ParsedPathPrefix {
        raw_prefix: prefix.to_string(),
        is_at_prefix: false,
        is_quoted_prefix: false,
    }
}

fn build_completion_value(path: &str, options: &CompletionOptions) -> String {
    let needs_quotes = options.is_quoted_prefix || path.contains(' ');
    let prefix = if options.is_at_prefix { "@" } else { "" };

    if !needs_quotes {
        return format!("{prefix}{path}");
    }

    format!("{prefix}\"{path}\"")
}

fn dirname(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(idx) => &path[..idx],
        None => ".",
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or("")
}

fn join_path(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir == "." {
        return name.to_string();
    }
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

#[derive(Debug, Clone)]
struct CompletionOptions {
    #[allow(dead_code)]
    is_directory: bool,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

#[derive(Debug, Clone)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AutocompleteSuggestions {
    pub items: Vec<AutocompleteItem>,
    pub prefix: String,
}

#[derive(Clone)]
pub struct SlashCommand {
    pub name: String,
    pub description: Option<String>,
    pub get_argument_completions:
        Option<Arc<dyn Fn(&str) -> Option<Vec<AutocompleteItem>> + Send + Sync>>,
}

#[derive(Clone)]
pub enum CommandEntry {
    Command(SlashCommand),
    Item(AutocompleteItem),
}

impl CommandEntry {
    fn name(&self) -> &str {
        match self {
            CommandEntry::Command(cmd) => cmd.name.as_str(),
            CommandEntry::Item(item) => item.value.as_str(),
        }
    }

    fn label(&self) -> &str {
        match self {
            CommandEntry::Command(cmd) => cmd.name.as_str(),
            CommandEntry::Item(item) => item.label.as_str(),
        }
    }

    fn description(&self) -> Option<&str> {
        match self {
            CommandEntry::Command(cmd) => cmd.description.as_deref(),
            CommandEntry::Item(item) => item.description.as_deref(),
        }
    }

    fn argument_completions(&self, prefix: &str) -> Option<Vec<AutocompleteItem>> {
        match self {
            CommandEntry::Command(cmd) => cmd
                .get_argument_completions
                .as_ref()
                .and_then(|handler| handler(prefix)),
            CommandEntry::Item(_) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AbortSignal {
    aborted: Arc<AtomicBool>,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self {
            aborted: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

pub type SuggestionUpdate = Box<dyn Fn(AutocompleteSuggestions) + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub struct CompletionResult {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

pub trait AutocompleteProvider {
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> Option<AutocompleteSuggestions>;

    fn get_force_file_suggestions(
        &self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
    ) -> Option<AutocompleteSuggestions> {
        None
    }

    fn should_trigger_file_completion(
        &self,
        _lines: &[String],
        _cursor_line: usize,
        _cursor_col: usize,
    ) -> bool {
        true
    }

    fn get_suggestions_async(
        &self,
        _lines: Vec<String>,
        _cursor_line: usize,
        _cursor_col: usize,
        _signal: Option<AbortSignal>,
        _on_update: Option<SuggestionUpdate>,
    ) -> Option<JoinHandle<Option<AutocompleteSuggestions>>> {
        None
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionResult;
}

#[derive(Clone)]
pub struct CombinedAutocompleteProvider {
    commands: Vec<CommandEntry>,
    base_path: PathBuf,
    fd_path: Option<PathBuf>,
}

impl CombinedAutocompleteProvider {
    pub fn new(commands: Vec<CommandEntry>, base_path: PathBuf, fd_path: Option<PathBuf>) -> Self {
        Self {
            commands,
            base_path,
            fd_path,
        }
    }

    pub fn get_force_file_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let text_before_cursor = current_line.get(..cursor_col).unwrap_or(current_line);

        if text_before_cursor.trim().starts_with('/') && !text_before_cursor.trim().contains(' ') {
            return None;
        }

        let path_match = self.extract_path_prefix(text_before_cursor, true);
        if let Some(path_prefix) = path_match {
            let suggestions = self.get_file_suggestions(&path_prefix);
            if suggestions.is_empty() {
                return None;
            }
            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: path_prefix,
            });
        }

        None
    }

    pub fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let text_before_cursor = current_line.get(..cursor_col).unwrap_or(current_line);

        if text_before_cursor.trim().starts_with('/') && !text_before_cursor.trim().contains(' ') {
            return false;
        }

        true
    }

    fn extract_at_prefix(&self, text: &str) -> Option<String> {
        if let Some(prefix) = extract_quoted_prefix(text) {
            if prefix.starts_with("@\"") {
                return Some(prefix);
            }
        }

        let last_delim = find_last_delimiter(text);
        let token_start = last_delim.map(|idx| idx + 1).unwrap_or(0);

        if text.get(token_start..token_start + 1) == Some("@") {
            return Some(text[token_start..].to_string());
        }

        None
    }

    fn extract_path_prefix(&self, text: &str, force_extract: bool) -> Option<String> {
        if let Some(prefix) = extract_quoted_prefix(text) {
            return Some(prefix);
        }

        let last_delim = find_last_delimiter(text);
        let path_prefix = match last_delim {
            Some(idx) => text.get(idx + 1..).unwrap_or("").to_string(),
            None => text.to_string(),
        };

        if force_extract {
            return Some(path_prefix);
        }

        if path_prefix.contains('/')
            || path_prefix.starts_with('.')
            || path_prefix.starts_with("~/")
        {
            return Some(path_prefix);
        }

        if path_prefix.is_empty() && text.ends_with(' ') {
            return Some(path_prefix);
        }

        None
    }

    fn expand_home_path(&self, path: &str) -> String {
        let home = std::env::var("HOME").unwrap_or_default();
        if let Some(rest) = path.strip_prefix("~/") {
            let mut expanded = Path::new(&home).join(rest).to_string_lossy().to_string();
            if path.ends_with('/') && !expanded.ends_with('/') {
                expanded.push('/');
            }
            return expanded;
        }
        if path == "~" {
            return home;
        }
        path.to_string()
    }

    fn get_file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let parsed = parse_path_prefix(prefix);
        let mut expanded_prefix = parsed.raw_prefix.clone();

        if expanded_prefix.starts_with('~') {
            expanded_prefix = self.expand_home_path(&expanded_prefix);
        }

        let is_root_prefix = parsed.raw_prefix.is_empty()
            || parsed.raw_prefix == "./"
            || parsed.raw_prefix == "../"
            || parsed.raw_prefix == "~"
            || parsed.raw_prefix == "~/"
            || parsed.raw_prefix == "/"
            || (parsed.is_at_prefix && parsed.raw_prefix.is_empty());

        let (search_dir, search_prefix) = if is_root_prefix || parsed.raw_prefix.ends_with('/') {
            let dir = if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                expanded_prefix.clone()
            } else {
                self.base_path
                    .join(&expanded_prefix)
                    .to_string_lossy()
                    .to_string()
            };
            (dir, String::new())
        } else {
            let dir = dirname(&expanded_prefix).to_string();
            let file = basename(&expanded_prefix).to_string();
            let search_dir =
                if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                    dir
                } else {
                    self.base_path.join(dir).to_string_lossy().to_string()
                };
            (search_dir, file)
        };

        let entries = match read_dir(&search_dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };

        let mut suggestions = Vec::new();
        let search_prefix_lower = search_prefix.to_lowercase();

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.to_lowercase().starts_with(&search_prefix_lower) {
                continue;
            }

            let mut is_directory = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if !is_directory {
                if let Ok(metadata) = symlink_metadata(entry.path()) {
                    if metadata.file_type().is_symlink() {
                        if let Ok(target_meta) = entry.path().metadata() {
                            is_directory = target_meta.is_dir();
                        }
                    }
                }
            }

            let display_prefix = parsed.raw_prefix.clone();
            let relative_path = if display_prefix.ends_with('/') {
                format!("{display_prefix}{name}")
            } else if display_prefix.contains('/') {
                if display_prefix.starts_with("~/") {
                    let home_relative = display_prefix
                        .strip_prefix("~/")
                        .expect("display_prefix starts with ~/");
                    let dir = dirname(home_relative);
                    if dir == "." {
                        format!("~/{name}")
                    } else {
                        format!("~/{}/{}", dir, name)
                    }
                } else if display_prefix.starts_with('/') {
                    let dir = dirname(&display_prefix);
                    if dir == "/" {
                        format!("/{name}")
                    } else {
                        format!("{}/{}", dir, name)
                    }
                } else {
                    join_path(dirname(&display_prefix), &name)
                }
            } else if display_prefix.starts_with('~') {
                format!("~/{name}")
            } else {
                name.clone()
            };

            let path_value = if is_directory {
                format!("{relative_path}/")
            } else {
                relative_path
            };

            let value = build_completion_value(
                &path_value,
                &CompletionOptions {
                    is_directory,
                    is_at_prefix: parsed.is_at_prefix,
                    is_quoted_prefix: parsed.is_quoted_prefix,
                },
            );

            suggestions.push(AutocompleteItem {
                value,
                label: format!("{}{}", name, if is_directory { "/" } else { "" }),
                description: None,
            });
        }

        suggestions.sort_by(|a, b| {
            let a_is_dir = a.value.ends_with('/');
            let b_is_dir = b.value.ends_with('/');
            if a_is_dir && !b_is_dir {
                return std::cmp::Ordering::Less;
            }
            if !a_is_dir && b_is_dir {
                return std::cmp::Ordering::Greater;
            }
            a.label.cmp(&b.label)
        });

        suggestions
    }

    fn score_entry(&self, file_path: &str, query: &str, is_directory: bool) -> i32 {
        let file_name = basename(file_path).to_lowercase();
        let lower_query = query.to_lowercase();
        let lower_path = file_path.to_lowercase();

        let mut score = 0;
        if file_name == lower_query {
            score = 100;
        } else if file_name.starts_with(&lower_query) {
            score = 80;
        } else if file_name.contains(&lower_query) {
            score = 50;
        } else if lower_path.contains(&lower_query) {
            score = 30;
        }

        if is_directory && score > 0 {
            score += 10;
        }

        score
    }

    fn build_fuzzy_suggestions(
        &self,
        entries: Vec<FuzzyEntry>,
        is_quoted_prefix: bool,
    ) -> Vec<AutocompleteItem> {
        let mut sorted_entries = entries;
        sorted_entries.sort_by(|a, b| {
            let score_diff = b.score.cmp(&a.score);
            if score_diff != std::cmp::Ordering::Equal {
                return score_diff;
            }
            a.path.cmp(&b.path)
        });
        let top_entries = sorted_entries.into_iter().take(20);

        let mut suggestions = Vec::new();
        for entry in top_entries {
            let path_without_slash = if entry.is_directory {
                entry.path.strip_suffix('/').unwrap_or(&entry.path)
            } else {
                entry.path.as_str()
            };
            let entry_name = basename(path_without_slash).to_string();
            let value = build_completion_value(
                &entry.path,
                &CompletionOptions {
                    is_directory: entry.is_directory,
                    is_at_prefix: true,
                    is_quoted_prefix,
                },
            );

            suggestions.push(AutocompleteItem {
                value,
                label: format!(
                    "{}{}",
                    entry_name,
                    if entry.is_directory { "/" } else { "" }
                ),
                description: Some(path_without_slash.to_string()),
            });
        }

        suggestions
    }

    fn get_fuzzy_file_suggestions(
        &self,
        query: &str,
        is_quoted_prefix: bool,
        signal: Option<AbortSignal>,
        on_update: Option<Arc<dyn Fn(Vec<AutocompleteItem>) + Send + Sync>>,
    ) -> Vec<AutocompleteItem> {
        let fd_path = match self.fd_path.as_ref() {
            Some(path) => path.clone(),
            None => return Vec::new(),
        };

        let mut scored_entries: Vec<FuzzyEntry> = Vec::new();

        let handle_entry = |entry: DirectoryEntry| {
            if let Some(sig) = &signal {
                if sig.is_aborted() {
                    return;
                }
            }
            let score = if query.is_empty() {
                1
            } else {
                self.score_entry(&entry.path, query, entry.is_directory)
            };
            if score <= 0 {
                return;
            }
            scored_entries.push(FuzzyEntry {
                path: entry.path,
                is_directory: entry.is_directory,
                score,
            });
            if let Some(callback) = &on_update {
                let suggestions =
                    self.build_fuzzy_suggestions(scored_entries.clone(), is_quoted_prefix);
                if !suggestions.is_empty() {
                    callback(suggestions);
                }
            }
        };

        if let Some(sig) = &signal {
            if sig.is_aborted() {
                return Vec::new();
            }
        }

        let entries = walk_directory_with_fd(
            &self.base_path,
            &fd_path,
            query,
            100,
            signal.clone(),
            Some(handle_entry),
        );
        let _ = entries;

        self.build_fuzzy_suggestions(scored_entries, is_quoted_prefix)
    }
}

impl AutocompleteProvider for CombinedAutocompleteProvider {
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let text_before_cursor = current_line.get(..cursor_col).unwrap_or(current_line);

        if let Some(at_prefix) = self.extract_at_prefix(text_before_cursor) {
            return Some(AutocompleteSuggestions {
                items: Vec::new(),
                prefix: at_prefix,
            });
        }

        if text_before_cursor.starts_with('/') {
            if let Some(space_index) = text_before_cursor.find(' ') {
                let command_name = &text_before_cursor[1..space_index];
                let argument_text = &text_before_cursor[space_index + 1..];

                if let Some(command) = self
                    .commands
                    .iter()
                    .find(|entry| entry.name() == command_name)
                {
                    if let Some(argument_suggestions) = command.argument_completions(argument_text)
                    {
                        if argument_suggestions.is_empty() {
                            return None;
                        }
                        return Some(AutocompleteSuggestions {
                            items: argument_suggestions,
                            prefix: argument_text.to_string(),
                        });
                    }
                }

                return None;
            }

            let prefix = &text_before_cursor[1..];
            let command_items: Vec<CommandInfo> = self
                .commands
                .iter()
                .map(|entry| CommandInfo {
                    name: entry.name().to_string(),
                    label: entry.label().to_string(),
                    description: entry.description().map(|d| d.to_string()),
                })
                .collect();

            let filtered = fuzzy_filter(&command_items, prefix, |item| item.name.clone());
            if filtered.is_empty() {
                return None;
            }

            let items = filtered
                .into_iter()
                .map(|item| AutocompleteItem {
                    value: item.name,
                    label: item.label,
                    description: item.description,
                })
                .collect();

            return Some(AutocompleteSuggestions {
                items,
                prefix: text_before_cursor.to_string(),
            });
        }

        if let Some(path_prefix) = self.extract_path_prefix(text_before_cursor, false) {
            let suggestions = self.get_file_suggestions(&path_prefix);
            if suggestions.is_empty() {
                return None;
            }
            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: path_prefix,
            });
        }

        None
    }

    fn get_force_file_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> Option<AutocompleteSuggestions> {
        CombinedAutocompleteProvider::get_force_file_suggestions(
            self,
            lines,
            cursor_line,
            cursor_col,
        )
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        CombinedAutocompleteProvider::should_trigger_file_completion(
            self,
            lines,
            cursor_line,
            cursor_col,
        )
    }

    fn get_suggestions_async(
        &self,
        lines: Vec<String>,
        cursor_line: usize,
        cursor_col: usize,
        signal: Option<AbortSignal>,
        on_update: Option<SuggestionUpdate>,
    ) -> Option<JoinHandle<Option<AutocompleteSuggestions>>> {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let text_before_cursor = current_line
            .get(..cursor_col)
            .unwrap_or(current_line)
            .to_string();
        let at_prefix = self.extract_at_prefix(&text_before_cursor)?;

        let ParsedPathPrefix {
            raw_prefix,
            is_quoted_prefix,
            ..
        } = parse_path_prefix(&at_prefix);

        let base_path = self.base_path.clone();
        let fd_path = self.fd_path.clone()?;
        let update: Option<Arc<dyn Fn(AutocompleteSuggestions) + Send + Sync>> =
            on_update.map(Arc::from);
        let signal_clone = signal.clone();

        Some(thread::spawn(move || {
            if let Some(sig) = &signal_clone {
                if sig.is_aborted() {
                    return None;
                }
            }

            let provider = CombinedAutocompleteProvider::new(Vec::new(), base_path, Some(fd_path));
            let suggestions = provider.get_fuzzy_file_suggestions(
                &raw_prefix,
                is_quoted_prefix,
                signal_clone,
                update.map(|callback| {
                    let prefix = at_prefix.clone();
                    Arc::new(move |items: Vec<AutocompleteItem>| {
                        callback(AutocompleteSuggestions {
                            items,
                            prefix: prefix.clone(),
                        });
                    }) as Arc<dyn Fn(Vec<AutocompleteItem>) + Send + Sync>
                }),
            );

            if suggestions.is_empty() {
                return None;
            }

            Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: at_prefix,
            })
        }))
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionResult {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let prefix_len = prefix.len();
        let before_prefix_len = cursor_col.saturating_sub(prefix_len);
        let before_prefix = current_line.get(..before_prefix_len).unwrap_or("");
        let after_cursor = current_line.get(cursor_col..).unwrap_or("");

        let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let has_leading_quote_after_cursor = after_cursor.starts_with('"');
        let has_trailing_quote_in_item = item.value.ends_with('"');

        let adjusted_after_cursor =
            if is_quoted_prefix && has_trailing_quote_in_item && has_leading_quote_after_cursor {
                after_cursor.get(1..).unwrap_or("")
            } else {
                after_cursor
            };

        let is_slash_command = prefix.starts_with('/')
            && before_prefix.trim().is_empty()
            && !prefix[1..].contains('/');
        if is_slash_command {
            let new_line = format!("{}/{} {}", before_prefix, item.value, adjusted_after_cursor);
            let mut new_lines = lines.to_vec();
            new_lines[cursor_line] = new_line;
            return CompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + item.value.len() + 2,
            };
        }

        if prefix.starts_with('@') {
            let is_directory = item.label.ends_with('/');
            let suffix = if is_directory { "" } else { " " };
            let new_line = format!(
                "{}{}{}{}",
                before_prefix, item.value, suffix, adjusted_after_cursor
            );
            let mut new_lines = lines.to_vec();
            new_lines[cursor_line] = new_line;

            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                item.value.len().saturating_sub(1)
            } else {
                item.value.len()
            };

            return CompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + cursor_offset + suffix.len(),
            };
        }

        let text_before_cursor = current_line.get(..cursor_col).unwrap_or("");
        if text_before_cursor.contains('/') && text_before_cursor.contains(' ') {
            let new_line = format!("{}{}{}", before_prefix, item.value, adjusted_after_cursor);
            let mut new_lines = lines.to_vec();
            new_lines[cursor_line] = new_line;

            let is_directory = item.label.ends_with('/');
            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                item.value.len().saturating_sub(1)
            } else {
                item.value.len()
            };

            return CompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: before_prefix.len() + cursor_offset,
            };
        }

        let new_line = format!("{}{}{}", before_prefix, item.value, adjusted_after_cursor);
        let mut new_lines = lines.to_vec();
        new_lines[cursor_line] = new_line;

        let is_directory = item.label.ends_with('/');
        let has_trailing_quote = item.value.ends_with('"');
        let cursor_offset = if is_directory && has_trailing_quote {
            item.value.len().saturating_sub(1)
        } else {
            item.value.len()
        };

        CompletionResult {
            lines: new_lines,
            cursor_line,
            cursor_col: before_prefix.len() + cursor_offset,
        }
    }
}

#[derive(Debug, Clone)]
struct CommandInfo {
    name: String,
    label: String,
    description: Option<String>,
}

#[derive(Debug, Clone)]
struct DirectoryEntry {
    path: String,
    is_directory: bool,
}

#[derive(Debug, Clone)]
struct FuzzyEntry {
    path: String,
    is_directory: bool,
    score: i32,
}

fn walk_directory_with_fd(
    base_dir: &Path,
    fd_path: &Path,
    query: &str,
    max_results: usize,
    signal: Option<AbortSignal>,
    mut on_entry: Option<impl FnMut(DirectoryEntry)>,
) -> Vec<DirectoryEntry> {
    if let Some(sig) = &signal {
        if sig.is_aborted() {
            return Vec::new();
        }
    }

    let mut args = vec![
        "--base-directory".to_string(),
        base_dir.to_string_lossy().to_string(),
        "--max-results".to_string(),
        max_results.to_string(),
        "--type".to_string(),
        "f".to_string(),
        "--type".to_string(),
        "d".to_string(),
        "--full-path".to_string(),
        "--hidden".to_string(),
        "--exclude".to_string(),
        ".git".to_string(),
        "--exclude".to_string(),
        ".git/*".to_string(),
        "--exclude".to_string(),
        ".git/**".to_string(),
    ];

    if !query.is_empty() {
        args.push(query.to_string());
    }

    let mut child = match Command::new(fd_path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Vec::new(),
    };

    let pid = child.id() as i32;

    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Vec::new();
        }
    };

    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Vec::new();
        }
    };

    let stderr_overflow = Arc::new(AtomicBool::new(false));
    let stderr_overflow_flag = stderr_overflow.clone();
    let stderr_handle = thread::spawn(move || {
        let mut reader = stderr;
        let mut buffer = [0u8; 4096];
        let mut total_bytes = 0usize;
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    total_bytes += n;
                    if total_bytes > FD_MAX_BUFFER {
                        stderr_overflow_flag.store(true, Ordering::SeqCst);
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(pid, libc::SIGTERM);
                        }
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut reader = std::io::BufReader::new(stdout);
    let mut results = Vec::new();
    let mut total_bytes = 0usize;
    let mut line = String::new();
    let mut invalid = false;
    let mut killed_for_limit = false;

    loop {
        if stderr_overflow.load(Ordering::SeqCst) {
            invalid = true;
            let _ = child.kill();
            break;
        }
        line.clear();
        let bytes_read = match std::io::BufRead::read_line(&mut reader, &mut line) {
            Ok(bytes) => bytes,
            Err(_) => {
                invalid = true;
                break;
            }
        };
        if bytes_read == 0 {
            break;
        }
        total_bytes += bytes_read;
        if total_bytes > FD_MAX_BUFFER {
            let _ = child.kill();
            invalid = true;
            break;
        }

        if let Some(sig) = &signal {
            if sig.is_aborted() {
                let _ = child.kill();
                invalid = true;
                break;
            }
        }

        let trimmed = line.trim_end_matches(['\n', '\r'].as_ref());
        if trimmed.is_empty() {
            continue;
        }
        let is_directory = trimmed.ends_with('/');
        let entry = DirectoryEntry {
            path: trimmed.to_string(),
            is_directory,
        };
        results.push(entry.clone());
        if let Some(callback) = on_entry.as_mut() {
            callback(entry);
        }
        if results.len() >= max_results {
            let _ = child.kill();
            killed_for_limit = true;
            break;
        }
    }

    let status = child.wait();
    let _ = stderr_handle.join();

    if stderr_overflow.load(Ordering::SeqCst) {
        invalid = true;
    }

    if invalid {
        return Vec::new();
    }

    if !killed_for_limit {
        match status {
            Ok(status) if status.success() => {}
            _ => return Vec::new(),
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;

    #[test]
    fn parse_path_prefix_variants() {
        let parsed = parse_path_prefix("@\"foo");
        assert!(parsed.is_at_prefix);
        assert!(parsed.is_quoted_prefix);
        assert_eq!(parsed.raw_prefix, "foo");

        let parsed = parse_path_prefix("\"bar");
        assert!(!parsed.is_at_prefix);
        assert!(parsed.is_quoted_prefix);
        assert_eq!(parsed.raw_prefix, "bar");

        let parsed = parse_path_prefix("@baz");
        assert!(parsed.is_at_prefix);
        assert!(!parsed.is_quoted_prefix);
        assert_eq!(parsed.raw_prefix, "baz");
    }

    #[test]
    fn build_completion_value_quotes_when_needed() {
        let value = build_completion_value(
            "foo bar",
            &CompletionOptions {
                is_directory: false,
                is_at_prefix: false,
                is_quoted_prefix: false,
            },
        );
        assert_eq!(value, "\"foo bar\"");

        let value = build_completion_value(
            "foo bar",
            &CompletionOptions {
                is_directory: false,
                is_at_prefix: true,
                is_quoted_prefix: false,
            },
        );
        assert_eq!(value, "@\"foo bar\"");
    }

    #[test]
    fn apply_completion_for_slash_command() {
        let provider = CombinedAutocompleteProvider::new(Vec::new(), PathBuf::from("."), None);
        let lines = vec!["/he".to_string()];
        let item = AutocompleteItem {
            value: "help".to_string(),
            label: "help".to_string(),
            description: None,
        };
        let result = provider.apply_completion(&lines, 0, 3, &item, "/he");
        assert_eq!(result.lines[0], "/help ");
        assert_eq!(result.cursor_col, 6);
    }

    #[test]
    fn async_update_and_cancel_flow() {
        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join("fd_mock.sh");
        let script = "#!/bin/sh\necho \"alpha/\"\nsleep 0.1\necho \"beta\"\n";
        std::fs::write(&script_path, script).expect("write fd mock");
        let mut perms = std::fs::metadata(&script_path)
            .expect("metadata")
            .permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).expect("chmod");
        }

        let provider =
            CombinedAutocompleteProvider::new(Vec::new(), PathBuf::from("."), Some(script_path));
        let updates = Arc::new(AtomicUsize::new(0));
        let signal = AbortSignal::new();
        let update_signal = signal.clone();
        let update_count = updates.clone();

        let handle = provider
            .get_suggestions_async(
                vec!["@a".to_string()],
                0,
                2,
                Some(signal.clone()),
                Some(Box::new(move |suggestions| {
                    update_count.fetch_add(1, AtomicOrdering::SeqCst);
                    if !suggestions.items.is_empty() {
                        update_signal.abort();
                    }
                })),
            )
            .expect("expected async handle");

        let result = handle.join().expect("join handle");
        assert!(updates.load(AtomicOrdering::SeqCst) >= 1);
        if let Some(suggestions) = result {
            assert_eq!(suggestions.prefix, "@a");
        }
    }
}
