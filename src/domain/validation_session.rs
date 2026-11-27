//! Validation for interactive session display names.

/// Validate a mutable session name.
///
/// Names are 1-32 chars of lowercase ASCII letters, digits, and `-`.
pub fn validate_session_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Session name must not be empty.".to_string());
    }
    if name.len() > 32 {
        return Err("Session name must be at most 32 characters.".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(
            "Session name may only contain lowercase ASCII letters (a-z), digits (0-9), and '-'."
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_session_name;

    #[test]
    fn accepts_valid_names() {
        for name in ["a", "laricifomes", "toona-2", "a-b-c-1"] {
            assert!(validate_session_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_invalid_names() {
        for name in ["", "A", "abc_def", "abc def", "abc.def", &"a".repeat(33)] {
            assert!(validate_session_name(name).is_err(), "{name}");
        }
    }
}
