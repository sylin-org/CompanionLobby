//! The manager page.
//!
//! The page is a shell of three embedded files (`index.html`, `manager.css`,
//! `manager.js`) built on the seven nouns. It holds no truth: it loads the
//! lobby from `GET /api/lobby`, follows `GET /api/events`, and on a `reset`
//! item loads the lobby again, applying only the items that came after the
//! position the load included. Every change it asks for goes through the
//! operator API with the revision it last read.
//!
//! The page writes text only through `textContent` and text nodes, never
//! through markup, so nothing a companion's name holds can become markup. It
//! never opens a dialog: a destructive action asks for confirmation in place.
//! Links marked `data-link` are routed inside the page. What the sponsor
//! types is kept as a draft in the tab's own storage, so a reload loses
//! nothing; everything else it shows comes from the lobby. The routes, the
//! moments they serve and the words they use are described in
//! `docs/manager.md`.
//!
//! This module also writes the live feed's frames.

use bytes::Bytes;
use lobby_contracts::CompanionId;

/// The page itself, served at every route the page knows.
pub const PAGE: (&str, &str) = (
    include_str!("manager/index.html"),
    "text/html; charset=utf-8",
);
/// The stylesheet.
pub const STYLE: (&str, &str) = (
    include_str!("manager/manager.css"),
    "text/css; charset=utf-8",
);
/// The script.
pub const SCRIPT: (&str, &str) = (
    include_str!("manager/manager.js"),
    "text/javascript; charset=utf-8",
);
/// The content security policy of every file the page is made of: its own
/// script and stylesheet, requests to its own origin, nothing else. Images
/// come from its own origin, or from a file the sponsor just chose, shown
/// before it is sent.
pub const CONTENT_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     connect-src 'self'; img-src 'self' blob:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Whether `path` is a route of the page: the companions, the journal, one
/// companion, the page that tells a host how to attach as it, or one of its
/// sign-ins.
pub fn is_page_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/companion/") else {
        return matches!(path, "/" | "/journal");
    };
    if let Some(id) = rest.strip_suffix("/attach") {
        return CompanionId::parse(id).is_some();
    }
    match rest.split_once("/sign-in/") {
        Some((id, sign_in)) => {
            CompanionId::parse(id).is_some()
                && sign_in.len() == 32
                && sign_in.bytes().all(|byte| byte.is_ascii_hexdigit())
        }
        None => CompanionId::parse(rest).is_some(),
    }
}

/// The embedded file served at `path`, with its content type.
pub fn asset(path: &str) -> Option<(&'static str, &'static str)> {
    match path {
        "/manager.css" => Some(STYLE),
        "/manager.js" => Some(SCRIPT),
        _ => None,
    }
}

/// Tells the page's `EventSource` to reconnect two seconds after a drop.
pub fn retry_frame() -> Bytes {
    Bytes::from_static(b"retry: 2000\n\n")
}

/// Tells the page to load the lobby again before it applies anything.
pub fn reset_frame(reason: &str) -> Bytes {
    let data = serde_json::json!({ "kind": "reset", "reason": reason });
    Bytes::from(format!("data: {data}\n\n"))
}

/// One item of the feed. Its text is one line of JSON, so it is one `data:`
/// line.
pub fn item_frame(id: &str, data: &str) -> Bytes {
    Bytes::from(format!("id: {id}\ndata: {data}\n\n"))
}

/// A comment that keeps a quiet stream alive.
pub fn keepalive_frame() -> Bytes {
    Bytes::from_static(b": keepalive\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_opens_no_dialog_and_writes_no_markup() {
        for (name, text) in [("index.html", PAGE.0), ("manager.js", SCRIPT.0)] {
            for forbidden in [
                "alert(",
                "confirm(",
                "prompt(",
                "showModal(",
                "<dialog",
                "\"dialog\"",
                "innerHTML",
                "outerHTML",
                "insertAdjacentHTML",
                "document.write",
                "eval(",
                "new Function",
                "setAttribute(\"on",
            ] {
                assert!(!text.contains(forbidden), "{name} uses {forbidden}");
            }
        }
    }

    /// The page's text as a browser rendered it, walking the journey
    /// against the fake world `lobby-world` serves: the lobby, a companion's
    /// accounts with their details closed and with one opened, and both in
    /// trouble. Each starts with a line naming the SHA-256 of the
    /// manager.js it was recorded against. See docs/manager.md for how they
    /// are recorded again.
    const RENDERED: [(&str, &str); 5] = [
        (
            "lobby",
            include_str!("../../testkit/fixtures/manager/lobby.txt"),
        ),
        (
            "lobby in trouble",
            include_str!("../../testkit/fixtures/manager/lobby-trouble.txt"),
        ),
        (
            "accounts",
            include_str!("../../testkit/fixtures/manager/companion-accounts.txt"),
        ),
        (
            "accounts with details",
            include_str!("../../testkit/fixtures/manager/companion-account-details.txt"),
        ),
        (
            "accounts in trouble",
            include_str!("../../testkit/fixtures/manager/companion-trouble.txt"),
        ),
    ];

    /// What starts each recorded page, before the script's hash.
    const RECORDED_AGAINST: &str = "recorded against manager.js sha256 ";

    /// The hash a recorded page names, and the page's text after it.
    fn recorded(text: &str) -> (&str, &str) {
        let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
        (
            first.strip_prefix(RECORDED_AGAINST).unwrap_or_default(),
            rest,
        )
    }

    #[test]
    fn every_rendered_page_was_recorded_against_this_manager_script() {
        use sha2::{Digest, Sha256};
        let hash: String = Sha256::digest(SCRIPT.0.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        for (page, text) in RENDERED {
            assert_eq!(
                recorded(text).0,
                hash,
                "the {page} fixture was recorded against another manager.js: walk the page again and record it (docs/manager.md)"
            );
        }
    }

    #[test]
    fn a_rendered_page_shows_an_account_by_its_label_and_its_key_only_in_the_details() {
        use lobby_testkit::world::{DID, HANDLE};
        for (page, text) in RENDERED {
            let lines: Vec<&str> = recorded(text).1.lines().collect();
            let keyed: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.contains(DID))
                .map(|(index, _)| index)
                .collect();
            if page == "accounts with details" {
                assert_eq!(keyed.len(), 1, "{page}: the key shows once");
                assert_eq!(
                    lines[keyed[0] - 1],
                    "Account key",
                    "{page}: the key sits under its name, in the opened details"
                );
                assert_eq!(lines[keyed[0]], DID, "{page}: and nothing else on its line");
            } else {
                assert!(
                    keyed.is_empty(),
                    "{page} shows the key where a label belongs"
                );
            }
        }
        let (_, lobby) = RENDERED[0];
        assert!(
            recorded(lobby)
                .1
                .contains(&format!("{HANDLE} · Atmosphere")),
            "the lobby names the account by its label"
        );
    }

    #[test]
    fn what_the_sponsor_reads_after_pulling_a_key_says_what_is_left_and_where() {
        let script = SCRIPT.0;
        // The routine entries keep one control with one name, pressed or not.
        assert!(script.contains("button(\"Show routine entries\", \"quiet\")"));
        assert!(!script.contains("Hide routine entries"));
        // A revoked connection says what the service may still honor, and
        // an account that waited says where to check at the provider.
        assert!(script.contains("may still honor that until it expires there."));
        assert!(script.contains(
            "To be sure, open the account's own settings at \" + title + \", find Companion Lobby among the apps it allows, and remove it there."
        ));
    }

    #[test]
    fn the_page_loads_only_its_own_files() {
        let page = PAGE.0;
        assert!(page.contains("<script src=\"/manager.js\" defer></script>"));
        assert!(page.contains("<link rel=\"stylesheet\" href=\"/manager.css\">"));
        assert_eq!(page.matches("<script").count(), 1, "no inline script");
        assert!(!page.contains("style=\""), "no inline style");
        assert!(!page.contains("http://") && !page.contains("https://"));
    }

    #[test]
    fn the_page_routes_are_known() {
        let id = CompanionId::from_u128(7);
        assert!(is_page_path("/"));
        assert!(is_page_path("/journal"));
        assert!(is_page_path(&format!("/companion/{id}")));
        assert!(is_page_path(&format!("/companion/{id}/attach")));
        assert!(!is_page_path("/companion/ada/attach"));
        assert!(!is_page_path(&format!("/companion/{id}/attach/x")));
        assert!(!is_page_path("/companion/ada"));
        let sign_in = "0123456789abcdef0123456789abcdef";
        assert!(is_page_path(&format!("/companion/{id}/sign-in/{sign_in}")));
        assert!(!is_page_path(&format!("/companion/{id}/sign-in/x")));
        assert!(!is_page_path(&format!("/companion/ada/sign-in/{sign_in}")));
        assert!(!is_page_path(&format!("/companion/{id}/other/{sign_in}")));
        assert!(!is_page_path("/companion/"));
        assert!(!is_page_path("/elsewhere"));
        assert_eq!(asset("/manager.js"), Some(SCRIPT));
        assert_eq!(asset("/manager.css"), Some(STYLE));
        assert_eq!(asset("/index.html"), None);
    }

    #[test]
    fn feed_frames_are_whole_server_sent_events() {
        assert_eq!(
            item_frame("e:3", r#"{"kind":"journal"}"#),
            Bytes::from("id: e:3\ndata: {\"kind\":\"journal\"}\n\n")
        );
        let reset = reset_frame("fresh");
        assert!(reset.starts_with(b"data: {") && reset.ends_with(b"\n\n"));
        assert!(!reset.windows(4).any(|window| window == b"id: "));
    }
}
