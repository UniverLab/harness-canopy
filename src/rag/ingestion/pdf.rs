use super::*;

/// Internal helper command used to isolate PDF parsing in a subprocess.
/// This prevents parser-level stack overflows from taking down the daemon.
pub fn run_internal_pdf_extract(path: &Path) -> anyhow::Result<()> {
    let text = extract_pdf_text_in_process(path)?;
    print!("{text}");
    Ok(())
}

pub(super) fn extract_pdf_text(path: &Path) -> anyhow::Result<String> {
    let exe = std::env::current_exe()?;
    let exe_display = exe.display().to_string();
    let output = std::process::Command::new(&exe)
        .arg("internal-pdf-extract")
        .arg(path)
        .output()
        .map_err(|e| {
            anyhow::anyhow!("Failed to launch PDF extraction subprocess ({exe_display}): {e}")
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("subprocess exited with status {}", output.status)
        } else {
            stderr
        };
        anyhow::bail!(
            "PDF extraction subprocess failed for {}: {detail}",
            path.display()
        );
    }

    String::from_utf8(output.stdout).map_err(|e| {
        anyhow::anyhow!(
            "PDF extraction subprocess returned non-UTF8 output for {}: {e}",
            path.display()
        )
    })
}

pub(super) fn extract_pdf_text_in_process(path: &Path) -> anyhow::Result<String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    // Detect actual file type by magic bytes — .pdf files are sometimes HTML
    // error pages or other content saved with the wrong extension.
    if is_html_bytes(&buffer) {
        let html = String::from_utf8_lossy(&buffer);
        let text = strip_html_to_text(&html);
        if text.trim().is_empty() {
            anyhow::bail!(
                "File '{}' appears to be HTML but contains no extractable text",
                path.display()
            );
        }
        tracing::info!(
            "RAG: '{}' detected as HTML (not PDF) — extracted {} bytes via HTML stripper",
            path.display(),
            text.len()
        );
        return Ok(text);
    }

    if !buffer.starts_with(b"%PDF") {
        // Unknown binary — attempt pdf-extract anyway, fall back to raw salvage.
        tracing::warn!(
            "RAG: '{}' missing PDF magic bytes — attempting pdf-extract with raw-text fallback",
            path.display()
        );
        return pdf_extract::extract_text_from_mem(&buffer)
            .map_err(|_| ())
            .or_else(|_| {
                let text = salvage_printable_text(&buffer);
                if text.split_whitespace().count() >= 20 {
                    Ok(text)
                } else {
                    Err(())
                }
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "Could not extract text from '{}': not a valid PDF or recognisable text file",
                    path.display()
                )
            });
    }

    pdf_extract::extract_text_from_mem(&buffer)
        .map_err(|e| anyhow::anyhow!("PDF text extraction failed for {}: {e}", path.display()))
}

/// Returns `true` when the byte slice looks like an HTML document.
pub(super) fn is_html_bytes(bytes: &[u8]) -> bool {
    // Skip a leading UTF-8 BOM if present.
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let prefix = &bytes[..bytes.len().min(64)];
    let lower: Vec<u8> = prefix.iter().map(|b| b.to_ascii_lowercase()).collect();
    lower.starts_with(b"<!doctype")
        || lower.starts_with(b"<html")
        || lower.windows(6).any(|w| w == b"<html ")
}

/// Very simple HTML-to-text: strips tags, decodes common entities, collapses whitespace.
pub(super) fn strip_html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut state = HtmlStripState::new();
    let mut chars = html.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '&' && !state.in_tag && !state.in_script {
            decode_html_entity(&mut chars, &mut out);
            state.pending_space = false;
        } else {
            state.process_char(ch, &mut out);
        }
    }

    collapse_blank_lines(&out)
}

pub(super) struct HtmlStripState {
    in_tag: bool,
    in_script: bool,
    tag_buf: String,
    pending_space: bool,
}

impl HtmlStripState {
    fn new() -> Self {
        Self {
            in_tag: false,
            in_script: false,
            tag_buf: String::new(),
            pending_space: false,
        }
    }

    fn process_char(&mut self, ch: char, out: &mut String) {
        if self.in_tag {
            self.handle_tag_char(ch, out);
        } else if ch == '<' {
            self.in_tag = true;
            self.tag_buf.clear();
            self.tag_buf.push(ch);
        } else if self.in_script {
            // skip script/style content
        } else if ch.is_whitespace() {
            self.pending_space = true;
        } else {
            self.flush_pending_space(out);
            out.push(ch);
        }
    }

    fn handle_tag_char(&mut self, ch: char, out: &mut String) {
        self.tag_buf.push(ch);
        if ch != '>' {
            return;
        }

        let tag_name = extract_tag_name(&self.tag_buf);
        self.in_script = matches!(tag_name.as_str(), "script" | "style");

        if is_block_level_tag(&tag_name) {
            out.push('\n');
            self.pending_space = false;
        }

        self.tag_buf.clear();
        self.in_tag = false;
    }

    fn flush_pending_space(&mut self, out: &mut String) {
        if self.pending_space && !out.ends_with('\n') {
            out.push(' ');
        }
        self.pending_space = false;
    }
}

pub(super) fn decode_html_entity(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    out: &mut String,
) {
    let mut entity = String::new();
    for ec in chars.by_ref() {
        if ec == ';' {
            break;
        }
        entity.push(ec);
        if entity.len() > 8 {
            break;
        }
    }
    out.push_str(decode_entity(&entity));
}

pub(super) fn decode_entity(entity: &str) -> &'static str {
    match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "nbsp" | "#160" => " ",
        "quot" => "\"",
        "apos" | "#39" => "'",
        _ => " ",
    }
}

pub(super) fn extract_tag_name(tag_buf: &str) -> String {
    let tag_lower = tag_buf.to_ascii_lowercase();
    tag_lower
        .trim_start_matches('<')
        .trim_start_matches('/')
        .split(|c: char| c.is_whitespace() || c == '>')
        .next()
        .unwrap_or("")
        .to_owned()
}

pub(super) fn is_block_level_tag(tag_name: &str) -> bool {
    matches!(
        tag_name,
        "p" | "div"
            | "br"
            | "li"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "tr"
            | "td"
            | "th"
            | "blockquote"
            | "section"
            | "article"
    )
}

pub(super) fn collapse_blank_lines(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut blank_run = 0usize;
    for line in input.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                result.push('\n');
            }
        } else {
            blank_run = 0;
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

/// Last-resort text salvage: collect printable ASCII / Unicode runs from raw bytes.
/// Only accepts runs of at least 4 consecutive printable chars to filter binary noise.
pub(super) fn salvage_printable_text(bytes: &[u8]) -> String {
    const MIN_RUN: usize = 4;
    let mut runs: Vec<String> = Vec::new();
    let mut current = String::new();

    for &b in bytes {
        if (0x20..0x7f).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t' {
            current.push(b as char);
        } else {
            if current.trim().len() >= MIN_RUN {
                runs.push(current.trim().to_owned());
            }
            current.clear();
        }
    }
    if current.trim().len() >= MIN_RUN {
        runs.push(current.trim().to_owned());
    }

    runs.join(" ")
}
