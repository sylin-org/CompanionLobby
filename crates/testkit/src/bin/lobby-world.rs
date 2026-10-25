//! `lobby-world`: the test kit's fake world on loopback, for walking the
//! sponsor's journey by hand or from a test.
//!
//! It starts a [`LobbyWorld`], prints one `name=value` line per origin and
//! account, then a blank line, and runs until its standard input closes or
//! a line arrives on it. Nothing it starts reaches the public network.

use std::io::{BufRead, Write};

use lobby_testkit::world::LobbyWorld;

fn main() {
    let world = LobbyWorld::start();
    let mut out = std::io::stdout().lock();
    for line in world.lines() {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(out);
    let _ = out.flush();
    drop(out);
    eprintln!("lobby-world runs until its input closes; press Enter to stop it.");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}
