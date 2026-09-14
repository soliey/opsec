use rand::Rng;
use std::fmt;

/// Session codes are exactly this many decimal digits, e.g. "042817".
pub const CODE_LEN: usize = 6;

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
}

impl fmt::Display for SessionCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
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
}
