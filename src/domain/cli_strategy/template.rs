//! Invocation-template argv engine: parse `{{marker}}` tokens, validate the
//! model-bearing token, and render the final argv. Split out of
//! `cli_strategy.rs` to keep every production file within the file-size gate.

use std::collections::HashMap;

use super::CliStrategy;

/// One parsed piece of a template token: either literal text, or a
/// `{{marker}}` (required) / `{{marker?}}` (optional) reference.
enum TokenPart<'a> {
    Literal(&'a str),
    Marker { name: &'a str, optional: bool },
}

impl CliStrategy {
    /// Build argv from the invocation template. Returns the substituted
    /// argv words (without headless flags) for the given marker values.
    /// Effort and mcp_config are declared but not yet wired (CB3/CB4).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_argv_from_template(
        &self,
        template: &str,
        prompt: &str,
        model: Option<&str>,
        working_dir: Option<&str>,
        session_arg: Option<(&str, &str)>,
        effort: Option<&str>,
        mcp_config: Option<&str>,
    ) -> Vec<String> {
        let mut markers: HashMap<&str, Option<&str>> = HashMap::new();
        // When prompt is delivered via stdin, treat {{prompt}} as unavailable
        // so it and any preceding flag are elided from argv.
        if self.prompt_via_stdin {
            markers.insert("prompt", None);
        } else {
            markers.insert("prompt", Some(prompt));
        }
        markers.insert("model", model);
        markers.insert("workdir", working_dir);
        markers.insert("effort", effort);
        markers.insert("mcp_config", mcp_config);
        let session_id_val = session_arg.map(|(_, id)| id);
        markers.insert("session_id", session_id_val);
        let session_flag_val = session_arg.map(|(flag, _)| flag);
        markers.insert("session_flag", session_flag_val);

        let tokens: Vec<&str> = template.split_whitespace().collect();
        let mut argv: Vec<String> = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            let token = tokens[i];
            let has_marker = token.contains("{{");
            if !has_marker {
                let next_dropped = if i + 1 < tokens.len() {
                    let next = tokens[i + 1];
                    next.contains("{{") && Self::resolve_token(next, &markers).is_none()
                } else {
                    false
                };
                if token.starts_with('-') && next_dropped {
                    i += 1;
                    continue;
                }
                argv.push(token.to_string());
                i += 1;
            } else {
                if let Some(rendered) = Self::resolve_token(token, &markers) {
                    argv.push(rendered);
                }
                i += 1;
            }
        }
        argv
    }

    /// FR4: a template's "model-bearing" token (the one carrying the `model`
    /// marker) must never be able to render empty — that would silently drop
    /// the whole model argument, the exact defect this spec exists to fix,
    /// just hidden behind the `?` spelling instead of the old
    /// drop-whole-token-on-any-missing-marker rule. A token where EVERY marker
    /// in it (including `model` itself, i.e. spelled `{{model?}}`) is optional
    /// can do exactly that when model is absent. `{{model}}` (required, the
    /// only spelling a model-bearing token should ever use) is always safe and
    /// never triggers this.
    ///
    /// `template` is the raw `invocation_template` string (no `None` case here
    /// — callers only invoke this when a template exists). Returns `Some(msg)`
    /// naming the platform and the offending token; `None` when the template
    /// has no unsafe model-bearing token.
    pub fn unsafe_optional_model_token(template: &str, platform: &str) -> Option<String> {
        for token in template.split_whitespace() {
            let parts = Self::parse_token_parts(token);
            let has_model_marker = parts
                .iter()
                .any(|p| matches!(p, TokenPart::Marker { name, .. } if *name == "model"));
            if !has_model_marker {
                continue;
            }
            let all_optional = parts.iter().all(|p| match p {
                TokenPart::Marker { optional, .. } => *optional,
                TokenPart::Literal(_) => true,
            });
            if all_optional {
                return Some(format!(
                    "platform '{platform}' invocation_template token '{token}' has only \
                     optional markers including {{{{model?}}}} — it would render empty \
                     (silently dropping the model argument) whenever model is absent. \
                     Spell the model marker as required: '{{{{model}}}}'."
                ));
            }
        }
        None
    }

    /// One parsed piece of a template token: either literal text, or a
    /// `{{marker}}` (required) / `{{marker?}}` (optional) reference.
    fn parse_token_parts(token: &str) -> Vec<TokenPart<'_>> {
        let mut parts = Vec::new();
        let mut rest = token;
        loop {
            let Some(start) = rest.find("{{") else {
                if !rest.is_empty() {
                    parts.push(TokenPart::Literal(rest));
                }
                break;
            };
            if start > 0 {
                parts.push(TokenPart::Literal(&rest[..start]));
            }
            let after_open = &rest[start + 2..];
            let Some(end) = after_open.find("}}") else {
                parts.push(TokenPart::Literal(&rest[start..]));
                break;
            };
            let raw_name = &after_open[..end];
            let (name, optional) = match raw_name.strip_suffix('?') {
                Some(n) => (n, true),
                None => (raw_name, false),
            };
            parts.push(TokenPart::Marker { name, optional });
            rest = &after_open[end + 2..];
        }
        parts
    }

    /// Resolve one template token against the marker values. `None` means the
    /// token is dropped entirely — either because a REQUIRED marker in it is
    /// unavailable (today's rule, unchanged: FR3), or because after removing
    /// every unavailable OPTIONAL marker and its bound literal glue (FR1/FR2)
    /// nothing is left to render (FR2: "a token that becomes empty is
    /// dropped"). `Some(rendered)` is the fully substituted token.
    fn resolve_token(token: &str, markers: &HashMap<&str, Option<&str>>) -> Option<String> {
        let parts = Self::parse_token_parts(token);

        // FR3: any unavailable REQUIRED marker drops the whole token, exactly
        // as `all_markers_available` did before — optional markers never
        // participate in this check.
        for part in &parts {
            if let TokenPart::Marker {
                name,
                optional: false,
            } = part
            {
                markers.get(name).copied().flatten()?;
            }
        }

        // FR1/FR2: render left to right. A literal immediately followed by an
        // unavailable OPTIONAL marker is the "glue bound to" that marker (FR2)
        // — both are dropped as a pair. Every other literal, and every
        // available marker's value, is emitted normally.
        let mut out = String::new();
        let mut i = 0;
        while i < parts.len() {
            match &parts[i] {
                TokenPart::Literal(s) => {
                    let glue_drops = matches!(
                        parts.get(i + 1),
                        Some(TokenPart::Marker { name, optional: true })
                            if markers.get(name).copied().flatten().is_none()
                    );
                    if glue_drops {
                        i += 2;
                    } else {
                        out.push_str(s);
                        i += 1;
                    }
                }
                TokenPart::Marker { name, .. } => {
                    if let Some(val) = markers.get(name).copied().flatten() {
                        out.push_str(val);
                    }
                    // Unavailable here can only be an optional marker (a
                    // required-unavailable one already returned above) — skip
                    // it silently, its own glue (if any) was already consumed
                    // in the Literal arm above, or there was none to consume.
                    i += 1;
                }
            }
        }

        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
}
