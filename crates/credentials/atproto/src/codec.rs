//! The encodings the OAuth profile speaks, bounded and strict.

use lobby_contracts::Outcome;
use sha2::{Digest, Sha256};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Unpadded base64url (RFC 4648, section 5).
pub fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = chunk
            .iter()
            .enumerate()
            .fold(0u32, |triple, (index, byte)| {
                triple | (u32::from(*byte) << (16 - 8 * index))
            });
        for index in 0..=chunk.len() {
            out.push(char::from(
                ALPHABET[(triple >> (18 - 6 * index)) as usize & 63],
            ));
        }
    }
    out
}

/// Strict unpadded base64url: any other character, or leftover bits that are
/// not zero, is refused.
pub fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for byte in text.bytes() {
        let value = ALPHABET.iter().position(|candidate| *candidate == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    (bits < 6 && buffer == 0).then_some(out)
}

/// SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Percent-encodes everything but the RFC 3986 unreserved characters, as one
/// query component.
pub fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// `N` bytes from the operating system's generator.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], Outcome> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|_| Outcome::Internal)?;
    Ok(bytes)
}

/// 32 random bytes as 43 base64url characters: a PKCE verifier, a proof id.
pub fn random_token() -> Result<String, Outcome> {
    random_bytes::<32>().map(|bytes| base64url(&bytes))
}

/// The seconds since the Unix epoch.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Whether `text` is 1 to `limit` printable ASCII characters with no spaces:
/// the shape of every token, code, nonce and state the profile carries.
pub fn is_token(text: &str, limit: usize) -> bool {
    !text.is_empty() && text.len() <= limit && text.bytes().all(|byte| byte.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips_strictly() {
        for bytes in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            &[0xff, 0xfe, 0x00],
        ] {
            assert_eq!(
                base64url_decode(&base64url(bytes)).as_deref(),
                Some(bytes),
                "{bytes:?}"
            );
        }
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        for bad in ["Zm9vYg=", "Zm9v+g", "Zm9v/g", "Zh", "A", "Zm9vY "] {
            assert_eq!(base64url_decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_s256_challenge_of_the_rfc_example_matches() {
        // RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            base64url(&sha256(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn query_components_are_percent_encoded() {
        assert_eq!(
            percent_encode("http://127.0.0.1:5219/a b?x"),
            "http%3A%2F%2F127.0.0.1%3A5219%2Fa%20b%3Fx"
        );
        assert_eq!(percent_encode("az-._~09"), "az-._~09");
    }

    #[test]
    fn random_tokens_are_long_enough_for_pkce_and_never_repeat() {
        let first = random_token().expect("random");
        assert_eq!(first.len(), 43);
        assert_ne!(first, random_token().expect("random"));
        assert!(is_token(&first, 64));
        assert!(!is_token("a b", 64) && !is_token("", 64) && !is_token("abc", 2));
    }
}
