//! One connection's cookies.
//!
//! A Space signs an account in with a cookie, and its forms pair their
//! tokens with another, so a connection holds what the Space set for it and
//! nothing any other connection holds: the jar lives in what the lobby keeps
//! for that one connection (see [`crate::issued`]). A jar only ever goes to
//! its connection's one origin, so it reads `Set-Cookie` as an agent of that
//! origin alone: it keeps a cookie's name, value, path, expiry and whether it
//! is `Secure`, and ignores `Domain`, `SameSite` and `HttpOnly`, which say how
//! far a browser may carry one. Its values are never printed.

use serde_json::{Value, json};

use crate::issued::parse_rfc3339;

/// The most cookies one jar holds; a Space that sets more loses its oldest.
pub const MOST: usize = 32;
/// The longest cookie kept, name and value together, in bytes.
pub const COOKIE_LIMIT: usize = 16 * 1024;

#[derive(Clone, PartialEq, Eq)]
struct Cookie {
    name: String,
    value: String,
    path: String,
    expires: Option<i64>,
    secure: bool,
}

/// The cookies one connection holds.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Jar(Vec<Cookie>);

impl std::fmt::Debug for Jar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|cookie| &cookie.name))
            .finish()
    }
}

/// The directory of `path`, RFC 6265's default for a cookie that names none.
fn directory_of(path: &str) -> &str {
    let path = path.split('?').next().unwrap_or_default();
    match path.rfind('/') {
        None | Some(0) => "/",
        Some(last) => &path[..last],
    }
}

/// Whether a cookie scoped to `scope` goes with a request for `path`.
fn path_matches(scope: &str, path: &str) -> bool {
    let path = path.split('?').next().unwrap_or_default();
    path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|rest| scope.ends_with('/') || rest.starts_with('/'))
}

/// A cookie's `Expires`, as the seconds since the Unix epoch: the date form
/// HTTP writes, with its parts apart by spaces or hyphens.
fn parse_http_date(text: &str) -> Option<i64> {
    let (_, rest) = text.split_once(',')?;
    let parts: Vec<&str> = rest
        .split([' ', '-'])
        .filter(|part| !part.is_empty())
        .collect();
    let [day, month, year, time, ..] = parts[..] else {
        return None;
    };
    let month = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|name| name.eq_ignore_ascii_case(month))?
        + 1;
    let year: i64 = year.parse().ok()?;
    let year = if year < 100 { year + 2000 } else { year };
    let day: i64 = day.parse().ok()?;
    parse_rfc3339(&format!("{year:04}-{month:02}-{day:02}T{time}Z"))
}

impl Jar {
    /// Whether it holds nothing.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Takes one `Set-Cookie` value, answered to a request for `path`.
    /// `https` says whether that request went over HTTPS: a `Secure` cookie
    /// set over plain HTTP is not taken. A cookie that has already expired
    /// takes out the one it names; one that cannot be read changes nothing.
    pub fn absorb(&mut self, set_cookie: &str, path: &str, https: bool, now: i64) {
        let mut attributes = set_cookie.split(';');
        let Some((name, value)) = attributes.next().and_then(|pair| pair.split_once('=')) else {
            return;
        };
        let (name, value) = (name.trim(), value.trim());
        let shaped = !name.is_empty()
            && name.len() + value.len() <= COOKIE_LIMIT
            && name
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b',' | b';' | b'='))
            && value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b',' | b';'));
        if !shaped {
            return;
        }
        let mut scope = None;
        let mut expires = None;
        let mut lives_for = None;
        let mut secure = false;
        for attribute in attributes {
            let (key, content) = attribute.split_once('=').unwrap_or((attribute, ""));
            let content = content.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "path" if content.starts_with('/') => scope = Some(content.to_owned()),
                "expires" => expires = parse_http_date(content),
                "max-age" => lives_for = content.parse::<i64>().ok(),
                "secure" => secure = true,
                _ => {}
            }
        }
        if secure && !https {
            return;
        }
        let path = scope.unwrap_or_else(|| directory_of(path).to_owned());
        // Max-Age outranks Expires.
        let expires = lives_for.map(|secs| now.saturating_add(secs)).or(expires);
        self.0.retain(|held| held.name != name || held.path != path);
        if expires.is_some_and(|at| at <= now) {
            return;
        }
        if self.0.len() == MOST {
            self.0.remove(0);
        }
        self.0.push(Cookie {
            name: name.to_owned(),
            value: value.to_owned(),
            path,
            expires,
            secure,
        });
    }

    /// The `Cookie` header for a request for `path` at `now`, when any cookie
    /// goes with it: those scoped most narrowly first.
    pub fn header(&self, path: &str, https: bool, now: i64) -> Option<String> {
        let mut going: Vec<&Cookie> = self
            .0
            .iter()
            .filter(|cookie| {
                (https || !cookie.secure)
                    && cookie.expires.is_none_or(|at| at > now)
                    && path_matches(&cookie.path, path)
            })
            .collect();
        going.sort_by_key(|cookie| std::cmp::Reverse(cookie.path.len()));
        (!going.is_empty()).then(|| {
            going
                .iter()
                .map(|cookie| format!("{}={}", cookie.name, cookie.value))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    /// The form the lobby keeps.
    pub fn encode(&self) -> Value {
        Value::Array(
            self.0
                .iter()
                .map(|cookie| {
                    json!({
                        "name": cookie.name,
                        "value": cookie.value,
                        "path": cookie.path,
                        "expires": cookie.expires,
                        "secure": cookie.secure,
                    })
                })
                .collect(),
        )
    }

    /// The jar kept in `kept`, with every cookie in its own shape.
    pub fn decode(kept: &Value) -> Option<Self> {
        kept.as_array()?
            .iter()
            .map(|cookie| {
                let text = |field: &str| cookie[field].as_str().map(str::to_owned);
                let expires = match &cookie["expires"] {
                    Value::Null => None,
                    at => Some(at.as_i64()?),
                };
                Some(Cookie {
                    name: text("name")?,
                    value: text("value")?,
                    path: text("path")?,
                    expires,
                    secure: cookie["secure"].as_bool()?,
                })
            })
            .collect::<Option<Vec<_>>>()
            .map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_790_000_000;

    fn taken(lines: &[&str]) -> Jar {
        let mut jar = Jar::default();
        for line in lines {
            jar.absorb(line, "/api/v1/identity/exchange", false, NOW);
        }
        jar
    }

    #[test]
    fn a_jar_sends_what_the_space_set_as_the_space_scoped_it() {
        let jar = taken(&[
            ".AspNetCore.Koan.cookie=abc; path=/; samesite=lax; httponly",
            "narrow=n; path=/t/home",
            "here=h",
        ]);
        assert_eq!(
            jar.header("/", false, NOW).as_deref(),
            Some(".AspNetCore.Koan.cookie=abc")
        );
        assert_eq!(
            jar.header("/t/home/welcome?before=4", false, NOW)
                .as_deref(),
            Some("narrow=n; .AspNetCore.Koan.cookie=abc")
        );
        assert_eq!(
            jar.header("/t/homely", false, NOW).as_deref(),
            Some(".AspNetCore.Koan.cookie=abc"),
            "a path scope stops at a segment"
        );
        assert_eq!(
            jar.header("/api/v1/identity/other", false, NOW).as_deref(),
            Some("here=h; .AspNetCore.Koan.cookie=abc"),
            "a cookie that names no path is scoped to the directory it came from"
        );
    }

    #[test]
    fn a_cookie_is_replaced_by_its_name_and_path_and_taken_out_by_its_expiry() {
        let mut jar = taken(&["a=1; path=/", "b=2; path=/"]);
        jar.absorb("a=3; path=/", "/", false, NOW);
        assert_eq!(jar.header("/", false, NOW).as_deref(), Some("b=2; a=3"));
        jar.absorb(
            "a=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/",
            "/",
            false,
            NOW,
        );
        assert_eq!(jar.header("/", false, NOW).as_deref(), Some("b=2"));
        jar.absorb("b=; max-age=0; path=/", "/", false, NOW);
        assert!(jar.is_empty());

        let brief = taken(&["c=1; max-age=60; path=/"]);
        assert!(brief.header("/", false, NOW + 59).is_some());
        assert_eq!(brief.header("/", false, NOW + 60), None, "it has ended");
        let dated = taken(&["d=1; expires=Wed, 21-Oct-2026 14:13:20 GMT; path=/"]);
        assert!(dated.header("/", false, NOW).is_some());
        assert_eq!(dated.header("/", false, 1_792_592_000), None);
    }

    #[test]
    fn a_secure_cookie_travels_only_over_https_and_is_only_taken_from_it() {
        let mut jar = Jar::default();
        jar.absorb("s=1; secure; path=/", "/", false, NOW);
        assert!(jar.is_empty());
        jar.absorb("s=1; secure; path=/", "/", true, NOW);
        assert_eq!(jar.header("/", true, NOW).as_deref(), Some("s=1"));
        assert_eq!(jar.header("/", false, NOW), None);
    }

    #[test]
    fn what_cannot_be_a_cookie_changes_nothing_and_a_jar_is_bounded() {
        let mut jar = taken(&["a=1; path=/"]);
        for bad in ["", "no value", "=x", "bad name=1", "a=two words"] {
            jar.absorb(bad, "/", false, NOW);
        }
        jar.absorb(
            &format!("big={}", "x".repeat(COOKIE_LIMIT)),
            "/",
            false,
            NOW,
        );
        assert_eq!(jar.header("/", false, NOW).as_deref(), Some("a=1"));
        for index in 0..MOST {
            jar.absorb(&format!("c{index}=1; path=/"), "/", false, NOW);
        }
        let held = jar.header("/", false, NOW).expect("cookies");
        assert_eq!(held.split("; ").count(), MOST);
        assert!(!held.contains("a=1"), "the oldest went first");
    }

    #[test]
    fn a_jar_round_trips_and_never_prints_a_value() {
        let jar = taken(&["a=secretvalue; max-age=60; path=/", "s=other; path=/x"]);
        assert_eq!(Jar::decode(&jar.encode()).as_ref(), Some(&jar));
        let printed = format!("{jar:?}");
        assert!(printed.contains("\"a\"") && !printed.contains("secretvalue"));
        assert_eq!(Jar::decode(&json!([{ "name": "a" }])), None);
        assert_eq!(Jar::decode(&json!("a=1")), None);
    }
}
