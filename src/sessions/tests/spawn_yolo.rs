//! Spawn extension tests: the `yolo` switch (appended from the harness's
//! registry `yolo_flag`, refused without one and on terminals) and the
//! `workdir` rules (`~`/`~/x` expansion, absolute, existing directory).

use super::{agent_spawn, connect_client, recv_json, send_json, serve_state, test_db, test_hub};
use crate::domain::models::Cli;
use crate::sessions::catalog::{catalog_frame, CatalogData};
use crate::sessions::host_frame::HostFrame;
use crate::sessions::hub_command::build_spawn_command;
use crate::sessions::protocol::parse_host_frame;
use crate::sessions::ws::SessionsState;
use crate::sessions::{resolve_spawn_workdir, with_yolo};
use crate::tui::available_clis_for_catalog;

/// Send one spawn frame and return the `error` frame it answers.
async fn spawn_error(
    client: &mut super::WsStream,
    frame: serde_json::Value,
    req: &str,
) -> serde_json::Value {
    send_json(client, &frame).await;
    let error = recv_json(client, "spawn error").await;
    assert_eq!(error["t"], "error", "expected an error frame: {error}");
    assert_eq!(
        error["req"], req,
        "the error must echo the request: {error}"
    );
    error
}

/// A configured harness that defines a `yolo_flag`, with the flag itself.
/// `None` on a host with no registry to append from.
fn registry_cli_with_yolo() -> Option<(String, String)> {
    let (available, configs) = available_clis_for_catalog();
    available
        .into_iter()
        .zip(configs)
        .find_map(|(cli, config)| {
            let flag = config?.yolo_flag?;
            if flag.is_empty() {
                None
            } else {
                Some((cli.as_str().to_string(), flag))
            }
        })
}

#[tokio::test]
async fn ws_refuses_yolo_on_a_terminal() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    let error = spawn_error(
        &mut client,
        serde_json::json!({
            "t": "spawn", "req": "y-term", "kind": "terminal", "cli": "sh",
            "workdir": workdir.path().to_string_lossy(), "yolo": true,
        }),
        "y-term",
    )
    .await;
    assert_eq!(error["code"], "bad_request");
    assert_eq!(
        error["message"], "spawn: field 'yolo' is not allowed for kind 'terminal'",
        "must name the field and the kind: {error}"
    );
    server.abort();
}

#[tokio::test]
async fn ws_refuses_yolo_for_a_harness_without_a_flag() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    let error = spawn_error(
        &mut client,
        serde_json::json!({
            "t": "spawn", "req": "y-agent", "kind": "agent",
            "cli": "no-such-harness-xyz",
            "workdir": workdir.path().to_string_lossy(), "yolo": true,
        }),
        "y-agent",
    )
    .await;
    assert_eq!(error["code"], "bad_request");
    assert_eq!(
        error["message"], "spawn: harness 'no-such-harness-xyz' has no yolo mode",
        "must name the harness: {error}"
    );
    server.abort();
}

#[tokio::test]
async fn ws_refuses_a_relative_workdir() {
    let (db, _db_dir) = test_db();
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    let error = spawn_error(
        &mut client,
        serde_json::json!({
            "t": "spawn", "req": "w-rel", "kind": "agent", "cli": "cat",
            "workdir": "relative/dir",
        }),
        "w-rel",
    )
    .await;
    assert_eq!(error["code"], "bad_request");
    assert_eq!(
        error["message"], "spawn: field 'workdir' must be an absolute path",
        "must name the field: {error}"
    );
    server.abort();
}

#[tokio::test]
async fn ws_refuses_a_missing_workdir() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let missing = workdir.path().join("not-a-directory");
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    let error = spawn_error(
        &mut client,
        serde_json::json!({
            "t": "spawn", "req": "w-missing", "kind": "agent", "cli": "cat",
            "workdir": missing.to_string_lossy(),
        }),
        "w-missing",
    )
    .await;
    assert_eq!(error["code"], "bad_request");
    assert_eq!(
        error["message"],
        format!(
            "spawn: workdir '{}' does not exist or is not a directory",
            missing.display()
        ),
        "must name the path: {error}"
    );
    server.abort();
}

#[test]
fn yolo_flag_composition_is_unconditional() {
    // No registry needed: the helper pins the append the hub performs.
    assert_eq!(with_yolo(Some("--tui"), "--yolo"), "--tui --yolo");
    assert_eq!(with_yolo(None, "--yolo"), "--yolo");
    assert_eq!(with_yolo(Some(""), "--yolo"), "--yolo");
    assert_eq!(with_yolo(Some("  --tui  "), "  --yolo  "), "--tui --yolo");
    assert_eq!(
        with_yolo(Some("--tui"), "--agent auto-approve"),
        "--tui --agent auto-approve"
    );
}

#[test]
fn hub_leaves_args_untouched_without_yolo_even_for_unknown_harnesses() {
    // Pins the `req.yolo.then(...)` guard: without yolo the hub must never
    // look up the flag (an unknown harness has none, so an unconditional
    // lookup would panic on the `expect("validated")`).
    let workdir = tempfile::tempdir().expect("workdir");
    let mut req = agent_spawn("no-such-harness-xyz", &workdir.path().to_string_lossy());
    req.interactive_args = Some("--tui".to_string());
    let (_, args) = build_spawn_command(&req, "sess-plain", "plain-name");
    assert_eq!(args.as_deref(), Some("--tui"));
}

#[test]
fn hub_appends_the_harness_yolo_flag() {
    let Some((cli, flag)) = registry_cli_with_yolo() else {
        // No yolo-capable harness on this host: the composition and guard
        // tests above still pin the behaviour; the registry append needs a
        // real flag to append.
        return;
    };
    let workdir = tempfile::tempdir().expect("workdir");
    let mut req = agent_spawn(&cli, &workdir.path().to_string_lossy());
    req.interactive_args = Some("--tui".to_string());

    let (_, plain) = build_spawn_command(&req, "sess-plain", "plain-name");
    assert_eq!(
        plain.as_deref(),
        Some("--tui"),
        "without yolo the explicit args must stay untouched"
    );

    req.yolo = true;
    let (cmd, args) = build_spawn_command(&req, "sess-yolo", "yolo-name");
    let args = args.expect("agent args");
    assert_eq!(
        args,
        format!("--tui {flag}"),
        "the yolo flag must be appended after the explicit args"
    );
    let expected: Vec<String> = args.split_whitespace().map(str::to_string).collect();
    let argv: Vec<String> = cmd
        .get_argv()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        argv.ends_with(&expected),
        "the child argv {argv:?} must end with the yolo args {expected:?}"
    );

    // And on top of the registry's own interactive args when the frame sent
    // none: the expected string is the same composition the hub performs.
    let registry_interactive = Cli::new(cli.as_str())
        .registry_config()
        .and_then(|config| config.interactive_args);
    let mut req = agent_spawn(&cli, &workdir.path().to_string_lossy());
    req.yolo = true;
    let (_, args) = build_spawn_command(&req, "sess-registry", "registry-name");
    let expected = with_yolo(registry_interactive.as_deref(), &flag);
    assert_eq!(
        args.as_deref(),
        Some(expected.as_str()),
        "yolo must append to the registry args when the frame sent none"
    );
}

#[test]
fn catalog_yolo_matches_the_registry() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let canopy_dir = tempfile::tempdir().expect("canopy dir");
    let data = CatalogData::load(&db, workdir.path(), canopy_dir.path()).expect("loads");
    assert!(!data.clis.is_empty(), "the catalog must list CLIs");

    for row in &data.clis {
        let expected = Cli::new(row.cli.as_str())
            .registry_config()
            .is_some_and(|config| config.yolo_flag.is_some());
        assert_eq!(
            row.yolo, expected,
            "catalog yolo drifted from the registry for '{}'",
            row.cli
        );
    }

    // And the flag travels on the wire.
    let local: Vec<bool> = data.clis.iter().map(|cli| cli.yolo).collect();
    match parse_host_frame(&catalog_frame("cat-y", &data)) {
        Ok(HostFrame::Catalog { data, .. }) => {
            let wire: Vec<bool> = data.clis.iter().map(|cli| cli.yolo).collect();
            assert_eq!(wire, local, "catalog yolo must survive the wire");
        }
        other => panic!("expected a catalog frame, got {other:?}"),
    }
}

#[test]
fn catalog_yolo_absent_reads_as_false() {
    // Older hosts send no `yolo` key: the row must still parse as false.
    let row: crate::sessions::catalog::CatalogCli =
        serde_json::from_value(serde_json::json!({"cli": "c", "label": "c", "models": []}))
            .expect("a catalog row without yolo parses");
    assert!(!row.yolo, "a missing catalog yolo reads as false");
}

#[test]
fn workdir_tilde_alone_expands_to_the_home_directory() {
    let home = dirs::home_dir().expect("home directory");
    assert_eq!(
        resolve_spawn_workdir("~").expect("~ is the home directory"),
        home.to_string_lossy()
    );
    assert_eq!(
        resolve_spawn_workdir("~/").expect("~/ is the home directory"),
        home.to_string_lossy()
    );
}

#[test]
fn workdir_double_slash_stays_under_the_home_directory() {
    let home = dirs::home_dir().expect("home directory");
    assert_eq!(
        resolve_spawn_workdir("~//").expect("~// is the home directory"),
        home.to_string_lossy()
    );
    let expanded = home.join("canopy-no-such-dir-xyz");
    let error = resolve_spawn_workdir("~//canopy-no-such-dir-xyz").unwrap_err();
    assert_eq!(
        error,
        format!(
            "spawn: workdir '{}' does not exist or is not a directory",
            expanded.display()
        ),
        "~//x must expand under the home, not to the root"
    );
}

#[test]
fn workdir_tilde_child_expands_before_validation() {
    let expanded = dirs::home_dir()
        .expect("home directory")
        .join("canopy-no-such-dir-xyz");
    let error = resolve_spawn_workdir("~/canopy-no-such-dir-xyz").unwrap_err();
    assert_eq!(
        error,
        format!(
            "spawn: workdir '{}' does not exist or is not a directory",
            expanded.display()
        ),
        "~/x must expand to the home child before the check"
    );
}

#[test]
fn workdir_relative_is_refused() {
    assert_eq!(
        resolve_spawn_workdir("relative/dir").unwrap_err(),
        "spawn: field 'workdir' must be an absolute path"
    );
    // A `~user` form is not expanded, so it stays relative and is refused.
    assert_eq!(
        resolve_spawn_workdir("~someone/dir").unwrap_err(),
        "spawn: field 'workdir' must be an absolute path"
    );
}

#[test]
fn workdir_missing_or_not_a_directory_is_refused() {
    let dir = tempfile::tempdir().expect("dir");
    let missing = dir.path().join("nope");
    assert_eq!(
        resolve_spawn_workdir(&missing.to_string_lossy()).unwrap_err(),
        format!(
            "spawn: workdir '{}' does not exist or is not a directory",
            missing.display()
        )
    );

    let file = dir.path().join("a-file");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(
        resolve_spawn_workdir(&file.to_string_lossy()).unwrap_err(),
        format!(
            "spawn: workdir '{}' does not exist or is not a directory",
            file.display()
        )
    );

    // An existing directory is accepted as-is.
    assert_eq!(
        resolve_spawn_workdir(&dir.path().to_string_lossy()).expect("existing dir"),
        dir.path().to_string_lossy()
    );
}
