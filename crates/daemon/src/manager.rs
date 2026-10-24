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
//! Links marked `data-link` are routed inside the page. A companion's page
//! lists its accounts with a revoke for each ready one, and offers a sign-in
//! for each provider kind the lobby runs with; a sign-in has a page of its
//! own between the lobby and the provider, which names the companion and
//! the account the sponsor meant, links to the provider's page in a new tab,
//! and follows the sign-in until it is signed in, refused, cancelled or
//! expired. Each companion shows
//! the `mcpServers` block a host configuration needs, naming the companion by
//! its immutable id, with a warning that host configurations naming it by its
//! moniker stop working after a rename.
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
/// script and stylesheet, requests to its own origin, nothing else.
pub const CONTENT_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Whether `path` is a route of the page: the companions, one companion, one
/// of its sign-ins, or the journal.
pub fn is_page_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/companion/") else {
        return matches!(path, "/" | "/journal");
    };
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
