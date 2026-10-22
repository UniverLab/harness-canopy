//! Verdict-driven harness shutdown: once an agent files its verdict, its process
//! gets a grace period to exit on its own. If it is still alive after that, its
//! process group is terminated and its harness session is ended (CB84) before
//! the node, ensemble member or join that follows starts.

use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;

use crate::daemon::process::{terminate_process_group_async, KILL_GRACE};
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::graphs::GraphRunStatus;
use crate::domain::prompts::canopy_dir;

/// How often the run row is checked for a filed verdict.
const VERDICT_POLL: Duration = Duration::from_millis(500);

/// How a harness process ended relative to the verdict it filed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerdictEnd {
    /// The process exited on its own inside the grace period.
    Exited,
    /// The process was still alive after the grace period and was terminated.
    Terminated,
}

impl VerdictEnd {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::Terminated => "terminated",
        }
    }
}

/// Per-run verdict watch. The spawn path records how the process ended; the
/// caller reads it back once the process is gone.
#[derive(Clone)]
pub(crate) struct VerdictWatch {
    db: Database,
    run_id: String,
    grace: Duration,
    ended: Arc<OnceLock<VerdictEnd>>,
}

impl VerdictWatch {
    /// The watch for an agent run with the configured grace period. `None` when
    /// the grace period is 0, which disables the shutdown.
    pub(crate) fn for_run(db: &Database, run_id: &str) -> Option<Self> {
        let seconds = CanopyConfig::load(&canopy_dir())
            .graphs
            .verdict_grace_seconds;
        (seconds > 0).then(|| Self::new(db, run_id, Duration::from_secs(seconds)))
    }

    fn new(db: &Database, run_id: &str, grace: Duration) -> Self {
        Self {
            db: db.clone(),
            run_id: run_id.to_string(),
            grace,
            ended: Arc::new(OnceLock::new()),
        }
    }

    /// How the process ended after its verdict; `None` when no shutdown applied.
    pub(crate) fn ended(&self) -> Option<VerdictEnd> {
        self.ended.get().copied()
    }

    /// Resolves once the run has left `Running`, i.e. its verdict is filed. A
    /// run row that cannot be read never resolves, so nothing is terminated on
    /// a read error.
    async fn verdict_filed(&self) {
        loop {
            if self.verdict_is_filed() {
                return;
            }
            tokio::time::sleep(VERDICT_POLL).await;
        }
    }

    /// Whether the run row has left `Running` right now. An unreadable row
    /// counts as no verdict.
    fn verdict_is_filed(&self) -> bool {
        matches!(
            self.db.get_graph_run(&self.run_id),
            Ok(Some(run)) if run.status != GraphRunStatus::Running
        )
    }
}

/// Await a harness process's wait under the verdict watch. Until a verdict is
/// filed this is a plain wait. After it, the process gets the grace period to
/// exit on its own; if it is still alive then, its process group is terminated
/// and the wait is awaited to the end, so the process is gone when this
/// returns. A timeout that fires inside the grace period ends the wait as
/// `Exited`, since this code did not terminate the process.
pub(crate) async fn guard_wait<T>(
    watch: Option<&VerdictWatch>,
    pid: Option<u32>,
    wait: impl Future<Output = T>,
) -> T {
    let Some(watch) = watch else {
        return wait.await;
    };
    tokio::pin!(wait);
    tokio::select! {
        output = &mut wait => {
            // The process may have exited between verdict polls, after filing.
            if watch.verdict_is_filed() {
                let _ = watch.ended.set(VerdictEnd::Exited);
            }
            output
        }
        () = watch.verdict_filed() => {
            tokio::select! {
                output = &mut wait => {
                    let _ = watch.ended.set(VerdictEnd::Exited);
                    output
                }
                () = tokio::time::sleep(watch.grace) => {
                    let _ = watch.ended.set(VerdictEnd::Terminated);
                    if let Some(pid) = pid {
                        terminate_process_group_async(i64::from(pid), KILL_GRACE);
                    }
                    wait.await
                }
            }
        }
    }
}

/// Apply the verdict-driven shutdown record to a finished node's output: end
/// the harness session for a terminated process (CB84) and stamp the
/// `ended_after_verdict` / `session_end` fields. No-op when no verdict was
/// filed or the watch is disabled.
pub(crate) async fn apply_verdict_end(
    db: &Database,
    platform: &str,
    run: Option<&crate::domain::graphs::GraphNodeRun>,
    workdir: &str,
    run_id: &str,
    verdict: Option<&VerdictWatch>,
    output: &mut Value,
) {
    let Some(watch) = verdict else {
        return;
    };
    let Some(ended) = watch.ended() else {
        return;
    };
    let session_end = match (ended, run) {
        (VerdictEnd::Terminated, Some(run)) => Some(
            crate::graph_engine::routing::end_harness_session_for_verdict(
                db, platform, run, workdir, run_id,
            )
            .await,
        ),
        _ => None,
    };
    stamp_verdict_end(output, ended, session_end.as_deref());
}

/// Record how a verdict-filed process ended on the node's final output. The
/// session outcome is only present for a terminated process.
pub(crate) fn stamp_verdict_end(output: &mut Value, ended: VerdictEnd, session_end: Option<&str>) {
    let Value::Object(map) = output else {
        return;
    };
    map.insert(
        "ended_after_verdict".to_string(),
        Value::from(ended.as_str()),
    );
    if let Some(session_end) = session_end {
        map.insert("session_end".to_string(), Value::from(session_end));
    }
}

/// Carry the verdict-end record of the process onto a self-reported output.
/// A self-reported node takes its output from the run row, which never holds
/// the process's stamp, so the stamp is copied across here.
pub(crate) fn carry_verdict_end(reported: &mut Value, process: &Value) {
    let (Value::Object(map), Some(process)) = (reported, process.as_object()) else {
        return;
    };
    for key in ["ended_after_verdict", "session_end"] {
        if let Some(value) = process.get(key) {
            map.insert(key.to_string(), value.clone());
        }
    }
}

/// Wait for a child whose output is not streamed, bounded by the agent timeout
/// (the process group is terminated when the timeout elapses). Moved here to
/// keep `agent_spawn.rs` under the 800-line production cap.
pub(crate) async fn wait_untracked(
    child: tokio::process::Child,
    pid: Option<u32>,
    timeout_minutes: u64,
) -> Result<crate::graph_engine::CliProcessOutcome, crate::graph_engine::agent_spawn::SpawnError> {
    use crate::daemon::process::{terminate_process_group_async, KILL_GRACE};
    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_minutes * 60),
        child.wait_with_output(),
    )
    .await;

    match timeout_result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Ok(crate::graph_engine::CliProcessOutcome::Finished {
                exit_code,
                stdout,
                stderr,
            })
        }
        Ok(Err(error)) => Err(crate::graph_engine::agent_spawn::SpawnError::transient(
            error.to_string(),
        )),
        Err(_elapsed) => {
            if let Some(pid) = pid {
                terminate_process_group_async(pid as i64, KILL_GRACE);
            }
            Ok(crate::graph_engine::CliProcessOutcome::TimedOut)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::{tempdir, TempDir};

    use super::*;
    use crate::daemon::handler_run_history::graph_node_run_detail_json;
    use crate::domain::models::Cli;
    use crate::graph_engine::check::self_reported_execution;
    use crate::graph_engine::tests::{graph_fixture, insert_run_row, HomeGuard};
    use crate::graph_engine::{run_agent_process, GraphNode, GraphNodeKind};

    /// A run row in `status` (its graph, spec and node rows come from the
    /// engine fixture) and a watch over it with the given grace period.
    fn fixture(status: GraphRunStatus, grace: Duration) -> (TempDir, Arc<Database>, VerdictWatch) {
        let (dir, db, _engine, graph_id, spec_id) = graph_fixture().unwrap();
        insert_run_row(&db, &graph_id, &spec_id, "node-verdict", 0, 1).unwrap();
        let run_id = format!("run-{spec_id}-1");
        db.update_graph_run_result(&run_id, status, None, None)
            .unwrap();
        let watch = VerdictWatch::new(&db, &run_id, grace);
        (dir, db, watch)
    }

    fn spawn_harness(script: &str) -> tokio::process::Child {
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    #[tokio::test]
    async fn process_alive_after_grace_is_terminated_and_stops_writing() {
        let (dir, db, watch) = fixture(GraphRunStatus::Pass, Duration::from_secs(1));
        let file = dir.path().join("writes.txt");
        let mut child = spawn_harness(&format!(
            "while true; do echo x >> {}; sleep 0.1; done",
            file.display()
        ));
        let pid = child.id();

        let status = guard_wait(Some(&watch), pid, child.wait()).await.unwrap();

        assert_eq!(watch.ended(), Some(VerdictEnd::Terminated));
        assert!(
            !status.success(),
            "a terminated process must not report success"
        );
        let pid = pid.unwrap();
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "process must be gone when guard returns"
        );
        let written = fs::metadata(&file).unwrap().len();
        assert!(
            written > 0,
            "the harness must have been writing before termination"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            fs::metadata(&file).unwrap().len(),
            written,
            "file must stop changing"
        );
        let run = db.get_graph_run(&watch.run_id).unwrap().unwrap();
        assert_eq!(
            run.status,
            GraphRunStatus::Pass,
            "termination must not change the verdict"
        );
    }

    #[tokio::test]
    async fn process_that_exits_after_reporting_is_recorded_as_exited() {
        let (_dir, db, watch) = fixture(GraphRunStatus::Pass, Duration::from_secs(5));
        let mut child = spawn_harness("sleep 0.2");

        let status = guard_wait(Some(&watch), child.id(), child.wait())
            .await
            .unwrap();

        assert!(status.success());
        assert_eq!(watch.ended(), Some(VerdictEnd::Exited));
        let run = db.get_graph_run(&watch.run_id).unwrap().unwrap();
        assert_eq!(run.status, GraphRunStatus::Pass);
    }

    #[tokio::test]
    async fn process_without_a_verdict_is_never_terminated() {
        let (_dir, _db, watch) = fixture(GraphRunStatus::Running, Duration::from_millis(100));
        let mut child = spawn_harness("sleep 0.5");

        let status = guard_wait(Some(&watch), child.id(), child.wait())
            .await
            .unwrap();

        assert!(status.success());
        assert_eq!(watch.ended(), None);
    }

    #[test]
    fn stamp_records_exit_kind_and_session_outcome_on_the_output() {
        let mut output = serde_json::json!({ "verdict": "pass" });

        stamp_verdict_end(&mut output, VerdictEnd::Terminated, Some("ended"));

        assert_eq!(output["verdict"], "pass");
        assert_eq!(output["ended_after_verdict"], "terminated");
        assert_eq!(output["session_end"], "ended");
    }

    #[test]
    fn carry_adds_the_process_record_and_keeps_the_reported_verdict() {
        let mut reported = serde_json::json!({ "verdict": "pass" });
        let process = serde_json::json!({
            "ended_after_verdict": "terminated",
            "session_end": "ended",
            "failure_kind": "ignored",
        });

        carry_verdict_end(&mut reported, &process);

        assert_eq!(reported["verdict"], "pass");
        assert_eq!(reported["ended_after_verdict"], "terminated");
        assert_eq!(reported["session_end"], "ended");
        assert!(reported.get("failure_kind").is_none());
    }

    #[test]
    fn carry_leaves_the_reported_output_alone_without_a_process_record() {
        let mut reported = serde_json::json!({ "verdict": "pass" });

        carry_verdict_end(&mut reported, &serde_json::json!({ "exit_code": 0 }));

        assert_eq!(reported, serde_json::json!({ "verdict": "pass" }));
    }

    /// Config for the fake platform `fake-end`: the verdict grace period and,
    /// when given, the binary that ends its session.
    fn write_config(home: &Path, grace_seconds: u64, end_binary: Option<&Path>) {
        let mut config = format!("[graphs]\nverdict_grace_seconds = {grace_seconds}\n");
        if let Some(binary) = end_binary {
            config.push_str(&format!(
                "\n[[clis]]\nname = \"fake-end\"\nbinary = \"{}\"\nsession_end_args = [\"end\", \"--session-id\", \"{{session_id}}\"]\n",
                binary.display()
            ));
        }
        let canopy = home.join(".canopy");
        fs::create_dir_all(&canopy).unwrap();
        fs::write(canopy.join("config.toml"), config).unwrap();
    }

    /// A harness script that records its pid, then writes `file` every 100 ms
    /// for up to 60 s — or until it is terminated.
    fn long_harness(dir: &Path, file: &Path, pid_file: &Path) -> String {
        write_script(
            dir,
            "long-harness.sh",
            &format!(
                "echo $$ > '{}'\nend=$(( $(date +%s) + 60 ))\nwhile [ \"$(date +%s)\" -lt \"$end\" ]; do echo x >> '{}'; sleep 0.1; done",
                pid_file.display(),
                file.display()
            ),
        )
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().to_string()
    }

    /// Fake session-end binary: records every argv word and a `done=1` marker.
    fn end_binary(dir: &Path, record: &Path) -> PathBuf {
        let path = PathBuf::from(write_script(
            dir,
            "fake-end-cli",
            &format!(
                "for arg in \"$@\"; do echo \"arg=$arg\" >> '{}'; done\necho done=1 >> '{}'",
                record.display(),
                record.display()
            ),
        ));
        path
    }

    /// Strategy for the fake harness. `set_session_id` makes the engine assign
    /// the session id at spawn, which is what the session end later targets.
    fn harness_strategy(
        binary: &str,
        set_session_id: bool,
    ) -> crate::domain::cli_strategy::CliStrategy {
        crate::domain::cli_strategy::CliStrategy {
            binary: binary.to_string(),
            headless_mode: String::new(),
            model_flag: None,
            supports_working_dir: false,
            working_dir_flag: None,
            env_vars: std::collections::HashMap::new(),
            prompt_via_stdin: false,
            session_id_set_flag: set_session_id.then(|| "--session-id".to_string()),
            session_list_cmd: None,
            session_list_format_args: None,
            session_id_pattern: None,
            session_title_flag: None,
            session_list_args: None,
            session_end_args: None,
            session_resume_cmd: None,
            trust_flag: None,
            invocation_template: None,
            effort_declaration: None,
            decision_args: None,
            identity: None,
        }
    }

    /// A graph with one `Running` agent run, and the node that run executes.
    fn agent_run() -> (TempDir, Arc<Database>, String, GraphNode) {
        let (dir, db, _engine, graph_id, spec_id) = graph_fixture().unwrap();
        insert_run_row(&db, &graph_id, &spec_id, "node-agent-run", 0, 1).unwrap();
        let run_id = format!("run-{spec_id}-1");
        db.update_graph_run_result(&run_id, GraphRunStatus::Running, None, None)
            .unwrap();
        let node = GraphNode {
            id: "node-agent-run".to_string(),
            spec_id: None,
            graph_id: Some(graph_id),
            name: "Agent".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 0,
            created_at: chrono::Utc::now(),
        };
        (dir, db, run_id, node)
    }

    /// Stands in for the agent's `graph_complete_node` call: files a PASS on the
    /// run row after `delay`, while the harness process is still running.
    fn file_pass_after(db: &Arc<Database>, run_id: &str, delay: Duration) {
        let db = db.clone();
        let run_id = run_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            db.update_graph_run_result(
                &run_id,
                GraphRunStatus::Pass,
                Some(&serde_json::json!({ "verdict": "pass" })),
                Some(chrono::Utc::now()),
            )
            .unwrap();
        });
    }

    fn pid_alive(pid: &str) -> bool {
        Path::new(&format!("/proc/{}", pid.trim())).exists()
    }

    #[tokio::test]
    async fn run_agent_process_terminates_a_harness_that_keeps_running_after_pass() {
        let (dir, db, run_id, node) = agent_run();
        let home = tempdir().unwrap();
        write_config(home.path(), 1, None);
        let _home = HomeGuard::set(home.path());
        let file = dir.path().join("writes.txt");
        let pid_file = dir.path().join("pid.txt");
        let binary = long_harness(dir.path(), &file, &pid_file);
        file_pass_after(&db, &run_id, Duration::from_millis(200));

        let execution = run_agent_process(
            &db,
            &run_id,
            &Cli::new("test-cli"),
            &harness_strategy(&binary, false),
            &node,
            "prompt",
            None,
            None,
            dir.path().to_str().unwrap(),
            5,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            db.get_graph_run(&run_id).unwrap().unwrap().status,
            GraphRunStatus::Pass
        );
        assert_eq!(execution.output["ended_after_verdict"], "terminated");
        let pid = fs::read_to_string(&pid_file).unwrap();
        assert!(
            !pid_alive(&pid),
            "the harness must be gone when the call returns"
        );
        let written = fs::metadata(&file).unwrap().len();
        assert!(
            written > 0,
            "the harness must have written before termination"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            fs::metadata(&file).unwrap().len(),
            written,
            "file must stop changing"
        );
    }

    #[tokio::test]
    async fn run_agent_process_records_exited_when_the_harness_leaves_after_reporting() {
        let (dir, db, run_id, node) = agent_run();
        let home = tempdir().unwrap();
        write_config(home.path(), 5, None);
        let _home = HomeGuard::set(home.path());
        let binary = write_script(dir.path(), "quick.sh", "sleep 0.5");
        file_pass_after(&db, &run_id, Duration::from_millis(100));

        let execution = run_agent_process(
            &db,
            &run_id,
            &Cli::new("test-cli"),
            &harness_strategy(&binary, false),
            &node,
            "prompt",
            None,
            None,
            dir.path().to_str().unwrap(),
            5,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            db.get_graph_run(&run_id).unwrap().unwrap().status,
            GraphRunStatus::Pass
        );
        assert_eq!(execution.output["ended_after_verdict"], "exited");
        assert!(execution.output.get("session_end").is_none());
    }

    #[tokio::test]
    async fn terminated_harness_has_its_session_ended_before_the_call_returns() {
        let (dir, db, run_id, node) = agent_run();
        let home = tempdir().unwrap();
        let record = dir.path().join("end-record.txt");
        let binary = end_binary(dir.path(), &record);
        write_config(home.path(), 1, Some(&binary));
        let _home = HomeGuard::set(home.path());
        let file = dir.path().join("writes.txt");
        let pid_file = dir.path().join("pid.txt");
        let harness = long_harness(dir.path(), &file, &pid_file);
        file_pass_after(&db, &run_id, Duration::from_millis(200));

        let strategy = harness_strategy(&harness, true);
        let execution = run_agent_process(
            &db,
            &run_id,
            &Cli::new("fake-end"),
            &strategy,
            &node,
            "prompt",
            None,
            None,
            dir.path().to_str().unwrap(),
            5,
            None,
        )
        .await
        .unwrap();

        let run = db.get_graph_run(&run_id).unwrap().unwrap();
        let session_id = run
            .session_id
            .expect("the engine assigns the session id at spawn");
        let recorded = fs::read_to_string(&record)
            .expect("the session-end command must have run before the call returned");
        assert!(recorded.contains("done=1"), "{recorded}");
        assert!(recorded.contains("arg=end"), "{recorded}");
        assert!(
            recorded.contains(&format!("arg={session_id}")),
            "run row id {session_id}: {recorded}"
        );
        assert_eq!(execution.output["ended_after_verdict"], "terminated");
        assert!(execution.output.get("session_end").is_some());
        let pid = fs::read_to_string(&pid_file).unwrap();
        assert!(
            !pid_alive(&pid),
            "the next node must not start while the harness is alive"
        );
    }

    #[tokio::test]
    async fn ended_after_verdict_reads_back_through_the_run_detail() {
        let (dir, db, run_id, node) = agent_run();
        let home = tempdir().unwrap();
        write_config(home.path(), 1, None);
        let _home = HomeGuard::set(home.path());
        let file = dir.path().join("writes.txt");
        let pid_file = dir.path().join("pid.txt");
        let binary = long_harness(dir.path(), &file, &pid_file);
        file_pass_after(&db, &run_id, Duration::from_millis(200));
        let execution = run_agent_process(
            &db,
            &run_id,
            &Cli::new("test-cli"),
            &harness_strategy(&binary, false),
            &node,
            "prompt",
            None,
            None,
            dir.path().to_str().unwrap(),
            5,
            None,
        )
        .await
        .unwrap();

        // The self-reported path the engine takes: the run row's output, with
        // the process record carried onto it.
        let run = db.get_graph_run(&run_id).unwrap();
        let mut reported = self_reported_execution(run.as_ref(), &node).unwrap();
        carry_verdict_end(&mut reported.output, &execution.output);
        db.update_graph_run_result(
            &run_id,
            reported.status,
            Some(&reported.output),
            Some(chrono::Utc::now()),
        )
        .unwrap();

        let run = db.get_graph_run(&run_id).unwrap().unwrap();
        let detail = graph_node_run_detail_json(&run, Some("Agent"), None);
        assert_eq!(detail["status"], "pass");
        assert_eq!(detail["output"]["verdict"], "pass");
        assert_eq!(detail["output"]["ended_after_verdict"], "terminated");
    }

    #[test]
    fn resolve_keeps_the_self_reported_verdict_and_merges_the_process_record() {
        let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
        insert_run_row(&db, &graph_id, &spec_id, "node-resolve", 0, 1).unwrap();
        let run_id = format!("run-{spec_id}-1");
        db.update_graph_run_result(
            &run_id,
            GraphRunStatus::Pass,
            Some(&serde_json::json!({ "verdict": "pass" })),
            Some(chrono::Utc::now()),
        )
        .unwrap();
        let run = db.get_graph_run(&run_id).unwrap().unwrap();
        let execution = crate::graph_engine::NodeExecution {
            status: GraphRunStatus::Fail,
            output: serde_json::json!({ "ended_after_verdict": "terminated" }),
            summary: "process".to_string(),
        };

        let resolved = engine
            .resolve_running_node_execution(&run_id, execution, &run)
            .unwrap();

        assert_eq!(resolved.status, GraphRunStatus::Pass);
        let stored = db.get_graph_run(&run_id).unwrap().unwrap();
        let detail = graph_node_run_detail_json(&stored, None, None);
        assert_eq!(detail["status"], "pass");
        assert_eq!(detail["output"]["verdict"], "pass");
        assert_eq!(detail["output"]["ended_after_verdict"], "terminated");
    }
}
