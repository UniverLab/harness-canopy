//! Refusal of quoted markers in check commands (audit finding F-10).
//!
//! [`render_check_command`](super::check_render::render_check_command) expands
//! the quoted markers as single-quoted shell words. Written inside double
//! quotes (`"{{spec_name}}"`) the value renders as `"'...'"`, and the shell
//! still expands `$(...)` and backticks coming from the spec name — so such
//! a command is refused here, when the node is saved and again before it
//! runs, instead of running.

use super::check_render::CHECK_COMMAND_QUOTED_MARKERS;

/// Quoted marker to the `CANOPY_*` environment variable carrying the same
/// value, or `None` when there is none (`workdir`: the command already runs
/// in the workdir). One table so the refusal hint and its test share it.
pub(crate) const ENV_HINT: &[(&str, Option<&str>)] = &[
    ("graph_name", Some("CANOPY_GRAPH_NAME")),
    ("workdir", None),
    ("spec_id", Some("CANOPY_SPEC_ID")),
    ("spec_name", Some("CANOPY_SPEC_NAME")),
    ("spec_code", Some("CANOPY_SPEC_CODE")),
    ("spec_type", Some("CANOPY_SPEC_TYPE")),
    ("spec_scope", Some("CANOPY_SPEC_SCOPE")),
    ("spec_commit_message", Some("CANOPY_SPEC_COMMIT_MESSAGE")),
];

/// One shell-quoting context of the scanner stack.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Frame {
    Normal,
    Single,
    Double,
    Backtick,
    /// Inside `$(...)`: behaves as [`Frame::Normal`], counting nested
    /// parens so the frame pops at its own matching `)`.
    Subst(u32),
}

/// The refusal for the first quoted marker in `command`, or `None` when
/// every quoted marker sits bare, in single quotes (including the
/// `'{{x}}'` wrapper the renderer strips), or directly in a `$(...)`
/// inside double quotes. Raw markers are never reported, and neither is
/// `$CANOPY_*` environment-variable use.
pub(crate) fn quoted_marker_error(command: &str) -> Option<String> {
    let bytes = command.as_bytes();
    let mut stack = vec![Frame::Normal];
    let mut index = 0;
    while index < bytes.len() {
        if stack.last() == Some(&Frame::Single) {
            if bytes[index] == b'\'' {
                stack.pop();
            }
            index += 1;
            continue;
        }
        if let Some(name) = marker_at(command, index) {
            if matches!(stack.last(), Some(Frame::Double) | Some(Frame::Backtick)) {
                return Some(quoted_message(name, stack.last() == Some(&Frame::Backtick)));
            }
            index += name.len() + 4;
            continue;
        }
        index = step(bytes, index, &mut stack);
    }
    None
}

/// `{{name}}` at `index` with `name` in the quoted vocabulary, else `None`.
/// A `\{{name}}` escape never reaches here: the backslash step skips the
/// second `{`, so the remainder cannot match the `{{` prefix.
fn marker_at(command: &str, index: usize) -> Option<&str> {
    let inner = command
        .get(index..)?
        .strip_prefix("{{")?
        .split("}}")
        .next()?;
    if inner.is_empty() || !CHECK_COMMAND_QUOTED_MARKERS.contains(&inner) {
        return None;
    }
    Some(inner)
}

/// Consume `bytes[index]` outside single quotes; return the next index.
fn step(bytes: &[u8], index: usize, stack: &mut Vec<Frame>) -> usize {
    match bytes[index] {
        b'\\' => index + 2,
        // Inside double quotes a single quote is literal text (POSIX), so
        // `echo "it's {{spec_name}}"` keeps the marker inside `Double` and
        // is refused instead of hiding in a phantom `Single` frame.
        b'\'' if !matches!(stack.last(), Some(Frame::Double)) => {
            stack.push(Frame::Single);
            index + 1
        }
        b'"' => {
            if stack.last() == Some(&Frame::Double) {
                stack.pop();
            } else {
                stack.push(Frame::Double);
            }
            index + 1
        }
        b'`' => {
            if stack.last() == Some(&Frame::Backtick) {
                stack.pop();
            } else {
                stack.push(Frame::Backtick);
            }
            index + 1
        }
        // `$'...'` is literal inside double quotes, same as a bare `'`.
        b'$' if bytes.get(index + 1) == Some(&b'\'')
            && !matches!(stack.last(), Some(Frame::Double)) =>
        {
            stack.push(Frame::Single);
            index + 2
        }
        b'$' if bytes.get(index + 1) == Some(&b'(') => {
            stack.push(Frame::Subst(1));
            index + 2
        }
        b'(' if matches!(stack.last(), Some(Frame::Subst(_))) => {
            if let Some(Frame::Subst(depth)) = stack.pop() {
                stack.push(Frame::Subst(depth + 1));
            }
            index + 1
        }
        b')' if matches!(stack.last(), Some(Frame::Subst(_))) => {
            if let Some(Frame::Subst(depth)) = stack.pop() {
                if depth > 1 {
                    stack.push(Frame::Subst(depth - 1));
                }
            }
            index + 1
        }
        _ => index + 1,
    }
}

/// Build the refusal for `{{name}}` found in `backtick` (else double
/// quotes). The only builder of this text, so every call site shares one
/// contract.
fn quoted_message(name: &str, backtick: bool) -> String {
    let inside = if backtick {
        "backticks"
    } else {
        "double quotes"
    };
    let mut message = format!(
        "marker {{{{{name}}}}} sits inside {inside}; markers are inserted as \
         single-quoted words, so use it bare or inside single quotes"
    );
    if let Some(var) = ENV_HINT
        .iter()
        .find(|(marker, _)| *marker == name)
        .and_then(|(_, var)| *var)
    {
        message.push_str(&format!(", or read ${var} instead"));
    }
    message
}

#[cfg(test)]
#[path = "check_quote_tests.rs"]
mod check_quote_tests;
