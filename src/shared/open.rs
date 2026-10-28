//! The system opener: hand a shelf file to the OS default viewer.
//!
//! Opener selection is pure ([`select_opener`], [`wsl_from`]) so unit tests
//! can cover every branch without touching the real `PATH` or `/proc`; only
//! [`open_path`] performs I/O.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

/// Which system command shows a path with the OS default viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opener {
    /// `wslview` (wslu) — the usual "open in Windows" wrapper under WSL.
    WslView,
    /// `explorer.exe`, with the path translated through `wslpath -w`.
    ExplorerWslpath,
    /// macOS `open`.
    MacOpen,
    /// Linux `xdg-open`.
    XdgOpen,
}

impl Opener {
    /// The command name to spawn for this opener.
    fn command(&self) -> &'static str {
        match self {
            Opener::WslView => "wslview",
            Opener::ExplorerWslpath => "explorer.exe",
            Opener::MacOpen => "open",
            Opener::XdgOpen => "xdg-open",
        }
    }

    /// Whether the argument must first be translated to a Windows path —
    /// true only for [`Opener::ExplorerWslpath`].
    fn needs_wslpath(&self) -> bool {
        matches!(self, Opener::ExplorerWslpath)
    }
}

/// Pick the opener for the host: macOS first (never WSL), then the WSL
/// pair — `wslview` when it is on `PATH`, `explorer.exe` + `wslpath`
/// otherwise — then plain `xdg-open`.
pub(crate) fn select_opener(os: &str, is_wsl: bool, wslview_available: bool) -> Opener {
    if os == "macos" {
        return Opener::MacOpen;
    }
    match (is_wsl, wslview_available) {
        (true, true) => Opener::WslView,
        (true, false) => Opener::ExplorerWslpath,
        (false, _) => Opener::XdgOpen,
    }
}

/// The pure half of [`is_wsl`]: WSL when the `WSLInterop` binfmt file
/// exists or `WSL_DISTRO_NAME` carries a non-empty distro name.
fn wsl_from(interop_exists: bool, distro_name: Option<&str>) -> bool {
    interop_exists || distro_name.is_some_and(|name| !name.is_empty())
}

/// True when this machine is WSL: the `/proc/sys/fs/binfmt_misc/WSLInterop`
/// registration exists, or `WSL_DISTRO_NAME` is set and non-empty.
fn is_wsl() -> bool {
    let interop = Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists();
    let distro = std::env::var("WSL_DISTRO_NAME").ok();
    wsl_from(interop, distro.as_deref())
}

/// True when `wslview` resolves on `PATH` as an executable file.
fn wslview_available() -> bool {
    which::which("wslview").is_ok()
}

/// `wslpath -w <path>`: the Windows spelling of `path`, or `None` when
/// `wslpath` is missing, exits non-zero, or prints nothing.
fn windows_path(path: &Path) -> Option<OsString> {
    let output = Command::new("wslpath").arg("-w").arg(path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let translated = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if translated.is_empty() {
        None
    } else {
        Some(OsString::from(translated))
    }
}

/// Open `path` with the OS default viewer, spawned detached (own session,
/// null stdio) so the viewer outlives this process.
///
/// Never panics: every failure — including a missing opener — is an `Err`
/// whose message names `path`.
pub fn open_path(path: &Path) -> anyhow::Result<()> {
    let opener = select_opener(std::env::consts::OS, is_wsl(), wslview_available());
    let arg: OsString = if opener.needs_wslpath() {
        windows_path(path).unwrap_or_else(|| path.into())
    } else {
        path.into()
    };

    let mut cmd = Command::new(opener.command());
    cmd.arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    match cmd.spawn() {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Err(anyhow::anyhow!("no system opener found for {path:?}"))
        }
        Err(err) => Err(anyhow::anyhow!(
            "failed to open {path:?} with {}: {err}",
            opener.command()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_opener_picks_macos_before_wsl_flags() {
        assert_eq!(select_opener("macos", false, false), Opener::MacOpen);
        assert_eq!(select_opener("macos", true, true), Opener::MacOpen);
    }

    #[test]
    fn select_opener_picks_xdg_open_on_plain_linux() {
        assert_eq!(select_opener("linux", false, false), Opener::XdgOpen);
    }

    #[test]
    fn select_opener_picks_wslview_when_wsl_and_available() {
        assert_eq!(select_opener("linux", true, true), Opener::WslView);
    }

    #[test]
    fn select_opener_picks_explorer_when_wsl_without_wslview() {
        assert_eq!(select_opener("linux", true, false), Opener::ExplorerWslpath);
    }

    #[test]
    fn opener_command_names_match_the_tools() {
        assert_eq!(Opener::WslView.command(), "wslview");
        assert_eq!(Opener::ExplorerWslpath.command(), "explorer.exe");
        assert_eq!(Opener::MacOpen.command(), "open");
        assert_eq!(Opener::XdgOpen.command(), "xdg-open");
    }

    #[test]
    fn only_explorer_needs_wslpath() {
        assert!(Opener::ExplorerWslpath.needs_wslpath());
        assert!(!Opener::WslView.needs_wslpath());
        assert!(!Opener::MacOpen.needs_wslpath());
        assert!(!Opener::XdgOpen.needs_wslpath());
    }

    #[test]
    fn wsl_from_true_when_interop_file_exists() {
        assert!(wsl_from(true, None));
        assert!(wsl_from(true, Some("Ubuntu")));
    }

    #[test]
    fn wsl_from_requires_a_signal() {
        assert!(wsl_from(false, Some("Ubuntu")));
        assert!(!wsl_from(false, Some("")));
        assert!(!wsl_from(false, None));
    }

    #[test]
    fn open_path_reports_errors_without_panicking() {
        let path = Path::new("/definitely/not/a/real/shelf-file-42.md");
        // A panic here fails the test outright; open_path may only return.
        // Whether the host opener exists is environment-dependent, so the
        // error string is only asserted when an error is actually produced.
        match open_path(path) {
            Ok(()) => {}
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains(path.to_string_lossy().as_ref()),
                    "error message {message:?} must contain {path:?}"
                );
            }
        }
    }
}
