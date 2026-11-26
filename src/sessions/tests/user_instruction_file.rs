use std::path::Path;
use std::sync::Mutex;

use super::{agent_spawn, terminal_spawn, test_db, test_hub, KillGuard};
use crate::domain::instruction_file::{SKILL_BEGIN, SKILL_LINE};
use crate::domain::registry_baseline::RegistryBaseline;
use crate::setup_module::models::Platform;

static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

struct HomeOverrideGuard {
    prev: Option<std::ffi::OsString>,
}

impl HomeOverrideGuard {
    fn set(path: &Path) -> Self {
        let prev = std::env::var_os("CANOPY_HOME_OVERRIDE");
        unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", path) };
        Self { prev }
    }
}

impl Drop for HomeOverrideGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", v) },
            None => unsafe { std::env::remove_var("CANOPY_HOME_OVERRIDE") },
        }
    }
}

fn seed_test_platform(home: &Path, cli: &str, user_instruction_file: &str) {
    let platform = Platform {
        name: cli.to_string(),
        config_path: format!(".{cli}/config.json"),
        config_format: None,
        toml_array_format: false,
        command_format: "separate".to_string(),
        mcp_servers_key: vec![],
        deprecated_keys: vec![],
        unsupported_keys: vec![],
        fields_mapping: std::collections::HashMap::new(),
        required_fields: std::collections::HashMap::new(),
        server_extras: std::collections::HashMap::new(),
        skills_dir: None,
        instruction_file: None,
        instruction_precedence: vec![],
        user_instruction_file: Some(user_instruction_file.to_string()),
        provider: None,
        tool_name: None,
        project_paths: Vec::new(),
        cli: None,
    };
    RegistryBaseline {
        clis: vec![],
        platforms: vec![platform],
    }
    .save(&home.join(".canopy"))
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn spawn_agent_with_readonly_user_instruction_file_still_returns_session() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    seed_test_platform(home.path(), "cat", ".test_harness/USER.md");

    let target = home.path().join(".test_harness/USER.md");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "# Read Only\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
    // Make parent dir read-only too so NamedTempFile::new_in fails even under rootless/umask edge cases.
    let parent = target.parent().unwrap();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o555)).unwrap();

    let _home_guard = HomeOverrideGuard::set(home.path());
    let (db, _db_dir) = test_db();
    let hub = test_hub(&db);
    let session = hub
        .spawn(&agent_spawn("cat", workdir.path().to_str().unwrap()))
        .expect("spawn must succeed even when user instruction file is read-only");
    let _kill = KillGuard {
        hub,
        id: session.id.clone(),
    };

    // Restore permissions so tempdir cleanup succeeds.
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[tokio::test]
async fn spawn_agent_leaves_repo_claude_md_byte_identical() {
    let _lock = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    seed_test_platform(home.path(), "cat", ".test_harness/USER.md");

    let user_file = home.path().join(".test_harness/USER.md");
    std::fs::create_dir_all(user_file.parent().unwrap()).unwrap();
    std::fs::write(&user_file, "# User\n").unwrap();

    let repo_claude = workdir.path().join("CLAUDE.md");
    let repo_original = b"# Project Instructions\nDo not touch.\n";
    std::fs::write(&repo_claude, repo_original).unwrap();

    let _home_guard = HomeOverrideGuard::set(home.path());
    let (db, _db_dir) = test_db();
    let hub = test_hub(&db);
    let session = hub
        .spawn(&agent_spawn("cat", workdir.path().to_str().unwrap()))
        .expect("spawn agent session");
    let _kill = KillGuard {
        hub,
        id: session.id.clone(),
    };

    assert_eq!(std::fs::read(&repo_claude).unwrap(), repo_original);
    let user_content = std::fs::read_to_string(&user_file).unwrap();
    assert!(user_content.contains(SKILL_BEGIN));
    assert!(user_content.contains(SKILL_LINE));
}

#[tokio::test]
async fn spawn_plain_shell_does_not_touch_user_instruction_file() {
    let _lock = HOME_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    seed_test_platform(home.path(), "sh", ".test_harness/USER.md");

    let user_file = home.path().join(".test_harness/USER.md");
    std::fs::create_dir_all(user_file.parent().unwrap()).unwrap();
    std::fs::write(&user_file, "# User\n").unwrap();

    let _home_guard = HomeOverrideGuard::set(home.path());
    let (db, _db_dir) = test_db();
    let hub = test_hub(&db);
    let session = hub
        .spawn(&terminal_spawn("sh", workdir.path().to_str().unwrap()))
        .expect("spawn terminal session");
    let _kill = KillGuard {
        hub,
        id: session.id.clone(),
    };

    assert_eq!(std::fs::read_to_string(&user_file).unwrap(), "# User\n");
}
