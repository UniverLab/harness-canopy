//! Tests for SPECCODE: conventional title parsing, commit message building,
//! and spec naming resolution.

use crate::domain::spec_type::{
    check_message, check_subject, commit_message, commit_message_opt, has_header_prefix,
    parse_conventional_title, resolve_spec_naming, strip_header, ConventionalTitle, SpecNaming,
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
fn commit_message_strips_stacked_and_matching_headers() {
    assert_eq!(
        commit_message(
            SpecType::Feat,
            Some("build"),
            "feat(build): keep the compile log"
        ),
        "feat(build): keep the compile log"
    );
    assert_eq!(
        commit_message(SpecType::Feat, Some("build"), "feat(a): fix(b): x"),
        "feat(build): x"
    );
    assert_eq!(
        commit_message(SpecType::Fix, Some("tui"), "feat(api): open F1"),
        "fix(tui): open F1"
    );
    assert_eq!(
        commit_message(SpecType::Feat, None, "feat(a): x"),
        "feat: x"
    );
    assert_eq!(
        commit_message(SpecType::Fix, Some("tui"), "fix(tui): "),
        "fix(tui): fix(tui): "
    );
    assert_eq!(
        commit_message_opt(None, Some("tui"), "fix(tui): open F1"),
        None
    );
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

#[test]
fn subject_refuses_empty_and_whitespace() {
    assert!(check_subject(Some(SpecType::Fix), "").is_err());
    assert!(check_subject(Some(SpecType::Fix), "   ").is_err());
    assert!(check_subject(None, "").is_err());
    assert!(check_subject(None, "   ").is_err());
}

#[test]
fn subject_refuses_multi_line() {
    assert!(check_subject(Some(SpecType::Fix), "one\ntwo").is_err());
    assert!(check_subject(Some(SpecType::Fix), "one\rtwo").is_err());
    assert!(check_subject(None, "one\ntwo").is_err());
}

#[test]
fn subject_refuses_repeated_header_on_typed_spec() {
    let err = check_subject(Some(SpecType::Fix), "fix(tui): open F1").unwrap_err();
    assert!(
        err.contains("repeated prefix") && err.contains("open F1"),
        "{err}"
    );
    assert!(check_subject(Some(SpecType::Feat), "feat: add x").is_err());
    assert!(check_subject(Some(SpecType::Fix), "docs: other text").is_err());
}

#[test]
fn subject_allows_plain_text_on_typed_spec() {
    assert!(check_subject(Some(SpecType::Fix), "open F1").is_ok());
}

#[test]
fn subject_allows_any_single_line_on_untyped_spec() {
    // A spec without a type stores its name verbatim today.
    assert!(check_subject(None, "fix(tui): open F1").is_ok());
    assert!(check_subject(None, "just a name").is_ok());
}

#[test]
fn strip_header_unstacks_repeated_prefixes() {
    assert_eq!(strip_header("fix(tui): fix(tui): x"), "x");
    assert_eq!(strip_header("open F1"), "open F1");
}

#[test]
fn header_prefix_detects_conventional_start() {
    assert!(has_header_prefix("fix(tui): x"));
    assert!(has_header_prefix("feat!: x"));
    assert!(!has_header_prefix("open F1"));
}

#[test]
fn message_accepts_well_formed_shapes() {
    assert!(check_message("fix(tui): open F1").is_ok());
    assert!(check_message("feat: add x").is_ok());
    assert!(check_message("fix(tui)!: urgent").is_ok());
}

#[test]
fn message_refuses_repeated_prefix() {
    assert!(check_message("fix(tui): fix(tui): x").is_err());
    // A scope may itself contain ": ": the subject is the part after the
    // header, so a doubled prefix hidden behind such a scope is still found.
    assert!(check_message("fix(a: b): fix: x").is_err());
}

#[test]
fn message_accepts_colon_inside_scope() {
    assert!(check_message("fix(a: b): x").is_ok());
}

#[test]
fn message_refuses_malformed_shapes() {
    assert!(check_message("not a header").is_err());
    assert!(check_message("fix: ").is_err());
    assert!(check_message("fix(tui):").is_err());
    assert!(check_message("unknown: x").is_err());
}
