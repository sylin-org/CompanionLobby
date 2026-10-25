//! DPoP keys and proofs (RFC 9449).
//!
//! Every OAuth session this adapter starts gets a fresh ES256 (P-256) key.
//! Each request to the authorization server carries a proof: a compact JWS
//! whose header holds the public key as a JWK, and whose claims bind the
//! request's method and URL (without query or fragment), the time, a fresh
//! `jti`, and the server's nonce once the server has issued one. The tokens
//! the server issues are bound to that key, so the key is part of the
//! material. The JSON is built with `serde_json`, whose objects keep their
//! members in lexicographic order.

use lobby_contracts::Outcome;
use p256::FieldBytes;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};

use crate::codec::{base64url, base64url_decode, now_secs, random_bytes, random_token};

/// How long a proof is valid after it is made, in seconds.
pub const PROOF_LIFETIME_SECS: i64 = 120;

/// One OAuth session's DPoP key. Its `Debug` never prints the key.
#[derive(Clone)]
pub struct DpopKey(SigningKey);

impl std::fmt::Debug for DpopKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DpopKey(..)")
    }
}

impl DpopKey {
    /// A fresh key from the operating system's generator.
    pub fn generate() -> Result<Self, Outcome> {
        // Almost every 32-byte string is a valid scalar; the rare one that is
        // not is drawn again.
        for _ in 0..8 {
            let bytes: [u8; 32] = random_bytes()?;
            if let Ok(key) = SigningKey::from_bytes(&FieldBytes::from(bytes)) {
                return Ok(Self(key));
            }
        }
        Err(Outcome::Internal)
    }

    /// The key as the material keeps it: its scalar in unpadded base64url.
    pub fn encode(&self) -> String {
        base64url(&self.0.to_bytes())
    }

    /// The key the material kept, when it is a valid P-256 scalar.
    pub fn decode(encoded: &str) -> Option<Self> {
        let bytes: [u8; 32] = base64url_decode(encoded)?.try_into().ok()?;
        SigningKey::from_bytes(&FieldBytes::from(bytes))
            .ok()
            .map(Self)
    }

    /// The public key as a JWK.
    pub fn jwk(&self) -> Value {
        let point = self.0.verifying_key().to_encoded_point(false);
        let coordinate = |bytes: Option<&FieldBytes>| bytes.map(|bytes| base64url(bytes));
        json!({
            "crv": "P-256",
            "kty": "EC",
            "x": coordinate(point.x()),
            "y": coordinate(point.y()),
        })
    }

    /// The key's RFC 7638 thumbprint: the SHA-256 of its JWK's required
    /// members in lexicographic order, which is how `serde_json` keeps them.
    pub fn thumbprint(&self) -> String {
        base64url(&crate::codec::sha256(self.jwk().to_string().as_bytes()))
    }

    /// A proof for one request: `method` to `url`, carrying `nonce` when the
    /// server issued one.
    pub fn proof(&self, method: &str, url: &str, nonce: Option<&str>) -> Result<String, Outcome> {
        let header = json!({ "alg": "ES256", "typ": "dpop+jwt", "jwk": self.jwk() });
        let now = now_secs();
        let mut claims = json!({
            "jti": random_token()?,
            "htm": method,
            "htu": htu(url),
            "iat": now,
            "exp": now + PROOF_LIFETIME_SECS,
        });
        if let Some(nonce) = nonce {
            claims["nonce"] = Value::String(nonce.to_owned());
        }
        let input = format!(
            "{}.{}",
            base64url(header.to_string().as_bytes()),
            base64url(claims.to_string().as_bytes())
        );
        let signature: Signature = self.0.sign(input.as_bytes());
        Ok(format!("{input}.{}", base64url(&signature.to_bytes())))
    }
}

/// The `htu` claim: the URL without its query or fragment.
pub fn htu(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier;

    fn segment(text: &str) -> Value {
        serde_json::from_slice(&base64url_decode(text).expect("base64url")).expect("JSON")
    }

    #[test]
    fn proofs_carry_the_profile_claims_and_verify_with_the_header_key() {
        let key = DpopKey::generate().expect("a key");
        let proof = key
            .proof("POST", "https://as.test/oauth/token?x=1#y", Some("n-1"))
            .expect("a proof");
        let parts: Vec<&str> = proof.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header = segment(parts[0]);
        assert_eq!(header["typ"], "dpop+jwt");
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["jwk"], key.jwk());
        assert!(
            header["jwk"].get("d").is_none(),
            "no private part in the header"
        );
        let claims = segment(parts[1]);
        assert_eq!(claims["htm"], "POST");
        assert_eq!(claims["htu"], "https://as.test/oauth/token");
        assert_eq!(claims["nonce"], "n-1");
        assert!(claims.get("ath").is_none());
        let iat = claims["iat"].as_i64().expect("iat");
        assert_eq!(claims["exp"].as_i64(), Some(iat + PROOF_LIFETIME_SECS));
        assert_eq!(claims["jti"].as_str().map(str::len), Some(43));

        let mut point = vec![0x04];
        for name in ["x", "y"] {
            point.extend(
                base64url_decode(header["jwk"][name].as_str().expect("coordinate"))
                    .expect("base64url"),
            );
        }
        let verifying = VerifyingKey::from_sec1_bytes(&point).expect("a point");
        let signature = Signature::from_slice(&base64url_decode(parts[2]).expect("signature"))
            .expect("64 bytes");
        assert!(
            verifying
                .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
                .is_ok()
        );
        let second = key
            .proof("POST", "https://as.test/oauth/token", None)
            .expect("a proof");
        let second_claims = segment(second.split('.').nth(1).expect("claims"));
        assert_ne!(
            second_claims["jti"], claims["jti"],
            "a fresh jti every time"
        );
        assert!(second_claims.get("nonce").is_none());
    }

    #[test]
    fn the_thumbprint_follows_rfc_7638() {
        // RFC 7638 section 3.1 uses an RSA key; this one checks the member
        // order and the hash for a P-256 key by hand.
        let key = DpopKey::generate().expect("a key");
        let jwk = key.jwk();
        let members = format!(
            "{{\"crv\":\"P-256\",\"kty\":\"EC\",\"x\":\"{}\",\"y\":\"{}\"}}",
            jwk["x"].as_str().expect("x"),
            jwk["y"].as_str().expect("y")
        );
        assert_eq!(jwk.to_string(), members);
        assert_eq!(
            key.thumbprint(),
            base64url(&crate::codec::sha256(members.as_bytes()))
        );
        assert_ne!(
            key.thumbprint(),
            DpopKey::generate().expect("another").thumbprint()
        );
    }

    #[test]
    fn keys_survive_their_encoding_and_foreign_text_is_no_key() {
        let key = DpopKey::generate().expect("a key");
        let again = DpopKey::decode(&key.encode()).expect("decodes");
        assert_eq!(again.jwk(), key.jwk());
        assert!(!format!("{key:?}").contains(&key.encode()));
        for bad in ["", "AAAA", &"A".repeat(43), "not base64url!"] {
            assert!(DpopKey::decode(bad).is_none(), "{bad}");
        }
    }
}
