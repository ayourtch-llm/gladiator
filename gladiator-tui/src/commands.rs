/// TUI slash commands for the user to type in the input box.
/// These are parsed before sending text to the agent.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiCommand {
    Save(String),
    Load(String),
    Fixme(String),
}

/// Parse a user input string for TUI commands.
/// Returns `Some(TuiCommand)` if the input is a recognized command,
/// `None` otherwise (the input should be sent to the agent as normal text).
pub fn parse_tui_command(text: &str) -> Option<TuiCommand> {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("/save ") {
        let filename = rest.trim();
        if !filename.is_empty() {
            return Some(TuiCommand::Save(filename.to_string()));
        }
    } else if let Some(rest) = trimmed.strip_prefix("/load ") {
        let filename = rest.trim();
        if !filename.is_empty() {
            return Some(TuiCommand::Load(filename.to_string()));
        }
    } else if let Some(rest) = trimmed.strip_prefix("/fixme ") {
        let phrase = rest.trim();
        if !phrase.is_empty() {
            return Some(TuiCommand::Fixme(phrase.to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_save_command() {
        assert_eq!(
            parse_tui_command("/save state.json"),
            Some(TuiCommand::Save("state.json".to_string()))
        );
    }

    #[test]
    fn parse_load_command() {
        assert_eq!(
            parse_tui_command("/load state.json"),
            Some(TuiCommand::Load("state.json".to_string()))
        );
    }

    #[test]
    fn parse_save_with_path() {
        assert_eq!(
            parse_tui_command("/save /tmp/my_state.json"),
            Some(TuiCommand::Save("/tmp/my_state.json".to_string()))
        );
    }

    #[test]
    fn parse_save_no_filename() {
        assert_eq!(parse_tui_command("/save"), None);
        assert_eq!(parse_tui_command("/save "), None);
    }

    #[test]
    fn parse_load_no_filename() {
        assert_eq!(parse_tui_command("/load"), None);
        assert_eq!(parse_tui_command("/load "), None);
    }

    #[test]
    fn parse_non_command() {
        assert_eq!(parse_tui_command("hello world"), None);
        assert_eq!(parse_tui_command("write a function"), None);
    }

    #[test]
    fn parse_not_a_command_starting_with_slash() {
        assert_eq!(parse_tui_command("/help"), None);
        assert_eq!(parse_tui_command("/"), None);
    }

    #[test]
    fn parse_save_with_trailing_whitespace() {
        assert_eq!(
            parse_tui_command("/save state.json   "),
            Some(TuiCommand::Save("state.json".to_string()))
        );
    }

    #[test]
    fn parse_fixme_command() {
        assert_eq!(
            parse_tui_command("/fixme fix the bug in auth"),
            Some(TuiCommand::Fixme("fix the bug in auth".to_string()))
        );
    }

    #[test]
    fn parse_fixme_with_trailing_whitespace() {
        assert_eq!(
            parse_tui_command("/fixme fix the bug   "),
            Some(TuiCommand::Fixme("fix the bug".to_string()))
        );
    }

    #[test]
    fn parse_fixme_no_phrase() {
        assert_eq!(parse_tui_command("/fixme"), None);
        assert_eq!(parse_tui_command("/fixme "), None);
    }
}
