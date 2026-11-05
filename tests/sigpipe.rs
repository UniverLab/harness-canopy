//! A short-lived CLI process must die by SIGPIPE, not panic, when the reader
//! of its stdout goes away. The child re-executes this test binary in a special
//! mode so the same signal setup the binary performs is exercised end to end.

#[cfg(unix)]
mod unix {
    use std::io::Read;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};

    const CHILD_ENV: &str = "CANOPY_SIGPIPE_CHILD";

    /// Runs inside the spawned child: restore the default disposition, then
    /// print until the closed pipe kills us.
    #[test]
    fn child_mode_prints_until_sigpipe() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        // SAFETY: default disposition only; the child exists to be killed.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
        loop {
            println!("x");
        }
    }

    #[test]
    fn dies_by_sigpipe_when_reader_closes() {
        let exe = std::env::current_exe().expect("current test executable");
        let mut child = Command::new(exe)
            .env(CHILD_ENV, "1")
            .args(["child_mode_prints_until_sigpipe", "--nocapture"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn child test process");

        let mut stdout = child.stdout.take().expect("child stdout pipe");
        let mut first = [0u8; 1];
        stdout
            .read_exact(&mut first)
            .expect("child must write to stdout before the reader closes");
        drop(stdout);

        let output = child.wait_with_output().expect("wait for child");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGPIPE),
            "expected termination by SIGPIPE, status: {:?}, stderr: {stderr}",
            output.status
        );
        assert!(
            !stderr.contains("panicked"),
            "child panicked instead of dying by SIGPIPE: {stderr}"
        );
    }
}

#[cfg(not(unix))]
#[test]
fn sigpipe_only_exists_on_unix() {
    // Windows has no SIGPIPE; `restore_default_sigpipe` compiles to nothing.
}
