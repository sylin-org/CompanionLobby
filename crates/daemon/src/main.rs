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

use lobby_daemon::cli;
use lobby_daemon::registry::Registry;

fn main() -> ExitCode {
    cli::main(Registry::product)
}
