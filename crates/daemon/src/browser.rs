//! `companion-lobby open`.
//!
//! Makes sure the daemon runs, prints the manager page's address, and hands it
//! to the platform's own browser handler, spawned detached with null stdio so
//! this command never owns or waits on a browser. Only `http` and `https`
//! addresses are ever handed over. `LOBBY_NO_BROWSER=1` prints the address and
//! opens nothing.

use std::process::{Command, Stdio};

use crate::{attach, contact, discovery, home, http};

/// Headless setups set this to `1` so nothing ever opens a browser.
pub const NO_BROWSER_ENV: &str = "LOBBY_NO_BROWSER";

/// The platform command that hands `url` to the default browser.
pub fn browser_command(url: &str) -> Command {
    let mut command = if cfg!(target_os = "windows") {
        // rundll32 with FileProtocolHandler resolves the default browser without a shell.
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler").arg(url);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(url);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Whether a browser may open for a given value of [`NO_BROWSER_ENV`].
pub fn spawn_allowed(flag: Option<&str>) -> bool {
    flag != Some("1")
}

/// Whether `url` may be handed to a browser: `http` or `https` only.
pub fn openable(url: &str) -> bool {
    let lowered = url.to_ascii_lowercase();
    lowered.starts_with("http://") || lowered.starts_with("https://")
}

/// Hands `url` to the browser unless it is not openable or browsers are off.
/// Returns whether a browser was started.
pub fn open_url(url: &str) -> bool {
    if !openable(url) || !spawn_allowed(std::env::var(NO_BROWSER_ENV).ok().as_deref()) {
        return false;
    }
    browser_command(url).spawn().is_ok()
}

/// Runs `companion-lobby open`: the daemon, then the page.
pub fn run() -> Result<String, String> {
    let dir = home::resolve().map_err(|problem| problem.to_string())?;
    let port = http::resolve_port()?;
    contact::ensure(&dir, port, attach::STARTUP_BUDGET)?;
    let url = format!("{}/", discovery::manager_origin(port));
    open_url(&url);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_is_the_platform_handler() {
        let url = "http://127.0.0.1:5220/";
        let command = browser_command(url);
        let program = command.get_program().to_string_lossy().to_string();
        let arguments: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().to_string())
            .collect();
        if cfg!(target_os = "windows") {
            assert!(program.ends_with("rundll32.exe"), "program was {program}");
            assert_eq!(
                arguments,
                vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()]
            );
        } else if cfg!(target_os = "macos") {
            assert_eq!(program, "open");
            assert_eq!(arguments, vec![url.to_string()]);
        } else {
            assert_eq!(program, "xdg-open");
            assert_eq!(arguments, vec![url.to_string()]);
        }
    }

    #[test]
    fn the_no_browser_setting_disables_spawning_only_when_set_to_one() {
        assert!(spawn_allowed(None), "unset means spawns are allowed");
        assert!(spawn_allowed(Some("0")), "only the exact value 1 disables");
        assert!(spawn_allowed(Some("")));
        assert!(
            !spawn_allowed(Some("1")),
            "the setting value disables spawns"
        );
    }

    #[test]
    fn only_web_addresses_are_handed_to_a_browser() {
        assert!(openable("http://127.0.0.1:5219/"));
        assert!(openable("HTTPS://auth.example.test/authorize"));
        for refused in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ms-settings:",
            "C:\\x.exe",
            "",
        ] {
            assert!(!openable(refused), "{refused}");
            assert!(!open_url(refused));
        }
    }
}
