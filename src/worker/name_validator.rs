use regex::Regex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NameValidationError {
    #[error("Worker name is empty")]
    Empty,

    #[error("Worker name exceeds maximum length of 64 characters")]
    TooLong,

    #[error("Worker name contains invalid characters: {0}")]
    InvalidCharacters(String),
}

pub struct NameValidator {
    pattern: Regex,
}

impl NameValidator {
    pub fn new() -> Self {
        Self {
            pattern: Regex::new(r"^[a-zA-Z0-9_\-\p{Hiragana}\p{Katakana}\p{Han}]{1,64}$")
                .expect("Invalid regex pattern"),
        }
    }

    pub fn validate(&self, name: &str) -> Result<(), NameValidationError> {
        if name.is_empty() {
            return Err(NameValidationError::Empty);
        }

        if name.len() > 64 {
            return Err(NameValidationError::TooLong);
        }

        if !self.pattern.is_match(name) {
            let invalid_chars: String = name
                .chars()
                .filter(|c| !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-')
                    && !is_japanese_char(*c))
                .collect();
            return Err(NameValidationError::InvalidCharacters(invalid_chars));
        }

        Ok(())
    }
}

impl Default for NameValidator {
    fn default() -> Self {
        Self::new()
    }
}

fn is_japanese_char(c: char) -> bool {
    matches!(c,
        '\u{3000}'..='\u{303F}' |  // Japanese punctuation
        '\u{3040}'..='\u{309F}' |  // Hiragana
        '\u{30A0}'..='\u{30FF}' |  // Katakana
        '\u{4E00}'..='\u{9FFF}'    // Kanji
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_validation_valid() {
        let validator = NameValidator::new();
        assert!(validator.validate("auth-feature").is_ok());
        assert!(validator.validate("認証機能").is_ok());
        assert!(validator.validate("worker_123").is_ok());
        assert!(validator.validate("MyWorker").is_ok());
        assert!(validator.validate("a").is_ok());
    }

    #[test]
    fn test_name_validation_invalid_characters() {
        let validator = NameValidator::new();
        assert!(validator.validate("worker/name").is_err());
        assert!(validator.validate("worker:name").is_err());
        assert!(validator.validate("worker\\name").is_err());
        assert!(validator.validate("worker*name").is_err());
        assert!(validator.validate("worker?name").is_err());
        assert!(validator.validate("worker\"name").is_err());
        assert!(validator.validate("worker<name").is_err());
        assert!(validator.validate("worker>name").is_err());
        assert!(validator.validate("worker|name").is_err());
    }

    #[test]
    fn test_name_validation_length() {
        let validator = NameValidator::new();
        assert!(validator.validate("").is_err());
        assert!(validator.validate(&"a".repeat(65)).is_err());
        assert!(validator.validate(&"a".repeat(64)).is_ok());
        assert!(validator.validate(&"a".repeat(1)).is_ok());
    }

    #[test]
    fn test_name_validation_empty() {
        let validator = NameValidator::new();
        match validator.validate("") {
            Err(NameValidationError::Empty) => {},
            _ => panic!("Expected Empty error"),
        }
    }

    #[test]
    fn test_name_validation_too_long() {
        let validator = NameValidator::new();
        match validator.validate(&"a".repeat(65)) {
            Err(NameValidationError::TooLong) => {},
            _ => panic!("Expected TooLong error"),
        }
    }

    #[test]
    fn test_name_validation_japanese_chars() {
        let validator = NameValidator::new();
        assert!(validator.validate("あいうえお").is_ok());
        assert!(validator.validate("アイウエオ").is_ok());
        assert!(validator.validate("漢字テスト").is_ok());
        assert!(validator.validate("mix英語日本語123").is_ok());
    }
}
