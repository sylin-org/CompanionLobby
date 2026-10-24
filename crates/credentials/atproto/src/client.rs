//! The loopback client the lobby signs in as.
//!
//! atproto lets a client on the sponsor's own machine sign in without
//! publishing a client metadata document: its client id is
//! `http://localhost` with a query that declares the redirect URIs it may
//! name and the scope it may ask for, and the authorization server refuses a
//! request outside what the client id declares. The lobby's client id
//! declares exactly one redirect URI, the lobby's own return route on the
//! loopback address, and exactly the scope this sign-in asks for. A refresh
//! of the session must present the same client id, so the material keeps it.

use crate::codec::percent_encode;

/// The client id's origin.
pub const CLIENT_ORIGIN: &str = "http://localhost";

/// The path of the lobby's return route for this adapter.
pub const RETURN_PATH: &str = "/signin/atproto/return";

/// `return_to` when it is the lobby's own return route on the loopback
/// address: `http://127.0.0.1:<port>/signin/atproto/return`, or the same on
/// `[::1]`. A name such as `localhost` is not a loopback address.
pub fn return_route(return_to: &str) -> Option<&str> {
    let rest = return_to.strip_prefix("http://")?;
    let (authority, path) = rest.split_once('/')?;
    if format!("/{path}") != RETURN_PATH {
        return None;
    }
    let port = authority
        .strip_prefix("127.0.0.1:")
        .or_else(|| authority.strip_prefix("[::1]:"))?;
    let valid_port = port
        .parse::<u16>()
        .is_ok_and(|number| number != 0 && number.to_string() == port);
    valid_port.then_some(return_to)
}

/// The client id that declares `redirect_uri` and `scope`.
pub fn client_id(redirect_uri: &str, scope: &str) -> String {
    format!(
        "{CLIENT_ORIGIN}?redirect_uri={}&scope={}",
        percent_encode(redirect_uri),
        percent_encode(scope)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_return_route_is_the_lobbys_own_on_the_loopback_address() {
        for good in [
            "http://127.0.0.1:5219/signin/atproto/return",
            "http://[::1]:5219/signin/atproto/return",
        ] {
            assert_eq!(return_route(good), Some(good));
        }
        for bad in [
            "http://localhost:5219/signin/atproto/return",
            "https://127.0.0.1:5219/signin/atproto/return",
            "http://127.0.0.1/signin/atproto/return",
            "http://127.0.0.1:05219/signin/atproto/return",
            "http://127.0.0.1:0/signin/atproto/return",
            "http://127.0.0.1:5219/signin/other/return",
            "http://127.0.0.1:5219/signin/atproto/return?x=1",
            "http://127.0.0.1:5219/",
            "http://10.0.0.1:5219/signin/atproto/return",
        ] {
            assert_eq!(return_route(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_client_id_declares_its_one_redirect_and_its_scope() {
        assert_eq!(
            client_id("http://127.0.0.1:5219/signin/atproto/return", "atproto"),
            "http://localhost?redirect_uri=http%3A%2F%2F127.0.0.1%3A5219%2Fsignin%2Fatproto%2Freturn&scope=atproto"
        );
    }
}
