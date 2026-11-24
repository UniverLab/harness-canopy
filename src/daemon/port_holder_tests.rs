use super::*;

#[test]
fn holder_message_names_pid_command_and_remedy() {
    assert_eq!(
        holder_message(8080, Some(1234), &|_| "mydaemon --serve".to_string()),
        "port 8080 is held by pid 1234 (mydaemon --serve), which this daemon did not start; \
         run canopy daemon stop to stop it, or pick another port with --port"
    );
}

#[test]
fn holder_message_reports_unknown_command() {
    assert_eq!(
        holder_message(8080, Some(1234), &|_| "unknown command".to_string()),
        "port 8080 is held by pid 1234 (unknown command), which this daemon did not start; \
         run canopy daemon stop to stop it, or pick another port with --port"
    );
}

#[test]
fn holder_message_without_pid_blames_unidentifiable_holder() {
    assert_eq!(
        holder_message(8080, None, &|_| panic!("no occupant, no command lookup")),
        "port 8080 is in use by a process that could not be identified \
         (likely another user's); pick another port with --port"
    );
}

#[test]
fn holder_command_returns_something_for_the_current_process() {
    assert!(!holder_command(std::process::id()).is_empty());
}

#[test]
fn port_in_use_holds_only_while_the_listener_lives() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();
    assert!(port_in_use(port));
    drop(listener);
    assert!(!port_in_use(port));
}

#[test]
fn start_decision_covers_the_whole_start_table() {
    // Free port: start.
    assert_eq!(
        start_decision(None, false, false, None),
        StartDecision::Start
    );
    // Believed pid-file daemon: already running, never touch the port.
    assert_eq!(
        start_decision(None, false, true, None),
        StartDecision::AlreadyRunning
    );
    assert_eq!(
        start_decision(Some(9), true, true, Some(7)),
        StartDecision::AlreadyRunning
    );
    // The service manager's own MainPID holds the port: already running.
    assert_eq!(
        start_decision(Some(9), true, false, Some(9)),
        StartDecision::AlreadyRunning
    );
    // A foreign pid holds the port: refuse, naming the holder.
    assert_eq!(
        start_decision(Some(9), true, false, None),
        StartDecision::Refuse
    );
    assert_eq!(
        start_decision(Some(9), true, false, Some(7)),
        StartDecision::Refuse
    );
    // Nobody resolvable, but the bind probe says taken (other user's
    // process): refuse all the same.
    assert_eq!(
        start_decision(None, true, false, None),
        StartDecision::Refuse
    );
}

#[test]
fn bind_refusal_passes_through_errors_other_than_addr_in_use() {
    let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    assert_eq!(
        bind_refusal(8080, &err, Some(9), &|_| "cmd".to_string()),
        None
    );
}

/// The serve bind-error path end to end: a real listener holds the port,
/// the refusal names this test process's own pid, and the listener is
/// still accepting connections afterwards — nothing was signalled.
#[cfg(target_os = "linux")]
#[test]
fn bind_error_path_names_holder_and_leaves_listener_alive() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("port").port();

    let err = std::net::TcpListener::bind(("127.0.0.1", port)).expect_err("port is held");
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);

    let occupant = crate::daemon::process::resolve_port_pid(port);
    assert_eq!(occupant, Some(std::process::id()));

    let refusal =
        bind_refusal(port, &err, occupant, &holder_command).expect("AddrInUse must refuse");
    let own_pid = std::process::id().to_string();
    assert!(
        refusal.contains(&own_pid),
        "refusal must name the holder, got: {refusal}"
    );
    assert!(
        refusal.contains("which this daemon did not start"),
        "refusal must disown the holder, got: {refusal}"
    );

    let addr = listener.local_addr().expect("port");
    std::net::TcpStream::connect(addr).expect("listener still accepts afterwards");
}

/// F-13 guard: the only terminating-signal call sites left in process.rs are
/// the signal-0 liveness probe, the explicit-stop helper, and the process-
/// group helpers. A reintroduced `libc::kill(` anywhere else fails here.
#[test]
fn no_libc_kill_outside_approved_helpers() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/daemon/process.rs");
    let content = std::fs::read_to_string(&path).expect("read process.rs");
    let allowed = [
        "is_process_running",
        "send_signal",
        "send_signal_to_group",
        "terminate_process_group_async",
    ];
    let mut current_fn = String::from("<top>");
    let mut violations = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        let rest = trimmed
            .strip_prefix("pub(crate) fn ")
            .or_else(|| trimmed.strip_prefix("pub fn "))
            .or_else(|| trimmed.strip_prefix("async fn "))
            .or_else(|| trimmed.strip_prefix("fn "));
        if let Some(rest) = rest {
            current_fn = rest.split('(').next().unwrap_or("?").to_string();
        }
        if line.contains("libc::kill(") && !allowed.contains(&current_fn.as_str()) {
            violations.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
        }
    }
    assert!(
        violations.is_empty(),
        "terminating libc::kill outside approved helpers:\n{}",
        violations.join("\n")
    );
}
