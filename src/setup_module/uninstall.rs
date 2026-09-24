use crate::setup_module::config_manip::{
    remove_toml_array_entry_str, remove_toml_key_section_str, strip_jsonc_comments,
};
use crate::setup_module::models::{resolve_config_path, Platform};
use crate::setup_module::registry_fetch::fetch_registry;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const OWN_SERVER_NAMES: &[&str] = &["canopy"];
const SHARED_SERVER_NAMES: &[&str] = &["fetch", "filesystem"];

pub struct UninstallPlan {
    pub service: ServicePlan,
    pub canopy_dir: PathBuf,
    pub platform_edits: Vec<PlatformEdit>,
    pub skills_symlinks: Vec<PathBuf>,
    pub daemon_running: bool,
    pub agents_skills_dir: PathBuf,
    pub keep_shared_servers: bool,
    pub orphan_pid: Option<u32>,
    pub port_pid: Option<u32>,
}

pub struct ServicePlan {
    pub unit_path: PathBuf,
    pub exists: bool,
    pub dropin_dir: PathBuf,
    pub dropin_exists: bool,
    pub dropin_files: Vec<PathBuf>,
}

pub struct PlatformEdit {
    pub platform_name: String,
    pub config_path: PathBuf,
    pub servers_to_remove: Vec<String>,
    pub is_toml: bool,
    pub toml_array_format: bool,
    pub mcp_servers_key: Vec<String>,
    pub own_servers: Vec<String>,
    pub shared_servers: Vec<String>,
}

pub fn build_uninstall_plan(keep_shared_servers: bool) -> Result<UninstallPlan> {
    let home = dirs::home_dir().context("No home directory")?;
    build_uninstall_plan_with_home(&home, keep_shared_servers)
}

pub fn build_uninstall_plan_with_home(
    home: &Path,
    keep_shared_servers: bool,
) -> Result<UninstallPlan> {
    let canopy_dir = home.join(".canopy");
    let agents_skills_dir = home.join(".agents").join("skills");

    let registry = match fetch_registry() {
        Ok(reg) => reg,
        Err(e) => {
            // The registry is the source of truth for where setup wrote
            // (config_path + mcp_servers_key). When it is unreachable, fall
            // back to the local baseline sidecar, which records that same
            // registry view as of the last successful refresh. Without this,
            // an offline uninstall would silently skip every third-party
            // config and orphan the MCP entries setup added.
            tracing::warn!("Could not fetch registry for uninstall plan: {e}");
            match crate::domain::registry_baseline::RegistryBaseline::load(&canopy_dir) {
                Some(baseline) => {
                    eprintln!(
                        "  Registry unreachable ({e}); using local baseline for config reversal."
                    );
                    crate::setup_module::models::RegistryRaw {
                        platforms: baseline.platforms,
                        canonical_servers: Default::default(),
                    }
                }
                None => {
                    eprintln!(
                        "  Warning: registry unreachable ({e}) and no local baseline found; \
                         third-party MCP config entries will NOT be reverted."
                    );
                    crate::setup_module::models::RegistryRaw {
                        platforms: vec![],
                        canonical_servers: Default::default(),
                    }
                }
            }
        }
    };

    let mut platform_edits = Vec::new();
    for platform in &registry.platforms {
        let config_path = resolve_config_path(home, &platform.config_path);
        if !config_path.exists() {
            continue;
        }
        let own_servers: Vec<String> = OWN_SERVER_NAMES
            .iter()
            .map(|server| server.to_string())
            .collect();
        let shared_servers: Vec<String> = SHARED_SERVER_NAMES
            .iter()
            .map(|server| server.to_string())
            .collect();
        let mut servers_to_remove = own_servers.clone();
        if !keep_shared_servers {
            servers_to_remove.extend(shared_servers.clone());
        }
        platform_edits.push(PlatformEdit {
            platform_name: platform.name.clone(),
            config_path,
            servers_to_remove,
            is_toml: platform.config_format.as_deref() == Some("toml"),
            toml_array_format: platform.toml_array_format,
            mcp_servers_key: platform.mcp_servers_key.clone(),
            own_servers,
            shared_servers,
        });
    }

    let service = build_service_plan(home);
    let (port_pid, orphan_pid) = detect_uninstall_orphan();
    let daemon_running = check_daemon_running(&canopy_dir)
        || port_pid.is_some_and(crate::daemon::process::is_process_running);
    let skills_symlinks = collect_skills_symlinks(&registry.platforms, home, &agents_skills_dir);

    Ok(UninstallPlan {
        service,
        canopy_dir,
        platform_edits,
        skills_symlinks,
        daemon_running,
        agents_skills_dir,
        keep_shared_servers,
        orphan_pid,
        port_pid,
    })
}

fn build_service_plan(home: &Path) -> ServicePlan {
    #[cfg(target_os = "macos")]
    let unit_path = home.join("Library/LaunchAgents/com.canopy.plist");
    #[cfg(not(target_os = "macos"))]
    let unit_path = home.join(".config/systemd/user/canopy.service");

    #[cfg(target_os = "macos")]
    let (dropin_dir, dropin_files, dropin_exists) = (PathBuf::new(), Vec::new(), false);
    #[cfg(not(target_os = "macos"))]
    let (dropin_dir, dropin_files, dropin_exists) = {
        let dropin_dir = unit_path.parent().map_or_else(
            || home.join(".config/systemd/user/canopy.service.d"),
            |parent| parent.join("canopy.service.d"),
        );
        let mut dropin_files: Vec<PathBuf> = std::fs::read_dir(&dropin_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension() == Some(std::ffi::OsStr::new("conf")))
                    .collect()
            })
            .unwrap_or_default();
        dropin_files.sort();
        let dropin_exists = dropin_dir.is_dir();
        (dropin_dir, dropin_files, dropin_exists)
    };

    ServicePlan {
        exists: unit_path.exists(),
        unit_path,
        dropin_dir,
        dropin_exists,
        dropin_files,
    }
}

/// Pure rule: Some(orphan_pid) iff a port occupant exists AND a unit is installed
/// AND occupant != manager MainPID. Mirrors process::orphan_needs_kill but returns the pid.
pub(crate) fn diagnose_uninstall_orphan(
    unit_installed: bool,
    manager_pid: Option<u32>,
    port_pid: Option<u32>,
) -> Option<u32> {
    if unit_installed && port_pid.is_some() && manager_pid != port_pid {
        port_pid
    } else {
        None
    }
}

fn detect_uninstall_orphan() -> (Option<u32>, Option<u32>) {
    let port = crate::resolve_port(None);
    let port_pid = crate::daemon::process::resolve_port_pid(port);
    let manager = crate::daemon::process::service_manager_facts();
    let orphan_pid = diagnose_uninstall_orphan(
        manager.is_some(),
        manager.and_then(|facts| facts.pid),
        port_pid,
    );
    (port_pid, orphan_pid)
}

fn check_daemon_running(canopy_dir: &Path) -> bool {
    let pid_path = canopy_dir.join("daemon.pid");
    if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            return crate::daemon::process::is_process_running(pid);
        }
    }
    false
}

fn collect_skills_symlinks(
    platforms: &[Platform],
    home: &Path,
    agents_skills_dir: &Path,
) -> Vec<PathBuf> {
    let mut symlinks = Vec::new();
    for platform in platforms {
        let Some(ref skills_dir_rel) = platform.skills_dir else {
            continue;
        };
        let skills_dir = home.join(skills_dir_rel);
        if !skills_dir.is_dir() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if is_symlink_pointing_into(&path, agents_skills_dir) {
                    symlinks.push(path);
                }
            }
        }
    }
    symlinks
}

fn is_symlink_pointing_into(path: &Path, target_dir: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    match std::fs::read_link(path) {
        Ok(target) => target.starts_with(target_dir),
        Err(_) => false,
    }
}

pub fn print_dry_run(plan: &UninstallPlan) {
    println!("canopy uninstall — dry run (no changes will be made)");
    println!();

    if plan.daemon_running {
        println!("  [stop]  Running daemon (PID file found)");
    }
    if let Some(port_pid) = plan.port_pid {
        if plan.orphan_pid == Some(port_pid) {
            println!(
                "  [stop]  Orphan daemon holds the port (PID {port_pid}, not the unit's MainPID) — will be stopped too"
            );
        }
    }

    if plan.service.exists {
        println!(
            "  [remove] Service unit: {}",
            plan.service.unit_path.display()
        );
    } else {
        println!("  [skip]  Service unit not found");
    }
    if let Some(line) = format_dropin_line(&plan.service) {
        println!("{line}");
        for file in &plan.service.dropin_files {
            println!("    - {}", file.display());
        }
    }

    if plan.platform_edits.is_empty() {
        println!("  [skip]  No platform configs to revert");
    } else {
        for edit in &plan.platform_edits {
            println!(
                "{}",
                format_platform_edit_line(edit, plan.keep_shared_servers)
            );
        }
    }

    if plan.skills_symlinks.is_empty() {
        println!("  [skip]  No skills symlinks found");
    } else {
        for link in &plan.skills_symlinks {
            println!("  [remove] Skills symlink: {}", link.display());
        }
    }

    if plan.canopy_dir.exists() {
        println!(
            "  [keep]  {} (use --purge to delete)",
            plan.canopy_dir.display()
        );
        if plan.agents_skills_dir.exists() {
            println!(
                "  [keep]  {} (use --purge to delete)",
                plan.agents_skills_dir.display()
            );
        }
    } else {
        println!("  [skip]  {} does not exist", plan.canopy_dir.display());
    }
}

fn format_dropin_line(service: &ServicePlan) -> Option<String> {
    service.dropin_exists.then(|| {
        format!(
            "  [remove] Service drop-ins: {}/",
            service.dropin_dir.display()
        )
    })
}

pub(crate) fn format_platform_edit_line(edit: &PlatformEdit, keep_shared: bool) -> String {
    let own = edit.own_servers.join(", ");
    let shared = edit.shared_servers.join(", ");
    if keep_shared {
        format!(
            "  [edit]  {} — remove servers [{own}] (shared [{shared}] kept) from {}",
            edit.platform_name,
            edit.config_path.display()
        )
    } else {
        format!(
            "  [edit]  {} — remove servers [{own}] + shared [{shared}] (keep with --keep-shared-servers) from {}",
            edit.platform_name,
            edit.config_path.display()
        )
    }
}

pub fn execute_uninstall(plan: &UninstallPlan, purge_data: bool) -> Result<()> {
    if plan.daemon_running {
        println!("  Stopping daemon...");
        let _ = crate::setup_module::daemon_service::stop_daemon();
        if let Some(pid) = plan.orphan_pid {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            for _ in 0..12 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if !crate::daemon::process::is_process_running(pid) {
                    break;
                }
            }
        }
    }

    if plan.service.exists {
        println!(
            "  Removing service unit: {}",
            plan.service.unit_path.display()
        );
        let _ = crate::daemon::service_install::uninstall_service();
    }
    if plan.service.dropin_exists {
        println!(
            "  Removing service drop-ins: {}",
            plan.service.dropin_dir.display()
        );
        let _ = remove_service_dropins(&plan.service);
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
    }

    for edit in &plan.platform_edits {
        let platform = Platform {
            name: edit.platform_name.clone(),
            config_path: String::new(),
            config_format: if edit.is_toml {
                Some("toml".to_string())
            } else {
                None
            },
            toml_array_format: edit.toml_array_format,
            command_format: "separate".to_string(),
            mcp_servers_key: edit.mcp_servers_key.clone(),
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: Default::default(),
            required_fields: Default::default(),
            server_extras: Default::default(),
            skills_dir: None,
            instruction_file: None,
            cli: None,

            provider: None,
            tool_name: None,
        };
        let removed =
            revert_platform_config(&platform, &edit.config_path, &edit.servers_to_remove)?;
        if removed.is_empty() {
            println!("  [skip]  {} — no canopy entries found", edit.platform_name);
        } else {
            println!(
                "  [edit]  {} — removed [{}]",
                edit.platform_name,
                removed.join(", ")
            );
        }
    }

    for link in &plan.skills_symlinks {
        println!("  Removing symlink: {}", link.display());
        let _ = std::fs::remove_file(link);
    }

    if purge_data {
        if plan.canopy_dir.exists() {
            println!("  Deleting {}", plan.canopy_dir.display());
            std::fs::remove_dir_all(&plan.canopy_dir).context("Failed to delete ~/.canopy")?;
        }
        if plan.agents_skills_dir.exists() {
            println!("  Deleting {}", plan.agents_skills_dir.display());
            let _ = std::fs::remove_dir_all(&plan.agents_skills_dir);
        }
    } else if plan.canopy_dir.exists() {
        println!(
            "  Keeping {} (use `canopy uninstall --purge` to delete data)",
            plan.canopy_dir.display()
        );
    }

    Ok(())
}

fn remove_service_dropins(service: &ServicePlan) -> std::io::Result<()> {
    if service.dropin_exists {
        std::fs::remove_dir_all(&service.dropin_dir)?;
    }
    Ok(())
}

fn revert_platform_config(
    platform: &Platform,
    config_path: &Path,
    server_names: &[String],
) -> Result<Vec<String>> {
    let mut removed = Vec::new();

    if !config_path.exists() {
        return Ok(removed);
    }

    if platform.config_format.as_deref() == Some("toml") {
        let content = std::fs::read_to_string(config_path)?;
        let mut updated = content.clone();
        for server_name in server_names {
            let before = updated.clone();
            if platform.toml_array_format {
                let section = platform.mcp_servers_key.join(".");
                let array_header = format!("[[{section}]]");
                let name_line = format!("name = \"{server_name}\"");
                updated = remove_toml_array_entry_str(&updated, &array_header, &name_line);
            } else {
                let section = platform
                    .mcp_servers_key
                    .first()
                    .map(String::as_str)
                    .unwrap_or("mcpServers");
                let table_header = format!("[{section}.{server_name}]");
                updated = remove_toml_key_section_str(&updated, &table_header);
            }
            if updated != before {
                removed.push(server_name.clone());
            }
        }
        if updated != content {
            atomic_write(config_path, &updated)?;
        }
    } else {
        let content = std::fs::read_to_string(config_path)?;
        let clean = strip_jsonc_comments(&content);
        let mut root: serde_json::Value =
            serde_json::from_str(&clean).unwrap_or(serde_json::json!({}));
        // Setup nests each server under the full `mcp_servers_key` chain
        // (see platform_adapter::apply_upsert_to_platform, which pushes the
        // server name onto the key list before calling upsert_json_key), so the
        // revert must walk the same chain — not just the first key — or a
        // nested platform is left with a dangling canopy entry.
        let parent_keys: Vec<&str> = if platform.mcp_servers_key.is_empty() {
            vec!["mcpServers"]
        } else {
            platform
                .mcp_servers_key
                .iter()
                .map(String::as_str)
                .collect()
        };
        if let Some(parent) = traverse_to_object_mut(&mut root, &parent_keys) {
            for server_name in server_names {
                if parent.remove(server_name).is_some() {
                    removed.push(server_name.clone());
                }
            }
        }
        if !removed.is_empty() {
            atomic_write(config_path, &(serde_json::to_string_pretty(&root)? + "\n"))?;
        }
    }

    Ok(removed)
}

/// Walk `keys` from `root` and return the object at that path, if every
/// segment exists and is an object.
fn traverse_to_object_mut<'a>(
    root: &'a mut serde_json::Value,
    keys: &[&str],
) -> Option<&'a mut serde_json::Map<String, serde_json::Value>> {
    let mut current = root;
    for key in keys {
        current = current.get_mut(*key)?;
    }
    current.as_object_mut()
}

fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("tmp");
    let tmp = path.with_extension(format!("{ext}.canopy-uninstall-tmp"));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_module::models::Platform;
    use std::collections::HashMap;

    fn json_platform(mcp_servers_key: Vec<String>) -> Platform {
        Platform {
            name: "test-json".to_string(),
            config_path: String::new(),
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key,
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: HashMap::new(),
            required_fields: HashMap::new(),
            server_extras: HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            cli: None,

            provider: None,
            tool_name: None,
        }
    }

    fn toml_keytable_platform(mcp_servers_key: Vec<String>) -> Platform {
        Platform {
            name: "test-toml-kt".to_string(),
            config_path: String::new(),
            config_format: Some("toml".to_string()),
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key,
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: HashMap::new(),
            required_fields: HashMap::new(),
            server_extras: HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            cli: None,

            provider: None,
            tool_name: None,
        }
    }

    fn toml_array_platform(mcp_servers_key: Vec<String>) -> Platform {
        Platform {
            name: "test-toml-arr".to_string(),
            config_path: String::new(),
            config_format: Some("toml".to_string()),
            toml_array_format: true,
            command_format: "separate".to_string(),
            mcp_servers_key,
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: HashMap::new(),
            required_fields: HashMap::new(),
            server_extras: HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            cli: None,

            provider: None,
            tool_name: None,
        }
    }

    #[test]
    fn test_atomic_write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");
        atomic_write(&path, r#"{"hello": "world"}"#).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, r#"{"hello": "world"}"#);
        let tmp = path.with_extension("json.canopy-uninstall-tmp");
        assert!(!tmp.exists());
    }

    #[test]
    fn test_atomic_write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");
        std::fs::write(&path, "old").unwrap();
        atomic_write(&path, "new").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "new");
        let tmp = path.with_extension("json.canopy-uninstall-tmp");
        assert!(!tmp.exists());
    }

    #[test]
    fn test_revert_json_removes_canopy_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {"canopy": {"command": "canopy"}, "other": {"command": "other"}}}"#,
        )
        .unwrap();

        let platform = json_platform(vec!["mcpServers".to_string()]);
        let servers = vec!["canopy".to_string()];
        let removed = revert_platform_config(&platform, &path, &servers).unwrap();

        assert_eq!(removed, vec!["canopy".to_string()]);
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root["mcpServers"].get("canopy").is_none());
        assert!(root["mcpServers"].get("other").is_some());
    }

    #[test]
    fn test_revert_json_removes_multiple_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {"canopy": {}, "fetch": {}, "filesystem": {}, "other": {}}}"#,
        )
        .unwrap();

        let platform = json_platform(vec!["mcpServers".to_string()]);
        let servers = vec![
            "canopy".to_string(),
            "fetch".to_string(),
            "filesystem".to_string(),
        ];
        let removed = revert_platform_config(&platform, &path, &servers).unwrap();

        assert_eq!(removed.len(), 3);
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root["mcpServers"].get("canopy").is_none());
        assert!(root["mcpServers"].get("fetch").is_none());
        assert!(root["mcpServers"].get("filesystem").is_none());
        assert!(root["mcpServers"].get("other").is_some());
    }

    #[test]
    fn test_revert_json_noop_when_server_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let original = r#"{"mcpServers": {"other": {}}}"#;
        std::fs::write(&path, original).unwrap();

        let platform = json_platform(vec!["mcpServers".to_string()]);
        let servers = vec!["canopy".to_string()];
        let removed = revert_platform_config(&platform, &path, &servers).unwrap();

        assert!(removed.is_empty());
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, original);
    }

    #[test]
    fn test_revert_toml_keytable_removes_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[mcp_servers.canopy]\nurl = \"http://localhost\"\n\n[mcp_servers.other]\nurl = \"http://other\"\n",
        )
        .unwrap();

        let platform = toml_keytable_platform(vec!["mcp_servers".to_string()]);
        let servers = vec!["canopy".to_string()];
        let removed = revert_platform_config(&platform, &path, &servers).unwrap();

        assert_eq!(removed, vec!["canopy".to_string()]);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("[mcp_servers.canopy]"));
        assert!(content.contains("[mcp_servers.other]"));
    }

    #[test]
    fn test_revert_toml_array_removes_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[mcp_servers]]\nname = \"canopy\"\ncommand = \"canopy\"\n\n[[mcp_servers]]\nname = \"other\"\ncommand = \"other\"\n",
        )
        .unwrap();

        let platform = toml_array_platform(vec!["mcp_servers".to_string()]);
        let servers = vec!["canopy".to_string()];
        let removed = revert_platform_config(&platform, &path, &servers).unwrap();

        assert_eq!(removed, vec!["canopy".to_string()]);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("name = \"canopy\""));
        assert!(content.contains("name = \"other\""));
    }

    #[test]
    fn test_revert_json_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");

        let platform = json_platform(vec!["mcpServers".to_string()]);
        let servers = vec!["canopy".to_string()];
        let removed = revert_platform_config(&platform, &path, &servers).unwrap();

        assert!(removed.is_empty());
    }

    #[test]
    fn test_is_symlink_pointing_into_true() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("targets");
        std::fs::create_dir_all(&target_dir).unwrap();
        let target_file = target_dir.join("skill.md");
        std::fs::write(&target_file, "content").unwrap();

        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target_file, &link).unwrap();

        assert!(is_symlink_pointing_into(&link, &target_dir));
    }

    #[test]
    fn test_is_symlink_pointing_into_false_for_external() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("targets");
        std::fs::create_dir_all(&target_dir).unwrap();

        let external = dir.path().join("external");
        std::fs::write(&external, "content").unwrap();

        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&external, &link).unwrap();

        assert!(!is_symlink_pointing_into(&link, &target_dir));
    }

    #[test]
    fn test_is_symlink_pointing_into_false_for_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("targets");
        std::fs::create_dir_all(&target_dir).unwrap();

        let regular = dir.path().join("regular");
        std::fs::write(&regular, "content").unwrap();

        assert!(!is_symlink_pointing_into(&regular, &target_dir));
    }

    #[test]
    fn test_build_service_plan_linux() {
        let dir = tempfile::tempdir().unwrap();
        let service = build_service_plan(dir.path());
        assert!(service.unit_path.to_string_lossy().contains("canopy"));
        #[cfg(not(target_os = "macos"))]
        assert!(service.dropin_dir.ends_with("canopy.service.d"));
    }

    #[test]
    fn test_check_daemon_running_no_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!check_daemon_running(dir.path()));
    }

    #[test]
    fn test_revert_json_removes_nested_canopy_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {"servers": {"canopy": {"command": "canopy"}, "other": {"command": "other"}}}}"#,
        )
        .unwrap();

        let platform = json_platform(vec!["mcpServers".to_string(), "servers".to_string()]);
        let servers = vec!["canopy".to_string()];
        let removed = revert_platform_config(&platform, &path, &servers).unwrap();

        assert_eq!(removed, vec!["canopy".to_string()]);
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root["mcpServers"]["servers"].get("canopy").is_none());
        assert!(root["mcpServers"]["servers"].get("other").is_some());
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn uninstall_plan_includes_dropin_dir_when_browser_conf_present() {
        let home = tempfile::tempdir().unwrap();
        let service_dir = home.path().join(".config/systemd/user");
        let unit_path = service_dir.join("canopy.service");
        let dropin_dir = service_dir.join("canopy.service.d");
        std::fs::create_dir_all(&dropin_dir).unwrap();
        std::fs::write(&unit_path, "[Service]\n").unwrap();
        let browser_conf = dropin_dir.join("browser.conf");
        std::fs::write(&browser_conf, "[Service]\nEnvironment=BROWSER=/x\n").unwrap();

        let registry = tempfile::tempdir().unwrap();
        crate::setup_module::registry_fetch::set_local_registry(registry.path().to_path_buf());
        let plan = build_uninstall_plan_with_home(home.path(), false).unwrap();

        assert!(plan.service.dropin_exists);
        assert!(plan.service.dropin_dir.ends_with("canopy.service.d"));
        assert_eq!(plan.service.dropin_files, vec![browser_conf]);
        let line = format_dropin_line(&plan.service).unwrap();
        assert!(line.contains("[remove] Service drop-ins: "));
        assert!(line.contains("canopy.service.d/"));
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn uninstall_apply_removes_dropin_dir() {
        let home = tempfile::tempdir().unwrap();
        let service_dir = home.path().join(".config/systemd/user");
        let unit_path = service_dir.join("canopy.service");
        let dropin_dir = service_dir.join("canopy.service.d");
        std::fs::create_dir_all(&dropin_dir).unwrap();
        std::fs::write(&unit_path, "[Service]\n").unwrap();
        std::fs::write(dropin_dir.join("browser.conf"), "[Service]\n").unwrap();

        let mut plan = empty_plan(
            home.path().join(".canopy"),
            home.path().join(".agents/skills"),
        );
        plan.service = ServicePlan {
            unit_path,
            exists: true,
            dropin_dir: dropin_dir.clone(),
            dropin_exists: true,
            dropin_files: vec![dropin_dir.join("browser.conf")],
        };

        remove_service_dropins(&plan.service).unwrap();

        assert!(!dropin_dir.exists());
        assert!(plan.service.unit_path.exists());
    }

    #[test]
    fn keep_shared_removes_only_canopy() {
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join(".config/test/config.json");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            r#"{"mcpServers":{"canopy":{},"fetch":{},"mine":{}}}"#,
        )
        .unwrap();

        let registry = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(registry.path().join("platforms")).unwrap();
        std::fs::write(
            registry.path().join("index.toml"),
            "version = 6\n[[platforms]]\nname = \"test\"\nbinary = \"test\"\n",
        )
        .unwrap();
        std::fs::write(
            registry.path().join("platforms/test.toml"),
            "name = \"test\"\nconfig_path = \".config/test/config.json\"\nmcp_servers_key = [\"mcpServers\"]\n",
        )
        .unwrap();
        crate::setup_module::registry_fetch::set_local_registry(registry.path().to_path_buf());

        let plan = build_uninstall_plan_with_home(home.path(), true).unwrap();
        let edit = plan.platform_edits.first().unwrap();
        assert!(plan.keep_shared_servers);
        assert_eq!(edit.servers_to_remove, vec!["canopy".to_string()]);

        let removed = revert_platform_config(
            &json_platform(vec!["mcpServers".to_string()]),
            &config_path,
            &edit.servers_to_remove,
        )
        .unwrap();
        assert_eq!(removed, vec!["canopy".to_string()]);
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(root["mcpServers"].get("canopy").is_none());
        assert!(root["mcpServers"].get("fetch").is_some());
        assert!(root["mcpServers"].get("mine").is_some());
    }

    #[test]
    fn dry_run_line_names_own_and_shared_separately() {
        let edit = PlatformEdit {
            platform_name: "test".to_string(),
            config_path: PathBuf::from("/tmp/config.json"),
            servers_to_remove: vec![
                "canopy".to_string(),
                "fetch".to_string(),
                "filesystem".to_string(),
            ],
            is_toml: false,
            toml_array_format: false,
            mcp_servers_key: vec!["mcpServers".to_string()],
            own_servers: vec!["canopy".to_string()],
            shared_servers: vec!["fetch".to_string(), "filesystem".to_string()],
        };

        let default_line = format_platform_edit_line(&edit, false);
        assert!(default_line.contains(
            "remove servers [canopy] + shared [fetch, filesystem] (keep with --keep-shared-servers)"
        ));
        assert!(default_line.contains("from /tmp/config.json"));

        let keep_line = format_platform_edit_line(&edit, true);
        assert!(keep_line.contains("remove servers [canopy] (shared [fetch, filesystem] kept)"));
        assert!(!keep_line.contains("remove servers [canopy, fetch"));
    }

    #[test]
    fn diagnose_uninstall_orphan_truth_table() {
        assert_eq!(diagnose_uninstall_orphan(true, Some(7), Some(9)), Some(9));
        assert_eq!(diagnose_uninstall_orphan(true, None, Some(9)), Some(9));
        assert_eq!(diagnose_uninstall_orphan(true, Some(9), Some(9)), None);
        assert_eq!(diagnose_uninstall_orphan(false, None, Some(9)), None);
        assert_eq!(diagnose_uninstall_orphan(true, None, None), None);
    }

    fn empty_plan(canopy_dir: PathBuf, agents_skills_dir: PathBuf) -> UninstallPlan {
        UninstallPlan {
            service: ServicePlan {
                unit_path: canopy_dir.join("nonexistent.service"),
                exists: false,
                dropin_dir: PathBuf::new(),
                dropin_exists: false,
                dropin_files: vec![],
            },
            canopy_dir,
            platform_edits: vec![],
            skills_symlinks: vec![],
            daemon_running: false,
            agents_skills_dir,
            keep_shared_servers: false,
            orphan_pid: None,
            port_pid: None,
        }
    }

    #[test]
    fn test_execute_purge_false_preserves_canopy_dir() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        std::fs::create_dir_all(canopy_dir.join("models")).unwrap();
        let plan = empty_plan(canopy_dir.clone(), dir.path().join(".agents/skills"));

        execute_uninstall(&plan, false).unwrap();

        assert!(canopy_dir.exists());
    }

    #[test]
    fn test_execute_purge_true_removes_canopy_dir() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        std::fs::create_dir_all(canopy_dir.join("models")).unwrap();
        std::fs::write(canopy_dir.join("canopy.db"), "data").unwrap();
        let plan = empty_plan(canopy_dir.clone(), dir.path().join(".agents/skills"));

        execute_uninstall(&plan, true).unwrap();

        assert!(!canopy_dir.exists());
    }
}
