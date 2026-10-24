//! Service addresses.
//!
//! An address is one origin: a scheme, a host and a port, and nothing else.
//! It is parsed by [`Address::parse`], the one origin parser in the
//! workspace: the scheme and host are lowercased, a default port is dropped,
//! an IPv6 literal must be bracketed and written in its canonical form, and
//! user info, a path other than `/`, a query or a fragment are refused. The
//! [`AddressPolicy`] decides which targets an address may name, and the same
//! policy is applied again to every address a name resolves to before a
//! connection is made.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A normalized service origin, such as `https://board.test` or
/// `http://127.0.0.1:5300`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address {
    origin: String,
    host: String,
    port: u16,
    secure: bool,
}

/// Which targets an address may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressPolicy {
    /// Only public HTTPS origins.
    PublicHttps,
    /// Public HTTPS origins, and plain HTTP to the loopback address, for test
    /// fixtures and services that run on the same machine.
    LoopbackAllowed,
}

impl AddressPolicy {
    /// Whether this policy lets a connection reach `ip`: a public address
    /// always, the loopback address only under [`Self::LoopbackAllowed`], and
    /// never a private, link-local, shared, documentation, multicast or
    /// otherwise reserved one.
    pub fn permits(self, ip: IpAddr) -> bool {
        if ip.is_loopback() {
            return self == Self::LoopbackAllowed;
        }
        match ip {
            IpAddr::V4(ip) => public_ipv4(ip),
            IpAddr::V6(ip) => public_ipv6(ip),
        }
    }
}

impl Address {
    /// The longest address text accepted, in bytes.
    pub const MAX_BYTES: usize = 2048;

    /// Parses `text` as an origin `policy` accepts.
    pub fn parse(text: &str, policy: AddressPolicy) -> Option<Self> {
        if text.is_empty()
            || text.len() > Self::MAX_BYTES
            || text != text.trim()
            || text
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b'\\' || !byte.is_ascii())
        {
            return None;
        }
        let lowered = text.to_ascii_lowercase();
        let bare = lowered.strip_suffix('/').unwrap_or(&lowered);
        let (scheme, authority) = bare.split_once("://")?;
        let secure = match scheme {
            "https" => true,
            "http" => false,
            _ => return None,
        };
        if authority.is_empty() || authority.contains(['/', '?', '#', '@']) {
            return None;
        }
        let (host, port_text, literal) = if let Some(bracketed) = authority.strip_prefix('[') {
            let (inside, tail) = bracketed.split_once(']')?;
            let ip = inside.parse::<Ipv6Addr>().ok()?;
            if inside != ip.to_string() {
                return None;
            }
            let port = if tail.is_empty() {
                None
            } else {
                Some(tail.strip_prefix(':')?)
            };
            (format!("[{inside}]"), port, Some(IpAddr::V6(ip)))
        } else {
            let mut pieces = authority.split(':');
            let host = pieces.next()?;
            let port = pieces.next();
            if pieces.next().is_some() || host.is_empty() {
                return None;
            }
            let ip = host.parse::<Ipv4Addr>().ok();
            if ip.is_some_and(|ip| host != ip.to_string()) {
                return None;
            }
            if ip.is_none() && !valid_dns_name(host) {
                return None;
            }
            (host.to_owned(), port, ip.map(IpAddr::V4))
        };
        let default_port = if secure { 443 } else { 80 };
        let port = match port_text {
            None => default_port,
            Some(digits) => {
                let port = digits.parse::<u16>().ok()?;
                if digits.is_empty()
                    || !digits.bytes().all(|byte| byte.is_ascii_digit())
                    || digits != port.to_string()
                    || port == 0
                {
                    return None;
                }
                port
            }
        };
        let named_loopback = host == "localhost";
        let loopback = named_loopback || literal.is_some_and(|ip| ip.is_loopback());
        if loopback && policy != AddressPolicy::LoopbackAllowed {
            return None;
        }
        // Plain HTTP only ever reaches this machine.
        if !secure && !loopback {
            return None;
        }
        if literal.is_some_and(|ip| !policy.permits(ip)) {
            return None;
        }
        let origin = if port == default_port {
            format!("{scheme}://{host}")
        } else {
            format!("{scheme}://{host}:{port}")
        };
        Some(Self {
            origin,
            host,
            port,
            secure,
        })
    }

    /// The origin as text, with no trailing slash.
    pub fn as_str(&self) -> &str {
        &self.origin
    }

    /// The host: a DNS name, an IPv4 literal or a bracketed IPv6 literal.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port, the scheme's default when the origin names none.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Whether the origin is HTTPS.
    pub const fn is_https(&self) -> bool {
        self.secure
    }

    /// The URL of `path` at this origin; `path` starts with `/`.
    pub fn url(&self, path: &str) -> String {
        debug_assert!(path.starts_with('/'), "{path}");
        format!("{}{path}", self.origin)
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.origin)
    }
}

/// A DNS name of lowercase labels. A name whose last label is all digits is
/// refused: it is no name, and resolvers read some such text as an address.
fn valid_dns_name(host: &str) -> bool {
    host.len() <= 253
        && !host
            .rsplit('.')
            .next()
            .is_some_and(|last| last.bytes().all(|byte| byte.is_ascii_digit()))
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [first, second, third, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || first == 0
        || first == 127
        || (first == 100 && (64..=127).contains(&second))
        || (first == 192 && second == 0 && third == 0)
        || (first == 192 && second == 0 && third == 2)
        || (first == 192 && second == 88 && third == 99)
        || (first == 198 && matches!(second, 18 | 19))
        || (first == 198 && second == 51 && third == 100)
        || (first == 203 && second == 0 && third == 113)
        || first >= 240)
}

fn public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return public_ipv4(mapped);
    }
    let segments = ip.segments();
    !(ip.is_unspecified()
        || ip.is_multicast()
        || (segments[0] & 0xe000) != 0x2000
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || (segments[0] == 0x0064 && segments[1] == 0xff9b)
        || (segments[0] == 0x0100 && segments[1..4] == [0, 0, 0])
        || (segments[0] == 0x2001 && segments[1] <= 0x01ff)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || segments[0] == 0x2002
        || (segments[0] == 0x3fff && (segments[1] & 0xf000) == 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOOPBACK: AddressPolicy = AddressPolicy::LoopbackAllowed;
    const PUBLIC: AddressPolicy = AddressPolicy::PublicHttps;

    fn origin(text: &str, policy: AddressPolicy) -> Option<String> {
        Address::parse(text, policy).map(|address| address.as_str().to_owned())
    }

    #[test]
    fn origins_are_normalized() {
        for (text, expected) in [
            ("https://board.test", "https://board.test"),
            ("https://board.test/", "https://board.test"),
            ("HTTPS://Board.TEST", "https://board.test"),
            ("https://board.test:443", "https://board.test"),
            ("https://board.test:8443", "https://board.test:8443"),
            ("https://[2606:4700::1]", "https://[2606:4700::1]"),
            ("https://8.8.8.8:444", "https://8.8.8.8:444"),
        ] {
            assert_eq!(origin(text, PUBLIC).as_deref(), Some(expected), "{text}");
        }
        let address = Address::parse("http://127.0.0.1:5300/", LOOPBACK).expect("loopback");
        assert_eq!(address.as_str(), "http://127.0.0.1:5300");
        assert_eq!(address.host(), "127.0.0.1");
        assert_eq!(address.port(), 5300);
        assert!(!address.is_https());
        assert_eq!(address.url("/posts"), "http://127.0.0.1:5300/posts");
        assert_eq!(
            Address::parse("https://board.test", PUBLIC)
                .expect("public")
                .port(),
            443
        );
    }

    #[test]
    fn anything_but_one_origin_is_refused() {
        for text in [
            "",
            " https://board.test",
            "https://board.test/path",
            "https://board.test?x=1",
            "https://board.test#top",
            "https://user@board.test",
            "https://board.test:0",
            "https://board.test:0443",
            "https://board.test:99999",
            "https://board.test:",
            "https://board..test",
            "https://-board.test",
            "https://bo_ard.test",
            "https://bóard.test",
            "ftp://board.test",
            "board.test",
            "https://",
            "https://[::ffff:8.8.8.8:1]",
            "https://[2606:4700:0::1]",
            "https://010.8.8.8",
            "https://board.test\\x",
        ] {
            assert_eq!(origin(text, LOOPBACK), None, "{text:?}");
        }
    }

    #[test]
    fn plain_http_reaches_only_this_machine_and_only_when_the_policy_allows() {
        for text in [
            "http://127.0.0.1:5300",
            "http://localhost:5300",
            "http://[::1]:5300",
        ] {
            assert!(origin(text, LOOPBACK).is_some(), "{text}");
            assert_eq!(origin(text, PUBLIC), None, "{text}");
        }
        assert_eq!(origin("http://board.test", LOOPBACK), None);
        assert_eq!(origin("http://8.8.8.8", LOOPBACK), None);
        assert_eq!(origin("https://127.0.0.1", PUBLIC), None);
    }

    #[test]
    fn private_and_reserved_targets_are_refused_under_every_policy() {
        for text in [
            "https://10.0.0.1",
            "https://192.168.1.1",
            "https://172.16.0.1",
            "https://169.254.169.254",
            "https://100.64.0.1",
            "https://0.0.0.0",
            "https://192.0.2.1",
            "https://[fd00::1]",
            "https://[fe80::1]",
            "https://[::ffff:10.0.0.1]",
            "https://[2001:db8::1]",
        ] {
            assert_eq!(origin(text, LOOPBACK), None, "{text}");
            assert_eq!(origin(text, PUBLIC), None, "{text}");
        }
        assert!(LOOPBACK.permits(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!PUBLIC.permits(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(PUBLIC.permits(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!LOOPBACK.permits(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))));
    }
}
