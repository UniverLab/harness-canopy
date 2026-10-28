//! The install confirmation question shared by `canopy graph install` and
//! `canopy node install`.
//!
//! The summary of what a document runs is always printed first (the review
//! is never skipped); this module only decides whether the install proceeds:
//! `--yes` proceeds without asking, a non-TTY refuses with a fixed message
//! that names `--yes`, and an interactive terminal asks
//! `Install <name> from <source>? [y/N]` and reads the answer.

use std::io::{BufRead, Write};

/// What a non-TTY (and no `--yes`) gets: the summary was printed, and the
/// install stops here.
pub(crate) const NO_TTY_REFUSAL: &str =
    "refusing to install without confirmation: re-run with --yes after reviewing the commands above";

/// One confirmation question. `name` and `source` come from the review, so
/// the question names exactly what was reviewed.
pub(crate) struct Ask {
    /// The document's name.
    pub name: String,
    /// The review's source label (`official`, `owner/repo`, with `@<ref>`).
    pub source: String,
    /// `--yes` was passed: install without asking.
    pub yes: bool,
    /// Whether both stdin and stdout are a TTY.
    pub tty: bool,
    /// Send the question to stderr instead of stdout (so `--json` stdout
    /// stays machine-readable).
    pub to_stderr: bool,
    /// The document is an unreviewed editor draft: the question says so.
    pub is_draft: bool,
}

/// The suffix for a refused draft install: the draft was consumed by the
/// review fetch itself, so it is gone whatever the answer was — the user
/// needs a new link from the editor.
pub(crate) fn draft_consumed_note() -> &'static str {
    " The draft is gone; copy a new link from the editor."
}

impl Ask {
    /// The question this ask would print: `Install <name> from <source>? [y/N]`,
    /// with the unreviewed-draft warning appended for a draft.
    pub(crate) fn question(&self) -> String {
        let base = format!("Install {} from {}? [y/N] ", self.name, self.source);
        if self.is_draft {
            format!(
                "{base}{}",
                crate::domain::nodes::review::UNREVIEWED_DRAFT_LINE
            )
        } else {
            base
        }
    }

    /// `Ok(())` when the install may proceed: `--yes`, or an affirmative
    /// answer on a TTY. Without a TTY and without `--yes`, [`NO_TTY_REFUSAL`]
    /// — and any other answer declines with an abort message. The question
    /// is read from `read`, so tests inject their own answers.
    pub(crate) fn confirm(&self, read: &mut impl BufRead) -> Result<(), String> {
        if self.yes {
            return Ok(());
        }
        if !self.tty {
            return Err(NO_TTY_REFUSAL.to_string());
        }
        let question = self.question();
        if self.to_stderr {
            eprint!("{question}");
            let _ = std::io::stderr().flush();
        } else {
            print!("{question}");
            let _ = std::io::stdout().flush();
        }
        let mut answer = String::new();
        if let Err(error) = read.read_line(&mut answer) {
            return Err(format!(
                "Could not read the confirmation answer for '{}': {error}.",
                self.name
            ));
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => Ok(()),
            _ => Err(format!(
                "Aborted: '{}' was not installed (answer was not yes).",
                self.name
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(yes: bool, tty: bool) -> Ask {
        Ask {
            name: "shared".to_string(),
            source: "acme/widgets@v1.2".to_string(),
            yes,
            tty,
            to_stderr: false,
            is_draft: false,
        }
    }

    fn answered(answer: &str) -> &[u8] {
        answer.as_bytes()
    }

    #[test]
    fn without_a_tty_and_without_yes_the_refusal_names_yes() {
        let error = ask(false, false)
            .confirm(&mut answered(""))
            .expect_err("non-TTY must refuse");
        assert_eq!(error, NO_TTY_REFUSAL);
        assert!(error.contains("--yes"), "{error}");
    }

    #[test]
    fn yes_proceeds_without_reading_an_answer() {
        ask(true, false)
            .confirm(&mut answered(""))
            .expect("--yes proceeds even without a TTY");
        ask(true, true)
            .confirm(&mut answered("n\n"))
            .expect("--yes never asks");
    }

    #[test]
    fn a_tty_answering_y_proceeds() {
        ask(false, true)
            .confirm(&mut answered("y\n"))
            .expect("y proceeds");
        ask(false, true)
            .confirm(&mut answered("YES\n"))
            .expect("YES proceeds");
        ask(false, true)
            .confirm(&mut answered("  yes  \n"))
            .expect("whitespace-padded yes proceeds");
    }

    #[test]
    fn a_tty_answering_empty_or_no_aborts_with_the_name() {
        let empty = ask(false, true)
            .confirm(&mut answered("\n"))
            .expect_err("empty answer aborts");
        assert!(empty.contains("'shared' was not installed"), "{empty}");
        let no = ask(false, true)
            .confirm(&mut answered("n\n"))
            .expect_err("n aborts");
        assert!(no.contains("'shared'"), "{no}");
    }

    #[test]
    fn the_question_names_the_reviewed_document_and_source() {
        let question = ask(false, true).question();
        assert_eq!(question, "Install shared from acme/widgets@v1.2? [y/N] ");
    }

    #[test]
    fn a_draft_question_appends_the_unreviewed_line() {
        let mut draft = ask(false, true);
        draft.is_draft = true;
        draft.source = "draft".to_string();
        let question = draft.question();
        assert!(
            question.starts_with("Install shared from draft? [y/N] "),
            "{question}"
        );
        assert!(
            question.contains("This is an unreviewed draft, not a published document."),
            "{question}"
        );
        assert!(
            !ask(false, true).question().contains("unreviewed draft"),
            "published documents carry no draft warning"
        );
    }

    #[test]
    fn the_consumed_note_says_the_draft_is_gone_and_names_a_new_link() {
        let note = draft_consumed_note();
        assert!(note.contains("gone"), "{note}");
        assert!(note.contains("copy a new link from the editor"), "{note}");
    }
}
