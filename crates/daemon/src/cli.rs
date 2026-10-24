//! The command line of `companion-lobby`.
//!
//! Subcommands: `run` (the default), `attach <companion>` where the companion
//! is its id or its moniker, `open` and `stop`. Each subcommand describes what
//! it will do; the daemon, the shim, the opener and the drain arrive in later
//! slices.

use std::fmt;

/// The usage text printed for `help` and after a usage problem.
pub const USAGE: &str = "\
usage: companion-lobby [run]
       companion-lobby attach <companion>
       companion-lobby open
       companion-lobby stop";

/// A subcommand of `companion-lobby`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Run the daemon in the foreground.
    Run,
    /// Relay an MCP host's stdio to the daemon as one companion.
    Attach {
        /// The companion's id or moniker, as the host configuration names it.
        companion: String,
    },
    /// Open the manager page in the default browser.
    Open,
    /// Ask the running daemon to drain and exit.
    Stop,
}

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// Print the usage text.
    Help,
    /// Perform a subcommand.
    Command(Command),
}

/// A command line that does not match the usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageError {
    /// The first argument is not a subcommand.
    UnknownSubcommand(String),
    /// `attach` was given no companion.
    MissingCompanion,
    /// A subcommand was given an argument it does not take.
    UnexpectedArgument(String),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSubcommand(word) => write!(f, "unknown subcommand: {word}"),
            Self::MissingCompanion => f.write_str("attach needs a companion id or moniker"),
            Self::UnexpectedArgument(word) => write!(f, "unexpected argument: {word}"),
        }
    }
}

impl std::error::Error for UsageError {}

impl Command {
    /// Parses the arguments that follow the program name.
    pub fn parse<I>(arguments: I) -> Result<Parsed, UsageError>
    where
        I: IntoIterator<Item = String>,
    {
        let mut arguments = arguments.into_iter();
        let command = match arguments.next().as_deref() {
            None | Some("run") => Command::Run,
            Some("open") => Command::Open,
            Some("stop") => Command::Stop,
            Some("attach") => match arguments.next() {
                Some(companion) if !companion.is_empty() => Command::Attach { companion },
                _ => return Err(UsageError::MissingCompanion),
            },
            Some("help" | "-h" | "--help") => return Ok(Parsed::Help),
            Some(other) => return Err(UsageError::UnknownSubcommand(other.to_owned())),
        };
        match arguments.next() {
            Some(extra) => Err(UsageError::UnexpectedArgument(extra)),
            None => Ok(Parsed::Command(command)),
        }
    }

    /// One sentence saying what this subcommand will do once its slice lands.
    pub fn plan(&self) -> String {
        match self {
            Self::Run => "run: will hold daemon.lock in the data directory and serve the manager \
                          page and the MCP endpoint on 127.0.0.1; this build serves nothing."
                .to_owned(),
            Self::Attach { companion } => format!(
                "attach {companion}: will relay this host's MCP frames to the daemon as \
                 companion {companion}, starting the daemon if needed; this build relays nothing."
            ),
            Self::Open => "open: will open the manager page in the default browser; this build \
                           opens nothing."
                .to_owned(),
            Self::Stop => "stop: will ask the running daemon to drain and exit; this build stops \
                           nothing."
                .to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(words: &[&str]) -> Result<Parsed, UsageError> {
        Command::parse(words.iter().map(|word| (*word).to_owned()))
    }

    #[test]
    fn no_subcommand_means_run() {
        assert_eq!(parse(&[]), Ok(Parsed::Command(Command::Run)));
    }

    #[test]
    fn attach_names_one_companion() {
        assert_eq!(
            parse(&["attach", "ada"]),
            Ok(Parsed::Command(Command::Attach {
                companion: "ada".to_owned()
            }))
        );
        assert_eq!(parse(&["attach"]), Err(UsageError::MissingCompanion));
        assert_eq!(
            parse(&["attach", "ada", "bea"]),
            Err(UsageError::UnexpectedArgument("bea".to_owned()))
        );
    }

    #[test]
    fn open_and_stop_take_no_arguments() {
        assert_eq!(parse(&["open"]), Ok(Parsed::Command(Command::Open)));
        assert_eq!(parse(&["stop"]), Ok(Parsed::Command(Command::Stop)));
        assert_eq!(
            parse(&["stop", "now"]),
            Err(UsageError::UnexpectedArgument("now".to_owned()))
        );
    }

    #[test]
    fn an_unknown_subcommand_is_a_usage_problem() {
        assert_eq!(
            parse(&["serve"]),
            Err(UsageError::UnknownSubcommand("serve".to_owned()))
        );
    }
}
