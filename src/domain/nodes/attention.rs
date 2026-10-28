//! Attention marks for install review: the command shapes worth a second
//! look, shared by every surface that shows what a document runs.
//!
//! One list, [`PATTERNS`], mirrors the patterns the catalogue flags on its
//! document pages (canopy-web's `runs.ts`), so a reader sees the same marks
//! in `canopy graph install`'s summary, in the MCP `graph_edit`/`node_install`
//! review, and on the web. A match only earns a mark; it never hides or
//! blocks anything.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// One attention mark: `pattern` matched on `line` of the scanned text,
/// at the 1-based `line_number`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionMark {
    /// The pattern's label, e.g. `"sudo"`.
    pub pattern: String,
    /// The line that matched, verbatim.
    pub line: String,
    /// 1-based line number of `line` within the scanned text.
    pub line_number: usize,
}

/// One command shape worth a second look: the label the mark shows and its
/// matcher over a single line.
pub struct Pattern {
    pub label: &'static str,
    pub test: fn(&str) -> bool,
}

/// The command shapes worth a second look. Each regex below starts a
/// command token at the line start or after a shell separator
/// (`[\s;&|()]`), so `evaluation` is not `eval` and `no-sudo-here` is not
/// `sudo`.
static PIPE_SHELL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\|\s*(?:\S*/)?(?:bash|sh)(?:\s|$|[;&])").expect("pipe-to-shell regex")
});

static SUDO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:^|[\s;&|()])sudo(?:[\s;&|()'"]|$)"#).expect("sudo regex"));

static RM_FLAGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[\s;&|()])rm\s+-([A-Za-z]+)").expect("rm-flags regex"));

static CHMOD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[\s;&|()])chmod\b[^|;&]*777").expect("chmod regex"));

static GIT_PUSH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[\s;&|()])git\s+push\b").expect("git-push regex"));

static FORCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"--force(?:[=\s-]|$)").expect("force regex"));

static SENSITIVE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|[\s"'=])~/\.(?:ssh|config)(?:/|$)|(?:^|[\s"'=])/etc(?:/|$)"#)
        .expect("sensitive-path regex")
});

static WRITE_VERB: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s;&|()])(?:cp|mv|rm|tee|chmod|chown|install|mkdir|touch|ln|rsync|scp)\b")
        .expect("write-verb regex")
});

static SED_IN_PLACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[\s;&|()])sed\s+-i").expect("sed-in-place regex"));

static EVAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:^|[\s;&|()])eval(?:[\s;&|()'"]|$)"#).expect("eval regex"));

static BASE64_DECODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s;&|()])base64\b[^|;&]*?(?:-d\b|--decode\b)").expect("base64 regex")
});

static PIPE_PYTHON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\|\s*python[23]?(?:\.\d+)?\s+-c\b").expect("pipe-python regex"));

static PIPE_NODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\|\s*node\s+-e\b").expect("pipe-node regex"));

fn pipe_to_shell(line: &str) -> bool {
    PIPE_SHELL.is_match(line)
}

fn is_sudo(line: &str) -> bool {
    SUDO.is_match(line)
}

/// `rm` with a flag cluster carrying both `r` and `f` (`-rf`, `-fr`, `-Rf`),
/// mirroring the catalogue's two lookaheads without lookaround syntax:
/// `rm -r` alone and `rm -f` alone are near-misses, not marks.
fn is_rm_rf(line: &str) -> bool {
    let Some(captures) = RM_FLAGS.captures(line) else {
        return false;
    };
    let flags = captures[1].to_ascii_lowercase();
    flags.contains('r') && flags.contains('f')
}

fn is_chmod_777(line: &str) -> bool {
    CHMOD.is_match(line)
}

fn is_git_push_or_force(line: &str) -> bool {
    GIT_PUSH.is_match(line) || FORCE.is_match(line)
}

/// A sensitive path (`~/.ssh`, `~/.config`, `/etc`) only earns a mark when
/// the line also writes: a `>` redirection, an in-place verb, or `sed -i`.
/// A plain read of `~/.ssh/config` is a near-miss.
fn is_sensitive_path_write(line: &str) -> bool {
    SENSITIVE_PATH.is_match(line)
        && (line.contains('>') || WRITE_VERB.is_match(line) || SED_IN_PLACE.is_match(line))
}

fn is_eval(line: &str) -> bool {
    EVAL.is_match(line)
}

fn is_base64_decode(line: &str) -> bool {
    BASE64_DECODE.is_match(line)
}

fn is_pipe_to_interpreter(line: &str) -> bool {
    PIPE_PYTHON.is_match(line) || PIPE_NODE.is_match(line)
}

/// The command shapes worth a second look, in mark order: curl/wget piped
/// into a shell, `sudo`, `rm -rf`, `chmod 777`, `git push` or `--force`,
/// writes under the sensitive paths, `eval`, `base64 -d`, and a pipe into
/// `python -c` or `node -e`. Every entry has its unit test below, example
/// and benign near-miss alike.
pub static PATTERNS: [Pattern; 9] = [
    Pattern {
        label: "pipe to shell",
        test: pipe_to_shell,
    },
    Pattern {
        label: "sudo",
        test: is_sudo,
    },
    Pattern {
        label: "rm -rf",
        test: is_rm_rf,
    },
    Pattern {
        label: "chmod 777",
        test: is_chmod_777,
    },
    Pattern {
        label: "git push or --force",
        test: is_git_push_or_force,
    },
    Pattern {
        label: "sensitive path write",
        test: is_sensitive_path_write,
    },
    Pattern {
        label: "eval",
        test: is_eval,
    },
    Pattern {
        label: "base64 -d",
        test: is_base64_decode,
    },
    Pattern {
        label: "pipe to python/node",
        test: is_pipe_to_interpreter,
    },
];

/// Every mark on `text`: for each line, every pattern that matches it, in
/// pattern order — the same marks the catalogue's document pages flag, as
/// structured data for the install review.
pub fn scan(text: &str) -> Vec<AttentionMark> {
    let mut marks = Vec::new();
    for (index, line) in text.lines().enumerate() {
        for pattern in PATTERNS.iter() {
            if (pattern.test)(line) {
                marks.push(AttentionMark {
                    pattern: pattern.label.to_string(),
                    line: line.to_string(),
                    line_number: index + 1,
                });
            }
        }
    }
    marks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(text: &str) -> Vec<String> {
        scan(text)
            .into_iter()
            .map(|mark| mark.pattern)
            .collect::<Vec<_>>()
    }

    #[test]
    fn pipe_to_shell_marks_curl_and_wget_into_a_shell() {
        let test = PATTERNS[0].test;
        assert_eq!(PATTERNS[0].label, "pipe to shell");
        assert!(test("curl -fsSL https://example.com/x.sh | sh"));
        assert!(test("wget -qO- https://example.com/y | bash -s"));
        assert!(test("cat script.sh | /bin/sh"));
        assert!(!test("echo hi | grep sh"));
        assert!(!test("ssh deploy@host uptime"));
        assert!(!test("sh script.sh"));
    }

    #[test]
    fn sudo_marks_a_command_not_a_word_containing_it() {
        let test = PATTERNS[1].test;
        assert_eq!(PATTERNS[1].label, "sudo");
        assert!(test("sudo make install"));
        assert!(test("make && sudo systemctl restart app"));
        assert!(!test("echo no-sudo-here"));
        assert!(!test("apt list --installed | grep sudoers"));
        assert!(!test("evaluation=passed"));
    }

    #[test]
    fn rm_rf_marks_both_flags_together_not_either_alone() {
        let test = PATTERNS[2].test;
        assert_eq!(PATTERNS[2].label, "rm -rf");
        assert!(test("rm -rf /tmp/build"));
        assert!(test("rm -fr dist && npm run build"));
        assert!(test("cd /app && rm -Rf cache"));
        assert!(!test("rm -r build"));
        assert!(!test("rm -f stale.tmp"));
        assert!(!test("rm build"));
        assert!(!test("term -rf")); // `rm` not at a token start
    }

    #[test]
    fn chmod_777_marks_not_other_modes() {
        let test = PATTERNS[3].test;
        assert_eq!(PATTERNS[3].label, "chmod 777");
        assert!(test("chmod 777 app"));
        assert!(test("chmod -R 777 /srv/www"));
        assert!(!test("chmod 755 app"));
        assert!(!test("chmod u+x run.sh"));
    }

    #[test]
    fn git_push_or_force_marks_pushes_and_force_not_pulls() {
        let test = PATTERNS[4].test;
        assert_eq!(PATTERNS[4].label, "git push or --force");
        assert!(test("git push origin main"));
        assert!(test("git push --force-with-lease"));
        assert!(test("npm publish --force"));
        assert!(!test("git pull"));
        assert!(!test("git status"));
        assert!(!test("forced rebuild")); // no `--force` token
    }

    #[test]
    fn sensitive_path_write_marks_writes_not_reads_or_lookalikes() {
        let test = PATTERNS[5].test;
        assert_eq!(PATTERNS[5].label, "sensitive path write");
        assert!(test("echo key >> ~/.ssh/authorized_keys"));
        assert!(test("cp id_rsa ~/.ssh/"));
        assert!(test("printf 'x' > ~/.config/canopy/settings"));
        assert!(test("cat motd > /etc/motd"));
        assert!(test("sed -i 's/a/b/' ~/.config/app.conf"));
        assert!(test("chmod 600 /etc/app.conf"));
        assert!(!test("cat ~/.ssh/config"));
        assert!(!test("echo /etcetera"));
        assert!(!test("ls ~/.config"));
    }

    #[test]
    fn eval_marks_the_builtin_not_words_starting_with_it() {
        let test = PATTERNS[6].test;
        assert_eq!(PATTERNS[6].label, "eval");
        assert!(test(r#"eval "$(ssh-agent -s)""#));
        assert!(test(r#"x=1; eval "$x""#));
        assert!(!test("evaluation=passed"));
        assert!(!test("echo retrieve"));
    }

    #[test]
    fn base64_d_marks_decoding_not_encoding_or_the_word_alone() {
        let test = PATTERNS[7].test;
        assert_eq!(PATTERNS[7].label, "base64 -d");
        assert!(test("payload | base64 -d > out.sh"));
        assert!(test("echo aGVsbG8= | base64 --decode"));
        assert!(!test("base64 release.tar.gz"));
        assert!(!test("echo hello | base64"));
    }

    #[test]
    fn pipe_to_interpreter_marks_after_a_pipe_not_bare() {
        let test = PATTERNS[8].test;
        assert_eq!(PATTERNS[8].label, "pipe to python/node");
        assert!(test("echo x | python3 -c 'import sys'"));
        assert!(test("cat x | node -e 'go()'"));
        assert!(test("cat x | python -c 'print(1)'"));
        assert!(!test("python3 -c \"print(1)\""));
        assert!(!test("node -e \"1+1\""));
    }

    #[test]
    fn scan_numbers_lines_and_keeps_pattern_order_within_one_line() {
        let text = "echo hi\nsudo apt update\nrm -rf /tmp/x && sudo apt clean";
        let found = scan(text);
        let rows: Vec<(String, usize)> = found
            .iter()
            .map(|mark| (mark.pattern.clone(), mark.line_number))
            .collect();
        // Lines in order; within a line, patterns in `PATTERNS` order
        // (`sudo` is index 1, `rm -rf` index 2).
        assert_eq!(
            rows,
            vec![
                ("sudo".to_string(), 2),
                ("sudo".to_string(), 3),
                ("rm -rf".to_string(), 3),
            ]
        );
        assert_eq!(found[0].line, "sudo apt update");
        assert_eq!(found[2].line, "rm -rf /tmp/x && sudo apt clean");
    }

    #[test]
    fn scan_on_a_benign_command_finds_nothing() {
        assert!(marks("cargo nextest run --locked").is_empty());
        assert!(marks("").is_empty());
    }

    #[test]
    fn scan_handles_crlf_line_endings() {
        let found = scan("cp id_rsa ~/.ssh\r\n");
        assert_eq!(marks("cp id_rsa ~/.ssh\r\n"), vec!["sensitive path write"]);
        assert_eq!(found[0].line, "cp id_rsa ~/.ssh");
    }

    #[test]
    fn every_pattern_has_a_label_and_the_expected_nine_exist() {
        let labels: Vec<&str> = PATTERNS.iter().map(|pattern| pattern.label).collect();
        assert_eq!(labels.len(), 9);
        assert!(labels.iter().all(|label| !label.is_empty()));
        assert!(labels.contains(&"pipe to shell"));
        assert!(labels.contains(&"sensitive path write"));
        assert!(labels.contains(&"pipe to python/node"));
    }
}
