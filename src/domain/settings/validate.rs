//! CM51: validation of a setting value against its [`SettingDef`].
//!
//! Every refusal names the key, the rejected value and the valid range, and
//! nothing is written when a value is refused.

use std::path::Path;

use super::{DurationUnit, Kind, SettingDef};

/// Validate `value` against `def`. `Ok(())` means the value may be written.
pub fn validate_value(def: &SettingDef, value: &toml_edit::Value) -> Result<(), String> {
    match def.kind {
        Kind::Bool => require_bool(def.key, value),
        Kind::Int { min, max } => require_int(def, value, min, max),
        Kind::Float { min, max } => require_float(def.key, value, min, max),
        Kind::Enum { options } => require_enum(def.key, value, options),
        Kind::String => require_string(def.key, value),
        Kind::ChronoFormat => require_chrono_format(def.key, value),
        Kind::Duration { unit, min, max } => require_duration(def, value, unit, min, max),
        Kind::PathList => require_path_list(def.key, value),
        // Structured values are validated by parsing the file, not here.
        Kind::Structured => Ok(()),
    }
}

/// Parse a value typed on the command line or sent over MCP. TOML syntax is
/// tried first; for a String or Enum setting whatever the user typed is the
/// string value, so `canopy config set theme modern` works unquoted.
pub fn parse_cli_value(def: &SettingDef, raw: &str) -> Result<toml_edit::Value, String> {
    if let Ok(value) = raw.parse::<toml_edit::Value>() {
        return Ok(value);
    }
    match def.kind {
        Kind::String | Kind::Enum { .. } | Kind::ChronoFormat => {
            Ok(toml_edit::Value::from(raw.to_string()))
        }
        _ => Err(format!(
            "{}: \"{}\" is not valid for {}",
            def.key,
            raw,
            def.kind_label()
        )),
    }
}

/// The value as it appears in an error message.
fn shown(value: &toml_edit::Value) -> String {
    value.to_string()
}

fn require_bool(key: &str, value: &toml_edit::Value) -> Result<(), String> {
    if value.as_bool().is_some() {
        return Ok(());
    }
    Err(format!("{key}: {} is not a boolean", shown(value)))
}

fn require_int(
    def: &SettingDef,
    value: &toml_edit::Value,
    min: i64,
    max: i64,
) -> Result<(), String> {
    let key = def.key;
    let Some(number) = value.as_integer() else {
        return Err(format!("{key}: {} is not a whole number", shown(value)));
    };
    if key == "rag_max_file_mb" {
        // One definition of the ceiling: reuse the validator the wizard and
        // doctor already share.
        if crate::domain::canopy_config::validate_rag_max_file_mb(number.max(0) as u32).is_err() {
            return Err(range_error(key, number, min, max, ""));
        }
        return Ok(());
    }
    if number < min || number > max {
        return Err(range_error(key, number, min, max, ""));
    }
    Ok(())
}

fn require_float(key: &str, value: &toml_edit::Value, min: f64, max: f64) -> Result<(), String> {
    let number = value
        .as_float()
        .or_else(|| value.as_integer().map(|n| n as f64));
    let Some(number) = number else {
        return Err(format!("{key}: {} is not a number", shown(value)));
    };
    if number < min || number > max {
        return Err(format!(
            "{key}: {number} is out of range; valid range is {min}..={max}"
        ));
    }
    Ok(())
}

fn require_enum(
    key: &str,
    value: &toml_edit::Value,
    options: &'static [&'static str],
) -> Result<(), String> {
    let valid = options.join(", ");
    if let Some(text) = value.as_str() {
        if options.contains(&text) {
            return Ok(());
        }
        return Err(format!(
            "{key}: \"{text}\" is not valid; expected one of {valid}"
        ));
    }
    Err(format!(
        "{key}: {} is not valid; expected one of {valid}",
        shown(value)
    ))
}

fn require_string(key: &str, value: &toml_edit::Value) -> Result<(), String> {
    if value.as_str().is_some() {
        return Ok(());
    }
    Err(format!("{key}: {} is not a string", shown(value)))
}

fn require_chrono_format(key: &str, value: &toml_edit::Value) -> Result<(), String> {
    use chrono::format::{Item, StrftimeItems};
    use std::fmt::Write;
    let Some(text) = value.as_str() else {
        return Err(format!("{key}: {} is not a string", shown(value)));
    };
    let invalid = || format!("{key}: \"{text}\" is not a valid chrono format");
    if StrftimeItems::new(text).any(|item| matches!(item, Item::Error)) {
        return Err(invalid());
    }
    // Formatting an invalid format panics through `to_string()`; write into a
    // String and check the fmt::Result instead.
    let instant = chrono::NaiveDate::from_ymd_opt(2026, 1, 2)
        .and_then(|d| d.and_hms_opt(3, 4, 5))
        .expect("fixed instant is valid");
    let mut out = String::new();
    write!(out, "{}", instant.format(text)).map_err(|_| invalid())?;
    Ok(())
}

fn require_duration(
    def: &SettingDef,
    value: &toml_edit::Value,
    unit: DurationUnit,
    min: u64,
    max: u64,
) -> Result<(), String> {
    let key = def.key;
    let Some(number) = value.as_integer() else {
        return Err(format!("{key}: {} is not a whole number", shown(value)));
    };
    if number < min as i64 || number > max as i64 {
        return Err(range_error(
            key,
            number,
            min as i64,
            max as i64,
            unit.label(),
        ));
    }
    Ok(())
}

fn require_path_list(key: &str, value: &toml_edit::Value) -> Result<(), String> {
    let Some(entries) = value.as_array() else {
        return Err(format!("{key}: {} is not a list of paths", shown(value)));
    };
    for entry in entries.iter() {
        let Some(path) = entry.as_str() else {
            return Err(format!("{key}: {} is not a path", shown(entry)));
        };
        if !Path::new(path).is_dir() {
            return Err(format!("{key}: \"{path}\" is not a directory"));
        }
    }
    Ok(())
}

fn range_error(key: &str, value: i64, min: i64, max: i64, unit: &str) -> String {
    let suffix = if unit.is_empty() {
        String::new()
    } else {
        format!(" ({unit})")
    };
    format!("{key}: {value} is out of range; valid range is {min}..={max}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::settings::find;

    fn value(text: &str) -> toml_edit::Value {
        text.parse().expect("valid TOML value")
    }

    #[test]
    fn config_set_refuses_type_and_range_errors() {
        let limit = find("spec_attempt_limit").unwrap();
        let error = validate_value(limit, &value("0")).unwrap_err();
        assert!(error.contains("spec_attempt_limit"), "{error}");
        assert!(error.contains("0 is out of range"), "{error}");
        assert!(error.contains("1..=20"), "{error}");

        let theme = find("theme").unwrap();
        let error = validate_value(theme, &value("\"neon\"")).unwrap_err();
        assert!(error.contains("theme"), "{error}");
        assert!(error.contains("neon"), "{error}");
        assert!(error.contains("classic"), "{error}");

        let threshold = find("similarity_threshold").unwrap();
        let error = validate_value(threshold, &value("\"x\"")).unwrap_err();
        assert!(error.contains("similarity_threshold"), "{error}");
    }

    /// CM52: a chrono format string is accepted only if it formats a fixed
    /// instant; the invalid specifier `%Q` is refused.
    #[test]
    fn chrono_format_is_validated_by_formatting() {
        let def = find("tui.datetime_format").unwrap();
        assert!(validate_value(def, &value("\"%Y-%m-%d %H:%M\"")).is_ok());
        assert!(validate_value(def, &value("\"%d/%m/%Y\"")).is_ok());
        let error = validate_value(def, &value("\"%Q\"")).unwrap_err();
        assert!(error.contains("tui.datetime_format"), "{error}");
        assert!(error.contains("not a valid chrono format"), "{error}");
        // A non-string is refused too.
        assert!(validate_value(def, &value("5")).is_err());

        let time = find("tui.time_format").unwrap();
        assert!(validate_value(time, &value("\"%H:%M\"")).is_ok());
        assert!(validate_value(time, &value("\"%Q\"")).is_err());
    }

    #[test]
    fn config_set_refuses_missing_directory_in_path_list() {
        let dirs = find("rag_personal_dirs").unwrap();
        let error = validate_value(dirs, &value("[\"/does/not/exist\"]")).unwrap_err();
        assert!(error.contains("rag_personal_dirs"), "{error}");
        assert!(error.contains("/does/not/exist"), "{error}");
        assert!(validate_value(dirs, &value("[\"/\"]")).is_ok());
    }

    #[test]
    fn rag_max_file_mb_uses_the_shared_ceiling_validator() {
        let def = find("rag_max_file_mb").unwrap();
        assert!(validate_value(def, &value("10")).is_ok());
        assert!(validate_value(def, &value("0")).is_err());
        assert!(validate_value(def, &value("101")).is_err());
    }

    #[test]
    fn parse_cli_value_wraps_bare_text_only_for_string_and_enum_kinds() {
        assert_eq!(
            parse_cli_value(find("theme").unwrap(), "modern")
                .unwrap()
                .as_str(),
            Some("modern")
        );
        assert_eq!(
            parse_cli_value(find("embeddings_model").unwrap(), "intfloat/e5")
                .unwrap()
                .as_str(),
            Some("intfloat/e5")
        );
        assert!(parse_cli_value(find("spec_attempt_limit").unwrap(), "abc").is_err());
        assert_eq!(
            parse_cli_value(find("spec_attempt_limit").unwrap(), "7")
                .unwrap()
                .as_integer(),
            Some(7)
        );
    }
}
