//! The `companion-lobby` command.
//!
//! One binary serves every role: `run` is the daemon, `attach <companion>` is
//! the stdio shim an MCP host launches, `open` shows the manager page and
//! `stop` drains the running daemon. Host configurations therefore name one
//! path, and the shim starts the daemon from its own executable.

use std::process::ExitCode;

use lobby_daemon::cli::{Command, Parsed, USAGE};

fn main() -> ExitCode {
    match Command::parse(std::env::args().skip(1)) {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Parsed::Command(command @ Command::Attach { .. })) => {
            // An attached shim's stdout carries only protocol frames.
            eprintln!("{}", command.plan());
            ExitCode::SUCCESS
        }
        Ok(Parsed::Command(command)) => {
            println!("{}", command.plan());
            ExitCode::SUCCESS
        }
        Err(problem) => {
            eprintln!("{problem}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
