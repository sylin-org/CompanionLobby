//! The command line of `companion-lobby`.
//!
//! Subcommands: `run` (the default), `attach <companion> [--log <file>]`
//! where the companion is its id or its moniker, `open` and `stop`.
//!
//! [`main`] is the whole command; a binary only says which adapters its
//! daemon runs with.

use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::registry::Registry;
use crate::{attach, browser, http, stop};

/// Runs the command named by this process's arguments. `registry` composes
/// the adapters, and is called only when the command runs the daemon.
pub fn main(registry: fn() -> Registry) -> ExitCode {
    match Command::parse(std::env::args().skip(1)) {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Parsed::Command(Command::Run)) => report(http::run(registry()).map(|()| None)),
        Ok(Parsed::Command(Command::Attach { companion, log })) => {
            ExitCode::from(attach::run(&companion, log.as_deref()))
        }
        Ok(Parsed::Command(Command::Open)) => report(browser::run().map(Some)),
        Ok(Parsed::Command(Command::Stop)) => report(stop::run().map(Some)),
        Err(problem) => {
            eprintln!(
                "{problem}

{USAGE}"
            );
            ExitCode::from(2)
        }
    }
}

fn report(result: Result<Option<String>, String>) -> ExitCode {
    match result {
        Ok(Some(line)) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("companion-lobby: {problem}");
            ExitCode::FAILURE
        }
    }
}

/// The usage text printed for `help` and after a usage problem.
pub const USAGE: &str = "\
usage: companion-lobby [run]
       companion-lobby attach <companion> [--log <file>]
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
        /// Where to append a record of every relayed frame, if anywhere.
        log: Option<PathBuf>,
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
    /// `--log` was given no file.
    MissingLogFile,
    /// A subcommand was given an argument it does not take.
    UnexpectedArgument(String),
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSubcommand(word) => write!(f, "unknown subcommand: {word}"),
            Self::MissingCompanion => f.write_str("attach needs a companion id or moniker"),
            Self::MissingLogFile => f.write_str("--log needs a file"),
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
            Some("attach") => {
                let companion = match arguments.next() {
                    Some(companion) if !companion.is_empty() && !companion.starts_with('-') => {
                        companion
                    }
                    _ => return Err(UsageError::MissingCompanion),
                };
                let log = match arguments.next().as_deref() {
                    None => None,
                    Some("--log") => match arguments.next() {
                        Some(file) if !file.is_empty() => Some(PathBuf::from(file)),
                        _ => return Err(UsageError::MissingLogFile),
                    },
                    Some(other) => return Err(UsageError::UnexpectedArgument(other.to_owned())),
                };
                Command::Attach { companion, log }
            }
            Some("help" | "-h" | "--help") => return Ok(Parsed::Help),
            Some(other) => return Err(UsageError::UnknownSubcommand(other.to_owned())),
        };
        match arguments.next() {
            Some(extra) => Err(UsageError::UnexpectedArgument(extra)),
            None => Ok(Parsed::Command(command)),
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
                companion: "ada".to_owned(),
                log: None,
            }))
        );
        assert_eq!(
            parse(&["attach", "ada", "--log", "frames.jsonl"]),
            Ok(Parsed::Command(Command::Attach {
                companion: "ada".to_owned(),
                log: Some(PathBuf::from("frames.jsonl")),
            }))
        );
        assert_eq!(
            parse(&["attach", "ada", "--log"]),
            Err(UsageError::MissingLogFile)
        );
        assert_eq!(
            parse(&["attach", "--log"]),
            Err(UsageError::MissingCompanion)
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
