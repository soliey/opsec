use hkdf::Hkdf;
use rand::Rng;
use serde::de::{self, Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};
use sha2::Sha256;
use std::fmt;
use zeroize::Zeroize;

/// Session codes are exactly this many decimal digits, e.g. "042817".
pub const CODE_LEN: usize = 6;

pub const SESSION_KEY_LEN: usize = 32;

/// Transport key material derived from an already-confirmed [`SessionCode`]
/// (see [`SessionCode::derive_key`] and, for the only sanctioned way to
/// obtain one outside of tests, [`crate::HandshakeMachine::session_key`]).
/// Zeroized on drop; `Debug` never prints the bytes.
#[derive(Clone)]
pub struct SessionKey([u8; SESSION_KEY_LEN]);

impl SessionKey {
    pub fn as_bytes(&self) -> &[u8; SESSION_KEY_LEN] {
        &self.0
    }
}

impl Drop for SessionKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionKey(..)")
    }
}

/// A validated 6-digit session code. The only way to get one is
/// [`SessionCode::generate`] (host side) or [`SessionCode::parse`]
/// (helper side, typing in what the host read out to them).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionCode(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeError {
    WrongLength,
    NonDigit,
}

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodeError::WrongLength => write!(f, "code must be exactly {CODE_LEN} digits"),
            CodeError::NonDigit => write!(f, "code must contain only digits"),
        }
    }
}

impl std::error::Error for CodeError {}

impl SessionCode {
    /// Generates a fresh random code. Called on the host side when starting
    /// a new pairing; the host reads this aloud/shares it out-of-band.
    pub fn generate() -> Self {
        let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
        SessionCode(format!("{n:0width$}", width = CODE_LEN))
    }

    /// Parses user-typed input into a code, rejecting anything that isn't
    /// exactly `CODE_LEN` ASCII digits once surrounding/interior whitespace
    /// (people often type "042 817") is stripped.
    pub fn parse(input: &str) -> Result<Self, CodeError> {
        let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
        if cleaned.chars().count() != CODE_LEN {
            return Err(CodeError::WrongLength);
        }
        if !cleaned.chars().all(|c| c.is_ascii_digit()) {
            return Err(CodeError::NonDigit);
        }
        Ok(SessionCode(cleaned))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Derives [`SESSION_KEY_LEN`] bytes of key material from this code via
    /// HKDF-SHA256, domain-separated by `info` (e.g.
    /// `b"remote-assist/sdp-auth/v1"`) so different phase-4 uses of the
    /// same code can never be confused for one another or reused across
    /// purposes.
    ///
    /// This is *not* a substitute for DTLS-SRTP's own key exchange — a
    /// 6-digit code has only ~20 bits of entropy, nowhere near enough to
    /// resist offline brute force as a standalone secret. What it's for:
    /// both sides already independently confirmed they hold the *same*
    /// code (that's the whole guarantee `HandshakeMachine` provides), so a
    /// key derived from it lets each side authenticate that the SDP/ICE
    /// material arriving over the signaling channel actually came from
    /// that confirmed peer and wasn't swapped by a MITM on signaling — see
    /// `crates/transport`'s signaling module, the only intended caller.
    ///
    /// Deliberately private: reach this only through
    /// [`crate::HandshakeMachine::session_key`], which only returns a key
    /// while `Active` — that's what ties key derivation to the consent
    /// guarantee instead of any code string someone happens to hold before
    /// both sides have actually confirmed it.
    pub(crate) fn derive_key(&self, info: &[u8]) -> SessionKey {
        let hk = Hkdf::<Sha256>::new(None, self.0.as_bytes());
        let mut okm = [0u8; SESSION_KEY_LEN];
        hk.expand(info, &mut okm)
            .expect("SESSION_KEY_LEN is a valid HKDF-SHA256 output length");
        SessionKey(okm)
    }
}

impl fmt::Display for SessionCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Wire format is just the digit string. Serializing is trivial; the
/// interesting half is `Deserialize`, which intentionally does **not**
/// derive — it routes through [`SessionCode::parse`] so a message that
/// crossed a real network can't hand this type a value that skips the
/// "exactly `CODE_LEN` ASCII digits" invariant every other constructor
/// enforces.
impl Serialize for SessionCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SessionCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        SessionCode::parse(&s).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_six_digits() {
        for _ in 0..200 {
            let code = SessionCode::generate();
            assert_eq!(code.as_str().len(), CODE_LEN);
            assert!(code.as_str().chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn parse_accepts_plain_six_digits() {
        let code = SessionCode::parse("042817").unwrap();
        assert_eq!(code.as_str(), "042817");
    }

    #[test]
    fn parse_strips_whitespace_people_type() {
        let code = SessionCode::parse(" 042 817 ").unwrap();
        assert_eq!(code.as_str(), "042817");
    }

    #[test]
    fn parse_rejects_wrong_length() {
        assert_eq!(SessionCode::parse("12345"), Err(CodeError::WrongLength));
        assert_eq!(SessionCode::parse("1234567"), Err(CodeError::WrongLength));
        assert_eq!(SessionCode::parse(""), Err(CodeError::WrongLength));
    }

    #[test]
    fn parse_rejects_non_digits() {
        assert_eq!(SessionCode::parse("12a456"), Err(CodeError::NonDigit));
        assert_eq!(SessionCode::parse("12-456"), Err(CodeError::NonDigit));
    }

    #[test]
    fn equality_is_by_digits() {
        let a = SessionCode::parse("042817").unwrap();
        let b = SessionCode::parse("042817").unwrap();
        let c = SessionCode::parse("042818").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn derive_key_is_deterministic_for_the_same_code_and_info() {
        let code = SessionCode::parse("042817").unwrap();
        assert_eq!(
            code.derive_key(b"purpose-a").as_bytes(),
            code.derive_key(b"purpose-a").as_bytes()
        );
    }

    #[test]
    fn derive_key_is_domain_separated_by_info() {
        let code = SessionCode::parse("042817").unwrap();
        assert_ne!(
            code.derive_key(b"purpose-a").as_bytes(),
            code.derive_key(b"purpose-b").as_bytes()
        );
    }

    #[test]
    fn derive_key_differs_between_codes() {
        let a = SessionCode::parse("042817").unwrap();
        let b = SessionCode::parse("042818").unwrap();
        assert_ne!(
            a.derive_key(b"same-info").as_bytes(),
            b.derive_key(b"same-info").as_bytes()
        );
    }

    #[test]
    fn derive_key_is_not_the_all_zero_key() {
        let code = SessionCode::parse("000000").unwrap();
        assert_ne!(code.derive_key(b"purpose-a").as_bytes(), &[0u8; SESSION_KEY_LEN]);
    }

    #[test]
    fn session_key_debug_never_prints_key_material() {
        let code = SessionCode::parse("042817").unwrap();
        let key = code.derive_key(b"purpose-a");
        assert_eq!(format!("{key:?}"), "SessionKey(..)");
    }

    #[test]
    fn serializes_as_the_plain_digit_string() {
        let code = SessionCode::parse("042817").unwrap();
        assert_eq!(serde_json::to_string(&code).unwrap(), "\"042817\"");
    }

    #[test]
    fn deserialize_round_trips_a_valid_code() {
        let code = SessionCode::parse("042817").unwrap();
        let json = serde_json::to_string(&code).unwrap();
        let back: SessionCode = serde_json::from_str(&json).unwrap();
        assert_eq!(code, back);
    }

    #[test]
    fn deserialize_rejects_what_parse_would_reject() {
        // A malicious or buggy relay can't hand this type a value that
        // bypasses `SessionCode::parse`'s own validation.
        let err: Result<SessionCode, _> = serde_json::from_str("\"12a456\"");
        assert!(err.is_err());
        let err: Result<SessionCode, _> = serde_json::from_str("\"12345\"");
        assert!(err.is_err());
    }
}
