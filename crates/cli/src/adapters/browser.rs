//! Browser opening for the companion manager: the platform's own handler, spawned detached
//! with null stdio so the connector never owns or waits on a browser process. The hub
//! reaches it only through a [`PageOpener`]: the binary installs [`system`], and a hub
//! built without one opens nothing, so the test suites never reach a real browser.

use std::process::{Command, Stdio};
use std::sync::Arc;

/// How the hub hands a page to a browser.
pub type PageOpener = Arc<dyn Fn(&str) + Send + Sync>;

/// The platform command that hands a URL to the user's default browser.
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
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    command
}

/// Headless hosts set this to `1` so the binary never opens a browser.
pub const NO_BROWSER_ENV: &str = "COMPANION_LOBBY_NO_BROWSER";

/// Whether a browser spawn is allowed for a given value of [`NO_BROWSER_ENV`].
pub fn spawn_allowed(flag: Option<&str>) -> bool {
    flag != Some("1")
}

/// The platform browser, spawned detached; failures are silent because the companion manager
/// prints its URL too. [`NO_BROWSER_ENV`] is read once, when the process builds its hub.
pub fn system() -> PageOpener {
    if !spawn_allowed(std::env::var(NO_BROWSER_ENV).ok().as_deref()) {
        return silent();
    }
    Arc::new(|url: &str| {
        let _ = browser_command(url).spawn();
    })
}

/// Opens nothing. Every hub starts with it.
pub fn silent() -> PageOpener {
    Arc::new(|_: &str| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_is_the_platform_handler() {
        let url = "http://127.0.0.1:5220/?token=abc123";
        let command = browser_command(url);
        let program = command.get_program().to_string_lossy().to_string();
        let arguments: Vec<String> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().to_string())
            .collect();
        if cfg!(target_os = "windows") {
            assert!(program.ends_with("rundll32.exe"), "program was {program}");
            assert_eq!(arguments, vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()]);
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
        assert!(!spawn_allowed(Some("1")), "the setting value disables spawns");
    }
}
