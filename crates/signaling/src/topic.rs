//! Deriving a Supabase Realtime Broadcast channel name from a session code.

use consent::SessionCode;
use sha2::{Digest, Sha256};

/// A channel topic derived from `code`, not the code itself — so the six
/// digits a human reads aloud don't sit in a channel-name URL that (e.g.)
/// ends up in a proxy's access log. This does **not** add secrecy beyond
/// the code's own ~20 bits of entropy (see `consent::SessionCode::
/// derive_key`'s doc comment for that accepted limitation, and
/// `consent::transport`'s module doc for what actually secures the
/// pre-`Active` leg) — an attacker who can enumerate topics can still
/// brute-force all million codes just as they always could; this only
/// avoids gratuitously exposing the literal digits.
pub fn topic_for_code(code: &SessionCode) -> String {
    let digest = Sha256::digest(code.as_str().as_bytes());
    format!("remote-assist:{}", hex::encode(&digest[..8]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_code_produces_the_same_topic() {
        let code = SessionCode::parse("042817").unwrap();
        assert_eq!(topic_for_code(&code), topic_for_code(&code));
    }

    #[test]
    fn different_codes_produce_different_topics() {
        let a = SessionCode::parse("042817").unwrap();
        let b = SessionCode::parse("042818").unwrap();
        assert_ne!(topic_for_code(&a), topic_for_code(&b));
    }

    #[test]
    fn topic_never_contains_the_literal_digits() {
        let code = SessionCode::parse("042817").unwrap();
        assert!(!topic_for_code(&code).contains("042817"));
    }

    #[test]
    fn topic_is_prefixed_for_the_rls_policy() {
        let code = SessionCode::parse("042817").unwrap();
        assert!(topic_for_code(&code).starts_with("remote-assist:"));
    }
}
