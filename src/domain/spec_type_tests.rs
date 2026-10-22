//! Tests for SPECCODE: conventional title parsing, commit message building,
//! and spec naming resolution.

use crate::domain::spec_type::{
    commit_message, parse_conventional_title, resolve_spec_naming, ConventionalTitle, SpecNaming,
    SpecType,
};

#[test]
fn test_parse_conventional_title_simple() {
    let result = parse_conventional_title("feat: add new feature");
    assert_eq!(
        result,
        Some(ConventionalTitle {
            spec_type: SpecType::Feat,
            scope: None,
            subject: "add new feature".to_string(),
        })
    );
}

#[test]
fn test_parse_conventional_title_with_scope() {
    let result = parse_conventional_title("fix(tui): open F1 from inside a session");
    assert_eq!(
        result,
        Some(ConventionalTitle {
            spec_type: SpecType::Fix,
            scope: Some("tui".to_string()),
            subject: "open F1 from inside a session".to_string(),
        })
    );
}

#[test]
fn test_parse_conventional_title_with_breaking_change() {
    let result = parse_conventional_title("feat!: breaking change");
    assert_eq!(
        result,
        Some(ConventionalTitle {
            spec_type: SpecType::Feat,
            scope: None,
            subject: "breaking change".to_string(),
        })
    );
}

#[test]
fn test_parse_conventional_title_with_scope_and_breaking_change() {
    let result = parse_conventional_title("fix(tui)!: urgent fix");
    assert_eq!(
        result,
        Some(ConventionalTitle {
            spec_type: SpecType::Fix,
            scope: Some("tui".to_string()),
            subject: "urgent fix".to_string(),
        })
    );
}

#[test]
fn test_parse_conventional_title_non_conventional() {
    // No colon-space separator
    assert_eq!(parse_conventional_title("fix:no space"), None);
    assert_eq!(parse_conventional_title("just a name"), None);
    assert_eq!(parse_conventional_title(""), None);
    assert_eq!(parse_conventional_title(": "), None);
    assert_eq!(parse_conventional_title("fix:"), None);
    assert_eq!(parse_conventional_title("fix: "), None);
    assert_eq!(parse_conventional_title("unknown(type): something"), None);
    // Unknown type
    assert_eq!(parse_conventional_title("chores: something"), None);
    // Empty subject
    assert_eq!(parse_conventional_title("fix: "), None);
    // Malformed scope
    assert_eq!(parse_conventional_title("fix(): something"), None);
    assert_eq!(parse_conventional_title("fix((nested)): something"), None);
    assert_eq!(parse_conventional_title("fix(tui: something"), None);
}

#[test]
fn test_commit_message_with_scope() {
    let msg = commit_message(SpecType::Fix, Some("tui"), "open F1 from inside a session");
    assert_eq!(msg, "fix(tui): open F1 from inside a session");
}

#[test]
fn test_commit_message_without_scope() {
    let msg = commit_message(SpecType::Feat, None, "add new feature");
    assert_eq!(msg, "feat: add new feature");
}

#[test]
fn test_commit_message_empty_scope() {
    let msg = commit_message(SpecType::Docs, Some(""), "update readme");
    assert_eq!(msg, "docs: update readme");
}

#[test]
fn test_resolve_spec_naming_explicit_type_wins() {
    // Explicit type beats parsed type
    let result = resolve_spec_naming(Some(SpecType::Fix), None, "feat: add new feature");
    assert_eq!(
        result,
        SpecNaming {
            name: "add new feature".to_string(),
            spec_type: Some(SpecType::Fix),
            scope: None,
        }
    );
}

#[test]
fn test_resolve_spec_naming_explicit_scope_wins() {
    // Explicit scope beats parsed scope
    let result = resolve_spec_naming(None, Some("api".to_string()), "fix(tui): something");
    assert_eq!(
        result,
        SpecNaming {
            name: "something".to_string(),
            spec_type: Some(SpecType::Fix),
            scope: Some("api".to_string()),
        }
    );
}

#[test]
fn test_resolve_spec_naming_parses_conventional_title() {
    // No explicit params, conventional title parsed
    let result = resolve_spec_naming(None, None, "fix(tui): open F1");
    assert_eq!(
        result,
        SpecNaming {
            name: "open F1".to_string(),
            spec_type: Some(SpecType::Fix),
            scope: Some("tui".to_string()),
        }
    );
}

#[test]
fn test_resolve_spec_naming_non_conventional_passthrough() {
    // Non-conventional title passes through
    let result = resolve_spec_naming(None, None, "just a name");
    assert_eq!(
        result,
        SpecNaming {
            name: "just a name".to_string(),
            spec_type: None,
            scope: None,
        }
    );
}

#[test]
fn test_resolve_spec_naming_explicit_overrides_partial() {
    // Explicit type with conventional title without scope
    let result = resolve_spec_naming(Some(SpecType::Fix), None, "docs: update readme");
    assert_eq!(
        result,
        SpecNaming {
            name: "update readme".to_string(),
            spec_type: Some(SpecType::Fix),
            scope: None,
        }
    );
}
