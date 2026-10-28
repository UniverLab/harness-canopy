//! CT31: the one table of every keyboard shortcut the TUI handles, powering
//! the hub's Keys tab and the coverage test that keeps it honest.

/// The contexts a shortcut belongs to, in display order.
pub const CONTEXTS: &[&str] = &[
    "Global",
    "Home and sidebar",
    "Graph live",
    "Agent focus",
    "Dialogs",
    "Crews",
    "Hub shelf",
    "Hub remote",
];

/// One shortcut row.
pub struct KeyRef {
    /// Human display, e.g. "F1", "Shift+←→", "Ctrl+N".
    pub key: &'static str,
    pub context: &'static str,
    pub action: &'static str,
    /// Normalized `KeyCode` token the coverage test matches against source.
    /// Exactly the source spelling without `KeyCode::`: `F1`, `Char('n')`.
    pub code: &'static str,
}

impl KeyRef {
    /// The label to display: the human `key`, or the normalized `code` token
    /// when a row has no separate display spelling.
    pub fn display_key(&self) -> &'static str {
        if self.key.is_empty() {
            self.code
        } else {
            self.key
        }
    }
}

/// Every shortcut, grouped by [`CONTEXTS`] order.
pub static KEY_REFS: &[KeyRef] = &[
    // ── Global ────────────────────────────────────────────────────
    KeyRef {
        key: "F1",
        context: "Global",
        action: "open the Canopy hub",
        code: "F1",
    },
    KeyRef {
        key: "F2",
        context: "Global",
        action: "cycle sidebar layer",
        code: "F2",
    },
    KeyRef {
        key: "F3",
        context: "Global",
        action: "toggle the activity panel",
        code: "F3",
    },
    KeyRef {
        key: "Ctrl+N",
        context: "Global",
        action: "new agent",
        code: "Char('n')",
    },
    KeyRef {
        key: "Ctrl+B",
        context: "Global",
        action: "open the prompt builder",
        code: "Char('b')",
    },
    KeyRef {
        key: "Ctrl+F",
        context: "Global",
        action: "find in scrollback",
        code: "Char('f')",
    },
    KeyRef {
        key: "j",
        context: "Global",
        action: "move down",
        code: "Char('j')",
    },
    KeyRef {
        key: "k",
        context: "Global",
        action: "move up",
        code: "Char('k')",
    },
    // ── Home and sidebar ──────────────────────────────────────────
    KeyRef {
        key: "F1",
        context: "Home and sidebar",
        action: "open the Canopy hub",
        code: "F1",
    },
    KeyRef {
        key: "F4",
        context: "Home and sidebar",
        action: "delete the selected item",
        code: "F4",
    },
    KeyRef {
        key: "F10",
        context: "Home and sidebar",
        action: "back to Home",
        code: "F10",
    },
    KeyRef {
        key: "a",
        context: "Home and sidebar",
        action: "archive the selected graph",
        code: "Char('a')",
    },
    KeyRef {
        key: "c",
        context: "Home and sidebar",
        action: "continue the selected graph",
        code: "Char('c')",
    },
    KeyRef {
        key: "d",
        context: "Home and sidebar",
        action: "dismiss / delete",
        code: "Char('d')",
    },
    KeyRef {
        key: "e",
        context: "Home and sidebar",
        action: "edit the selected item",
        code: "Char('e')",
    },
    KeyRef {
        key: "h",
        context: "Home and sidebar",
        action: "move left",
        code: "Char('h')",
    },
    KeyRef {
        key: "j",
        context: "Home and sidebar",
        action: "move down",
        code: "Char('j')",
    },
    KeyRef {
        key: "k",
        context: "Home and sidebar",
        action: "move up",
        code: "Char('k')",
    },
    KeyRef {
        key: "l",
        context: "Home and sidebar",
        action: "enter / focus",
        code: "Char('l')",
    },
    KeyRef {
        key: "n",
        context: "Home and sidebar",
        action: "new agent",
        code: "Char('n')",
    },
    KeyRef {
        key: "p",
        context: "Home and sidebar",
        action: "pause the selected graph",
        code: "Char('p')",
    },
    KeyRef {
        key: "r",
        context: "Home and sidebar",
        action: "run the selected graph",
        code: "Char('r')",
    },
    KeyRef {
        key: "t",
        context: "Home and sidebar",
        action: "open the tail pane",
        code: "Char('t')",
    },
    KeyRef {
        key: "w",
        context: "Home and sidebar",
        action: "toggle the watcher",
        code: "Char('w')",
    },
    KeyRef {
        key: "x",
        context: "Home and sidebar",
        action: "reset the selected graph",
        code: "Char('x')",
    },
    KeyRef {
        key: "y",
        context: "Home and sidebar",
        action: "confirm",
        code: "Char('y')",
    },
    KeyRef {
        key: "A",
        context: "Home and sidebar",
        action: "archive graph (shift)",
        code: "Char('A')",
    },
    KeyRef {
        key: "C",
        context: "Home and sidebar",
        action: "continue-skip (shift)",
        code: "Char('C')",
    },
    KeyRef {
        key: "E",
        context: "Home and sidebar",
        action: "edit (shift)",
        code: "Char('E')",
    },
    KeyRef {
        key: "R",
        context: "Home and sidebar",
        action: "restore archived graph",
        code: "Char('R')",
    },
    KeyRef {
        key: "[",
        context: "Home and sidebar",
        action: "previous sidebar tab",
        code: "Char('[')",
    },
    KeyRef {
        key: "]",
        context: "Home and sidebar",
        action: "next sidebar tab",
        code: "Char(']')",
    },
    KeyRef {
        key: ",",
        context: "Home and sidebar",
        action: "previous panel face",
        code: "Char(',')",
    },
    KeyRef {
        key: ".",
        context: "Home and sidebar",
        action: "next panel face",
        code: "Char('.')",
    },
    KeyRef {
        key: "1",
        context: "Home and sidebar",
        action: "jump to the first tab",
        code: "Char('1')",
    },
    KeyRef {
        key: "2",
        context: "Home and sidebar",
        action: "jump to the second tab",
        code: "Char('2')",
    },
    KeyRef {
        key: "7",
        context: "Home and sidebar",
        action: "jump to the last tab",
        code: "Char('7')",
    },
    // ── Graph live (display only) ─────────────────────────────────
    KeyRef {
        key: "F1",
        context: "Graph live",
        action: "open the Canopy hub",
        code: "F1",
    },
    KeyRef {
        key: "F4",
        context: "Graph live",
        action: "delete the selected item",
        code: "F4",
    },
    KeyRef {
        key: "F10",
        context: "Graph live",
        action: "back to Home",
        code: "F10",
    },
    KeyRef {
        key: "Enter",
        context: "Graph live",
        action: "open / select",
        code: "Enter",
    },
    KeyRef {
        key: "Esc",
        context: "Graph live",
        action: "back out",
        code: "Esc",
    },
    KeyRef {
        key: "a",
        context: "Graph live",
        action: "archive",
        code: "Char('a')",
    },
    KeyRef {
        key: "e",
        context: "Graph live",
        action: "edit",
        code: "Char('e')",
    },
    KeyRef {
        key: "r",
        context: "Graph live",
        action: "run",
        code: "Char('r')",
    },
    KeyRef {
        key: "A",
        context: "Graph live",
        action: "archive (shift)",
        code: "Char('A')",
    },
    KeyRef {
        key: "R",
        context: "Graph live",
        action: "restore",
        code: "Char('R')",
    },
    KeyRef {
        key: "j",
        context: "Graph live",
        action: "move down",
        code: "Char('j')",
    },
    KeyRef {
        key: "k",
        context: "Graph live",
        action: "move up",
        code: "Char('k')",
    },
    // ── Agent focus ───────────────────────────────────────────────
    KeyRef {
        key: "F1",
        context: "Agent focus",
        action: "open the Canopy hub",
        code: "F1",
    },
    KeyRef {
        key: "F4",
        context: "Agent focus",
        action: "delete the selected item",
        code: "F4",
    },
    KeyRef {
        key: "F10",
        context: "Agent focus",
        action: "leave focus (reserved)",
        code: "F10",
    },
    KeyRef {
        key: "/",
        context: "Agent focus",
        action: "search",
        code: "Char('/')",
    },
    KeyRef {
        key: "[",
        context: "Agent focus",
        action: "previous sidebar tab",
        code: "Char('[')",
    },
    KeyRef {
        key: "]",
        context: "Agent focus",
        action: "next sidebar tab",
        code: "Char(']')",
    },
    KeyRef {
        key: "a",
        context: "Agent focus",
        action: "attach / add",
        code: "Char('a')",
    },
    KeyRef {
        key: "d",
        context: "Agent focus",
        action: "dismiss orphaned session",
        code: "Char('d')",
    },
    KeyRef {
        key: "e",
        context: "Agent focus",
        action: "edit",
        code: "Char('e')",
    },
    KeyRef {
        key: "h",
        context: "Agent focus",
        action: "back to Preview",
        code: "Char('h')",
    },
    KeyRef {
        key: "j",
        context: "Agent focus",
        action: "scroll log down",
        code: "Char('j')",
    },
    KeyRef {
        key: "k",
        context: "Agent focus",
        action: "scroll log up",
        code: "Char('k')",
    },
    KeyRef {
        key: "n",
        context: "Agent focus",
        action: "new agent",
        code: "Char('n')",
    },
    KeyRef {
        key: "q",
        context: "Agent focus",
        action: "quit",
        code: "Char('q')",
    },
    KeyRef {
        key: "r",
        context: "Agent focus",
        action: "revive orphaned session",
        code: "Char('r')",
    },
    KeyRef {
        key: "s",
        context: "Agent focus",
        action: "stop the session",
        code: "Char('s')",
    },
    KeyRef {
        key: "t",
        context: "Agent focus",
        action: "terminate the session",
        code: "Char('t')",
    },
    KeyRef {
        key: "x",
        context: "Agent focus",
        action: "kill / close",
        code: "Char('x')",
    },
    KeyRef {
        key: "Ctrl+C",
        context: "Agent focus",
        action: "interrupt the session — on opencode, press twice to send",
        code: "Char('c')",
    },
    // ── Dialogs ───────────────────────────────────────────────────
    KeyRef {
        key: "d",
        context: "Dialogs",
        action: "delete a row",
        code: "Char('d')",
    },
    KeyRef {
        key: "f",
        context: "Dialogs",
        action: "focus a field",
        code: "Char('f')",
    },
    KeyRef {
        key: "n",
        context: "Dialogs",
        action: "new / add",
        code: "Char('n')",
    },
    KeyRef {
        key: "s",
        context: "Dialogs",
        action: "save",
        code: "Char('s')",
    },
    KeyRef {
        key: "Space",
        context: "Dialogs",
        action: "toggle / select",
        code: "Char(' ')",
    },
    KeyRef {
        key: "5",
        context: "Dialogs",
        action: "jump to a field",
        code: "Char('5')",
    },
    KeyRef {
        key: "h",
        context: "Dialogs",
        action: "move left",
        code: "Char('h')",
    },
    KeyRef {
        key: "i",
        context: "Dialogs",
        action: "insert",
        code: "Char('i')",
    },
    // ── Crews ─────────────────────────────────────────────────────
    KeyRef {
        key: "Space",
        context: "Crews",
        action: "toggle the row",
        code: "Char(' ')",
    },
    KeyRef {
        key: "a",
        context: "Crews",
        action: "add a member",
        code: "Char('a')",
    },
    KeyRef {
        key: "d",
        context: "Crews",
        action: "delete a member",
        code: "Char('d')",
    },
    KeyRef {
        key: "e",
        context: "Crews",
        action: "edit a member",
        code: "Char('e')",
    },
    KeyRef {
        key: "p",
        context: "Crews",
        action: "previous section",
        code: "Char('p')",
    },
    KeyRef {
        key: "r",
        context: "Crews",
        action: "refresh",
        code: "Char('r')",
    },
    KeyRef {
        key: "x",
        context: "Crews",
        action: "clear",
        code: "Char('x')",
    },
    // ── Hub shelf ────────────────────────────────────────────────
    KeyRef {
        key: "Enter",
        context: "Hub shelf",
        action: "open the selected file",
        code: "Enter",
    },
    KeyRef {
        key: "Space",
        context: "Hub shelf",
        action: "open the selected file",
        code: "Char(' ')",
    },
    KeyRef {
        key: "d",
        context: "Hub shelf",
        action: "delete the selected file (y/n confirm)",
        code: "Char('d')",
    },
    KeyRef {
        key: "c",
        context: "Hub shelf",
        action: "copy its path",
        code: "Char('c')",
    },
    KeyRef {
        key: "j",
        context: "Hub shelf",
        action: "move down",
        code: "Char('j')",
    },
    KeyRef {
        key: "k",
        context: "Hub shelf",
        action: "move up",
        code: "Char('k')",
    },
    KeyRef {
        key: "y",
        context: "Hub shelf",
        action: "confirm the delete",
        code: "Char('y')",
    },
    KeyRef {
        key: "n",
        context: "Hub shelf",
        action: "cancel the delete",
        code: "Char('n')",
    },
    // ── Hub remote ───────────────────────────────────────────────
    KeyRef {
        key: "Enter",
        context: "Hub remote",
        action: "sign in, toggle the switch, or pair a device",
        code: "Enter",
    },
    KeyRef {
        key: "d",
        context: "Hub remote",
        action: "revoke the selected device (y/n confirm)",
        code: "Char('d')",
    },
    KeyRef {
        key: "j",
        context: "Hub remote",
        action: "move down",
        code: "Char('j')",
    },
    KeyRef {
        key: "k",
        context: "Hub remote",
        action: "move up",
        code: "Char('k')",
    },
    KeyRef {
        key: "y",
        context: "Hub remote",
        action: "confirm the revoke",
        code: "Char('y')",
    },
    KeyRef {
        key: "n",
        context: "Hub remote",
        action: "cancel the revoke",
        code: "Char('n')",
    },
];

/// Rows of one context, in table order.
pub fn rows_for(context: &'static str) -> impl Iterator<Item = &'static KeyRef> {
    KEY_REFS.iter().filter(move |row| row.context == context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// (context, source files) — every `KeyCode::F(n)` / `KeyCode::Char('c')`
    /// token in these files' PRODUCTION code must appear in KEY_REFS under the
    /// context. Contexts without a file mapping are exempt.
    const ENFORCED: &[(&str, &[&str])] = &[
        ("Global", &["src/tui/event/mod.rs"]),
        ("Home and sidebar", &["src/tui/event/home_preview.rs"]),
        ("Agent focus", &["src/tui/event/agent_focus.rs"]),
        (
            "Dialogs",
            &[
                "src/tui/event/graph_editor.rs",
                "src/tui/event/graph_form.rs",
                "src/tui/event/new_agent_dialog.rs",
                "src/tui/event/knowledge_dialog.rs",
            ],
        ),
        ("Crews", &["src/tui/event/crews_keys.rs"]),
    ];

    #[test]
    fn every_context_is_known_and_every_row_has_key_and_action() {
        for row in KEY_REFS {
            assert!(
                CONTEXTS.contains(&row.context),
                "unknown context {:?} for key {:?}",
                row.context,
                row.key
            );
            assert!(!row.key.is_empty(), "row with empty key");
            assert!(!row.action.is_empty(), "row {} has empty action", row.key);
            assert!(!row.code.is_empty(), "row {} has empty code", row.key);
        }
    }

    /// Walks the handlers' match arms and fails when a handled key in a
    /// listed context is missing from the table.
    #[test]
    fn keymap_reference_covers_every_handled_key() {
        let root = env!("CARGO_MANIFEST_DIR");
        let key_re = regex::Regex::new(r"KeyCode::F\((\d+)\)").expect("key regex");
        let char_re = regex::Regex::new(r"KeyCode::Char\('([^']*)'\)").expect("char regex");

        let mut misses: Vec<String> = Vec::new();
        for (context, paths) in ENFORCED {
            let allowed: BTreeSet<&str> = KEY_REFS
                .iter()
                .filter(|row| row.context == *context)
                .map(|row| row.code)
                .collect();
            for path in *paths {
                let source = std::fs::read_to_string(std::path::Path::new(root).join(path))
                    .unwrap_or_else(|e| panic!("read {path}: {e}"));
                let production = production_region(&source);
                for (number, line) in production.lines().enumerate() {
                    let mut tokens: Vec<String> = Vec::new();
                    for capture in key_re.captures_iter(line) {
                        tokens.push(format!("F{}", &capture[1]));
                    }
                    for capture in char_re.captures_iter(line) {
                        tokens.push(format!("Char('{}')", &capture[1]));
                    }
                    for token in tokens {
                        if !allowed.contains(token.as_str()) {
                            misses.push(format!(
                                "{path}:{} — {context} handled {token} but has no table row",
                                number + 1
                            ));
                        }
                    }
                }
            }
        }

        assert!(
            misses.is_empty(),
            "keymap reference is missing handled keys:\n{}",
            misses.join("\n")
        );
    }

    /// The production region of a source file: everything before the first
    /// `#[cfg(test)]` whose next non-empty line opens a `mod ` test module.
    /// (`event/mod.rs` has `#[cfg(test)] use …` before `handle_key`, so a
    /// blanket "first `#[cfg(test)]`" cut would delete production code.)
    fn production_region(source: &str) -> String {
        let lines: Vec<&str> = source.lines().collect();
        let mut cut = lines.len();
        for (i, line) in lines.iter().enumerate() {
            if line.trim() != "#[cfg(test)]" {
                continue;
            }
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j < lines.len() && lines[j].trim().starts_with("mod ") {
                cut = i;
                break;
            }
        }
        lines[..cut].join("\n")
    }
}
