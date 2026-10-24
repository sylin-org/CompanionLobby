//! The `companion-lobby-synthetic` command, for tests only.
//!
//! It is `companion-lobby` with the test kit's synthetic adapters in its
//! registry: the passbook credential adapter and the board service adapter,
//! whose words belong to no real provider or service. It is built only with
//! the `synthetic` feature, which only the test builds turn on, and the shim
//! it runs starts its daemon from this same executable.

use std::process::ExitCode;
use std::sync::Arc;

use lobby_daemon::cli;
use lobby_daemon::registry::Registry;
use lobby_testkit::board::Board;
use lobby_testkit::passbook::Passbook;

fn synthetic() -> Registry {
    Registry::new(
        vec![Arc::new(Passbook::new())],
        vec![Arc::new(Board::new())],
    )
}

fn main() -> ExitCode {
    cli::main(synthetic)
}
