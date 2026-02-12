#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    Help,
    Clear,
    Cancel,
    Quit,
    Unknown(String),
}

pub fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let command = trimmed
        .split_whitespace()
        .next()
        .unwrap_or(trimmed)
        .to_string();

    let parsed = match command.as_str() {
        "/help" => SlashCommand::Help,
        "/clear" => SlashCommand::Clear,
        "/cancel" => SlashCommand::Cancel,
        "/quit" => SlashCommand::Quit,
        _ => SlashCommand::Unknown(command),
    };

    Some(parsed)
}
