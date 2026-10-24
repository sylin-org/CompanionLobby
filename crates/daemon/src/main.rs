//! The `companion-lobby` command.
//!
//! One binary serves every role: `run` is the daemon, `attach <companion>` is
//! the stdio shim an MCP host launches, `open` shows the manager page and
//! `stop` drains the running daemon. Host configurations therefore name one
//! path, and the shim starts the daemon from its own executable.
//!
//! An attached shim's stdout carries only protocol frames; every other message
//! of every subcommand goes to stderr, except the one result line `open` and
//! `stop` print.

use std::process::ExitCode;

use lobby_daemon::cli::{Command, Parsed, USAGE};
use lobby_daemon::{attach, browser, http, stop};

fn main() -> ExitCode {
    match Command::parse(std::env::args().skip(1)) {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Parsed::Command(Command::Run)) => report(http::run().map(|()| None)),
        Ok(Parsed::Command(Command::Attach { companion, log })) => {
            ExitCode::from(attach::run(&companion, log.as_deref()))
        }
        Ok(Parsed::Command(Command::Open)) => report(browser::run().map(Some)),
        Ok(Parsed::Command(Command::Stop)) => report(stop::run().map(Some)),
        Err(problem) => {
            eprintln!("{problem}\n\n{USAGE}");
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
