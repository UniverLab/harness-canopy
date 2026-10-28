//! SPECCODE: the change type of a spec — one of the Conventional Commits
//! types — plus the two pure helpers every other layer uses: parsing a
//! conventional title into `(type, scope, subject)` and building the
//! deterministic commit message from the stored fields. One parser, in the
//! domain layer, so no surface invents its own regex.

use serde::{Deserialize, Serialize};

/// The Conventional Commits types a spec may carry. Stored in the DB as the
/// lowercase label; serialized lowercase and accepted by `spec list`'s
/// `type` filter in the same form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SpecType {
    Feat,
    Fix,
    Docs,
    Style,
    Refactor,
    Perf,
    Test,
    Build,
    Ci,
    Chore,
    Revert,
}

pub const SPEC_TYPES: [SpecType; 11] = [
    SpecType::Feat,
    SpecType::Fix,
    SpecType::Docs,
    SpecType::Style,
    SpecType::Refactor,
    SpecType::Perf,
    SpecType::Test,
    SpecType::Build,
    SpecType::Ci,
    SpecType::Chore,
    SpecType::Revert,
];

impl SpecType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SpecType::Feat => "feat",
            SpecType::Fix => "fix",
            SpecType::Docs => "docs",
            SpecType::Style => "style",
            SpecType::Refactor => "refactor",
            SpecType::Perf => "perf",
            SpecType::Test => "test",
            SpecType::Build => "build",
            SpecType::Ci => "ci",
            SpecType::Chore => "chore",
            SpecType::Revert => "revert",
        }
    }

    /// The uppercase code prefix (`FEAT`, `FIX`, `DOCS`).
    pub fn code_prefix(&self) -> &'static str {
        match self {
            SpecType::Feat => "FEAT",
            SpecType::Fix => "FIX",
            SpecType::Docs => "DOCS",
            SpecType::Style => "STYLE",
            SpecType::Refactor => "REFACTOR",
            SpecType::Perf => "PERF",
            SpecType::Test => "TEST",
            SpecType::Build => "BUILD",
            SpecType::Ci => "CI",
            SpecType::Chore => "CHORE",
            SpecType::Revert => "REVERT",
        }
    }

    pub fn parse(value: &str) -> Option<SpecType> {
        SPEC_TYPES.iter().copied().find(|t| t.as_str() == value)
    }
}

impl std::fmt::Display for SpecType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A conventional title split into its deterministic parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConventionalTitle {
    pub spec_type: SpecType,
    pub scope: Option<String>,
    pub subject: String,
}

/// Parse a name of the form `<type>(<scope>)?!?: <subject>` — the variant
/// Conventional Commits actually allows on the first line. Returns `None`
/// when the name is not conventional (no `: `, unknown type, empty subject
/// or scope), so callers store the title verbatim and the spec simply has
/// no type. Breaking changes (`!`) are not modelled per spec: the code and
/// commit type are what matters here.
pub fn parse_conventional_title(name: &str) -> Option<ConventionalTitle> {
    let (head, subject) = name.split_once(": ")?;
    let subject = subject.trim();
    if subject.is_empty() {
        return None;
    }
    let head = head.trim_end_matches('!');
    let head = head.trim();
    let (spec_type_raw, scope): (&str, Option<String>) = match (head.find('('), head.ends_with(')'))
    {
        (Some(open), true) if open > 0 => {
            let scope = &head[open + 1..head.len() - 1];
            if scope.is_empty() || scope.contains('(') {
                return None;
            }
            (&head[..open], Some(scope.to_string()))
        }
        (None, false) => (head, None),
        _ => return None,
    };
    let spec_type = SpecType::parse(spec_type_raw.trim())?;
    Some(ConventionalTitle {
        spec_type,
        scope,
        subject: subject.to_string(),
    })
}

/// The commit message a deterministic check node should use verbatim:
/// `<type>(<scope>): <name>` or `<type>: <name>` without a scope. Built from
/// the stored fields — never from the code, which never appears in commits.
pub fn commit_message(spec_type: SpecType, scope: Option<&str>, subject: &str) -> String {
    match scope {
        Some(scope) if !scope.is_empty() => {
            format!("{}({}): {}", spec_type.as_str(), scope, subject)
        }
        _ => format!("{}: {}", spec_type.as_str(), subject),
    }
}

/// A spec name as the surfaces receive it, after the conventional-title
/// split: the name to store paired with the (possibly parsed-and-overridden)
/// type and scope. Explicit parameters win over the parsed ones; a parsed
/// title is stored as the subject alone so the built commit message (`<type>
/// (<scope>): <name>`) never repeats the type twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecNaming {
    pub name: String,
    pub spec_type: Option<SpecType>,
    pub scope: Option<String>,
}

/// Apply the SPECCODE name/parameter precedence to any creation or update
/// surface: an explicit `spec_type` beats a conventional title's type, an
/// explicit `spec_scope` beats its scope, and when nothing is explicit the
/// conventional title provides all three (storing only its subject as the
/// name); a non-conventional name passes through untouched.
pub fn resolve_spec_naming(
    explicit_type: Option<SpecType>,
    explicit_scope: Option<String>,
    name: &str,
) -> SpecNaming {
    match parse_conventional_title(name) {
        Some(parsed) => SpecNaming {
            name: parsed.subject,
            spec_type: Some(explicit_type.unwrap_or(parsed.spec_type)),
            scope: explicit_scope.or(parsed.scope),
        },
        None => SpecNaming {
            name: name.to_string(),
            spec_type: explicit_type,
            scope: explicit_scope,
        },
    }
}

/// `type` for templates, env vars, etc. — lowercase or `""` on a legacy
/// spec with no type. One place for the `.map(|t| t.as_str()).unwrap_or("")`
/// idiom so no surface spells it differently.
pub fn type_str(spec_type: Option<SpecType>) -> &'static str {
    spec_type.map(|t| t.as_str()).unwrap_or("")
}

/// `scope` for templates/env vars — empty when absent or empty, never
/// `None` or a stray whitespace string.
pub fn scope_str(scope: Option<&str>) -> &str {
    scope.filter(|s| !s.is_empty()).unwrap_or("")
}

/// The deterministic commit message for a typed spec, or `None` when it has
/// no type (legacy) — built from stored fields, never from the code.
pub fn commit_message_opt(
    spec_type: Option<SpecType>,
    scope: Option<&str>,
    name: &str,
) -> Option<String> {
    spec_type.map(|t| commit_message(t, scope, name))
}

/// Like [`commit_message_opt`] but defaults to `""` for templating, the way
/// the legacy empty-string convention renders on an untyped spec.
pub fn commit_message_or_default(
    spec_type: Option<SpecType>,
    scope: Option<&str>,
    name: &str,
) -> String {
    commit_message_opt(spec_type, scope, name).unwrap_or_default()
}
