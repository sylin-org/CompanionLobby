//! Server reference handling. References are qualified, opaque strings returned by a server
//! (`origin::tangentKey`, `origin::tangentKey::topicKey`, `...::postId`); the connector never
//! invents them and never routes a reference to a different origin.

/// Splits a qualified reference into its origin and segments. Returns `None` for anything a
/// prior response from this origin could not have produced.
pub fn split<'a>(origin: &str, reference: &'a str) -> Option<Vec<&'a str>> {
    let prefix = format!("{origin}::");
    if reference.len() > 512 || !reference.starts_with(&prefix) {
        return None;
    }
    let rest = &reference[prefix.len()..];
    if rest.is_empty() {
        return None;
    }
    Some(rest.split("::").collect())
}

/// `origin::tangent` → tangent key.
pub fn tangent_key<'a>(origin: &str, reference: &'a str) -> Option<&'a str> {
    let parts = split(origin, reference)?;
    (parts.len() == 1 && valid_key(parts[0])).then(|| parts[0])
}

/// `origin::tangent::topic` → (tangent key, topic key).
pub fn topic_keys<'a>(origin: &str, reference: &'a str) -> Option<(&'a str, &'a str)> {
    let parts = split(origin, reference)?;
    (parts.len() == 2 && valid_key(parts[0]) && valid_key(parts[1])).then(|| (parts[0], parts[1]))
}

/// `origin::tangent::topic::post` → (tangent, topic, post).
pub fn post_keys<'a>(origin: &str, reference: &'a str) -> Option<(&'a str, &'a str, &'a str)> {
    let parts = split(origin, reference)?;
    (parts.len() == 3 && valid_key(parts[0]) && valid_key(parts[1]) && valid_opaque(parts[2]))
        .then(|| (parts[0], parts[1], parts[2]))
}

/// `origin::tangent::topic::case_{64-lower-hex}` → tangent, Topic and raw case id.
pub fn case_keys<'a>(origin: &str, reference: &'a str) -> Option<(&'a str, &'a str, &'a str)> {
    let parts = split(origin, reference)?;
    let case_id = parts.get(2)?.strip_prefix("case_")?;
    (parts.len() == 3 && valid_key(parts[0]) && valid_key(parts[1]) && case_id.len() == 64
        && case_id.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
        .then(|| (parts[0], parts[1], case_id))
}

/// Segment sanity: bounded, no separators, not a nested qualified reference.
fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.contains("::")
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b':'))
}

fn valid_opaque(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Canonical origin check: HTTPS anywhere, HTTP only on explicit loopback for development.
pub fn acceptable_origin(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    let without_trailing = lower.trim_end_matches('/');
    let (scheme, rest) = without_trailing.split_once("://")?;
    let host_port = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host_port.is_empty() || host_port.contains('@') {
        return None;
    }
    match scheme {
        "https" => Some(without_trailing.to_string()),
        "http" => {
            let host = host_port.split(':').next().unwrap_or("");
            if matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
                Some(without_trailing.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "http://127.0.0.1:5220";

    #[test]
    fn qualified_references_split_strictly() {
        assert_eq!(tangent_key(ORIGIN, &format!("{ORIGIN}::home")), Some("home"));
        assert_eq!(
            topic_keys(ORIGIN, &format!("{ORIGIN}::home::lounge")),
            Some(("home", "lounge"))
        );
        assert!(post_keys(ORIGIN, &format!("{ORIGIN}::home::lounge::m1")).is_some());
        const CASE_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let case = format!("{ORIGIN}::home::lounge::case_{CASE_ID}");
        assert_eq!(case_keys(ORIGIN, &case), Some(("home", "lounge", CASE_ID)));
        assert!(post_keys(ORIGIN, &case).is_none(), "the discriminator cannot be mistaken for a Post id");
        assert!(case_keys(ORIGIN, &format!("{ORIGIN}::home::lounge::case_{}", "A".repeat(64))).is_none());
        assert!(case_keys(ORIGIN, &format!("{ORIGIN}::home::lounge::{}", "a".repeat(64))).is_none());
        // Another origin's reference is unusable here.
        assert!(tangent_key(ORIGIN, "https://elsewhere::home").is_none());
        // Nested or malformed segments are rejected.
        assert!(tangent_key(ORIGIN, &format!("{ORIGIN}::a::b")).is_none());
        assert!(topic_keys(ORIGIN, &format!("{ORIGIN}::only")).is_none());
    }

    #[test]
    fn origins_must_be_https_or_loopback_http() {
        assert_eq!(acceptable_origin("https://tangent.example"), Some("https://tangent.example".into()));
        assert_eq!(acceptable_origin("http://127.0.0.1:5220/"), Some("http://127.0.0.1:5220".into()));
        assert_eq!(acceptable_origin("http://localhost:8080"), Some("http://localhost:8080".into()));
        assert!(acceptable_origin("http://tangent.example").is_none());
        assert!(acceptable_origin("ftp://tangent.example").is_none());
        assert!(acceptable_origin("not a url").is_none());
    }
}
