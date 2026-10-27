//! Named-mutex list validation, shared by node writers, ensemble writers,
//! and the graph importer so the three can never drift apart. Split out of
//! `validation.rs` to keep every production file within the file-size gate.

/// Whether `name` is a valid mutex resource name:
/// `^[a-z][a-z0-9-]{0,31}$` — lowercase start, then lowercase alphanumerics
/// or hyphens, at most 32 bytes.
pub fn is_valid_mutex_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    if name.len() > 32 {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Validate a node/ensemble `mutex` list: every name must match
/// `^[a-z][a-z0-9-]{0,31}$`. `None` (absent) and an empty list are both valid
/// (no mutex). The refusal names `owner` (e.g. `Node 'x'`) and the value.
pub fn validate_mutex_list(owner: &str, mutex: Option<&[String]>) -> Result<(), String> {
    let Some(names) = mutex else {
        return Ok(());
    };
    for name in names {
        if !is_valid_mutex_name(name) {
            return Err(format!(
                "{owner} has an invalid mutex name '{name}': must match ^[a-z][a-z0-9-]{{0,31}}$."
            ));
        }
    }
    Ok(())
}
