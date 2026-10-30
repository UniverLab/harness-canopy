//! System clipboard helper, shared by the TUI selection copy and the OSC 52
//! forwarder that mirrors a PTY program's clipboard writes to the host.
//!
//! The prompt builder also pastes *images* from the clipboard (`Ctrl+V` /
//! `Alt+V`) and attaches dropped image *paths* from a bracketed paste (see
//! `parse_image_path_paste`).

use std::path::{Path, PathBuf};

/// Write `text` to the system clipboard. Fire-and-forget: failures (no display
/// server, unsupported platform) are ignored, matching the rest of the TUI.
pub(crate) fn set_text(text: &str) {
    let _ = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text.to_owned()));
}

/// Something the builder can read a pasted image from: PNG bytes, or `None`
/// when the clipboard holds no image. Behind a trait so tests stub it and
/// never touch the real clipboard.
pub(crate) trait ImageClipboard {
    fn read_image(&self) -> Option<Vec<u8>>;
}

/// The live clipboard: `arboard`'s image, PNG-encoded. Under WSL `arboard`
/// cannot see the Windows clipboard, so a `None` there falls through to the
/// PowerShell reader (`spawn_wsl_clipboard_fetch`), never to text.
pub(crate) struct SystemClipboard;

impl ImageClipboard for SystemClipboard {
    fn read_image(&self) -> Option<Vec<u8>> {
        let image = arboard::Clipboard::new().ok()?.get_image().ok()?;
        encode_png(image.width, image.height, &image.bytes)
    }
}

/// PNG-encode raw `RGBA8` pixels. `None` when the dimensions do not fit or
/// the encoder fails.
fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    use image::ImageEncoder as _;
    let (width, height) = (u32::try_from(width).ok()?, u32::try_from(height).ok()?);
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(png)
}

/// Whether this Linux process runs under WSL, where the PowerShell fallback
/// applies.
pub(crate) fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version")
        .is_ok_and(|version| version.to_ascii_lowercase().contains("microsoft"))
}

/// Read the Windows clipboard image via PowerShell, off-thread: starting
/// `powershell.exe` takes a second and must never stall the UI. The script
/// prints base64 PNG to stdout so no temp file has to cross the
/// Linux/Windows boundary; the channel carries `None` when there is no
/// image (or PowerShell itself fails).
pub(crate) fn spawn_wsl_clipboard_fetch(sender: std::sync::mpsc::Sender<Option<Vec<u8>>>) {
    std::thread::spawn(move || {
        let _ = sender.send(read_wsl_clipboard_image());
    });
}

fn read_wsl_clipboard_image() -> Option<Vec<u8>> {
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", WSL_CLIPBOARD_PS])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let encoded = String::from_utf8(output.stdout).ok()?;
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()
        .filter(|bytes| !bytes.is_empty())
}

/// `Get-Clipboard -Format Image`, PNG-encoded into a base64 line on stdout;
/// exit 1 when the clipboard holds no image.
const WSL_CLIPBOARD_PS: &str = "Add-Type -AssemblyName System.Drawing; \
    $img = Get-Clipboard -Format Image; \
    if ($null -eq $img) { exit 1 }; \
    $ms = New-Object IO.MemoryStream; \
    $img.Save($ms, [Drawing.Imaging.ImageFormat]::Png); \
    [Convert]::ToBase64String($ms.ToArray())";

/// Resolve one pasted line to an absolute path: a bare absolute path, a
/// `file://` URI (percent-decoded), or — under WSL only — a Windows path
/// mapped onto `/mnt/<drive>/…`. Surrounding quotes are stripped. `None`
/// for anything else (relative paths, other schemes, `C:\…` off WSL).
pub(crate) fn candidate_to_path(candidate: &str) -> Option<PathBuf> {
    candidate_to_path_impl(candidate, is_wsl())
}

fn candidate_to_path_impl(candidate: &str, wsl: bool) -> Option<PathBuf> {
    let candidate = unquote(candidate.trim());
    if candidate.is_empty() {
        return None;
    }
    if let Some(path) = strip_file_uri(candidate) {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = map_windows_path(candidate, wsl) {
        return Some(path);
    }
    let path = PathBuf::from(candidate);
    path.is_absolute().then_some(path)
}

/// Parse a bracketed paste as dropped image paths: every non-empty line must
/// resolve to an existing `PNG`/`JPEG`/`WebP`/`GIF` file, else the whole
/// paste stays text (empty vec).
pub(crate) fn parse_image_path_paste(text: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Some(path) = candidate_to_path(line) else {
            return Vec::new();
        };
        if !has_image_extension(&path) || !path.is_file() {
            return Vec::new();
        }
        paths.push(path);
    }
    paths
}

fn unquote(candidate: &str) -> &str {
    let bytes = candidate.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &candidate[1..candidate.len() - 1];
        }
    }
    candidate
}

/// Strip a `file://` prefix (plus an optional `localhost` authority) and
/// percent-decode the rest. `None` for non-`file://` candidates.
fn strip_file_uri(candidate: &str) -> Option<String> {
    let rest = candidate.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    Some(percent_decode(rest))
}

/// Decode ASCII `%XX` escapes, leaving malformed escapes literal.
fn percent_decode(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%')
            .then(|| {
                let h = hex_value(*bytes.get(i + 1)?)?;
                let l = hex_value(*bytes.get(i + 2)?)?;
                Some(h * 16 + l)
            })
            .flatten();
        match decoded {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Map `C:\…` / `C:/…` onto `/mnt/c/…` under WSL; `None` elsewhere.
fn map_windows_path(candidate: &str, wsl: bool) -> Option<PathBuf> {
    if !wsl {
        return None;
    }
    let bytes = candidate.as_bytes();
    let drive = *bytes.first()?;
    if !drive.is_ascii_alphabetic() || bytes.get(1) != Some(&b':') {
        return None;
    }
    let unix = candidate[2..]
        .trim_start_matches(['\\', '/'])
        .replace('\\', "/");
    Some(PathBuf::from(format!(
        "/mnt/{}/{}",
        char::from(drive).to_ascii_lowercase(),
        unix
    )))
}

fn has_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_named(dir: &tempfile::TempDir, name: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, b"fake image bytes").expect("write fixture");
        path
    }

    #[test]
    fn encode_png_produces_png_magic() {
        let png = encode_png(1, 1, &[255, 0, 0, 255]).expect("encodes");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));
    }

    #[test]
    fn path_paste_attaches_a_linux_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_named(&dir, "shot.png");

        let paths = parse_image_path_paste(&path.to_string_lossy());

        assert_eq!(paths, vec![path]);
    }

    #[test]
    fn path_paste_attaches_a_file_uri_with_escapes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_named(&dir, "my shot.png");
        let uri = format!("file://{}", path.to_string_lossy().replace(' ', "%20"));

        let paths = parse_image_path_paste(&uri);

        assert_eq!(paths, vec![path]);
    }

    #[test]
    fn path_paste_attaches_quoted_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_named(&dir, "shot.jpg");

        let paths = parse_image_path_paste(&format!("\"{}\"", path.display()));

        assert_eq!(paths, vec![path]);
    }

    #[test]
    fn path_paste_attaches_several_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = write_named(&dir, "a.png");
        let second = write_named(&dir, "b.webp");
        let text = format!("{}\n{}\n", first.display(), second.display());

        let paths = parse_image_path_paste(&text);

        assert_eq!(paths, vec![first, second]);
    }

    #[test]
    fn path_paste_rejects_a_non_image_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_named(&dir, "notes.txt");

        assert!(parse_image_path_paste(&path.to_string_lossy()).is_empty());
    }

    #[test]
    fn path_paste_rejects_a_mixed_paste() {
        let dir = tempfile::tempdir().expect("tempdir");
        let image = write_named(&dir, "a.png");
        let text = write_named(&dir, "b.txt");
        let paste = format!("{}\n{}", image.display(), text.display());

        assert!(parse_image_path_paste(&paste).is_empty());
    }

    #[test]
    fn path_paste_rejects_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("gone.png");

        assert!(parse_image_path_paste(&missing.to_string_lossy()).is_empty());
    }

    #[test]
    fn path_paste_rejects_plain_text() {
        assert!(parse_image_path_paste("just some words").is_empty());
        assert!(parse_image_path_paste("relative/path.png").is_empty());
        assert!(parse_image_path_paste("").is_empty());
    }

    #[test]
    fn windows_path_maps_onto_mnt_only_under_wsl() {
        assert_eq!(
            candidate_to_path_impl("C:\\Users\\u\\pic.png", true),
            Some(PathBuf::from("/mnt/c/Users/u/pic.png"))
        );
        assert_eq!(
            candidate_to_path_impl("D:/shots/a.JPG", true),
            Some(PathBuf::from("/mnt/d/shots/a.JPG"))
        );
        assert_eq!(candidate_to_path_impl("C:\\Users\\u\\pic.png", false), None);
    }
}
