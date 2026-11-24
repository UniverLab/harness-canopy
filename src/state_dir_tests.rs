//! Unit tests for private state directory and file permission enforcement.

#[cfg(unix)]
mod unix_tests {
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Layer;

    use crate::db::Database;
    use crate::state_dir::{ensure_private_dir, open_private_log, tighten_tree};

    static HOME_LOCK: Mutex<()> = Mutex::new(());

    struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self { _lock: lock, prev }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            if let Some(ref val) = self.prev {
                std::env::set_var("HOME", val);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    struct InfoCapture(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> Layer<S> for InfoCapture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct MsgVisitor(String);
            impl tracing::field::Visit for MsgVisitor {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut visitor = MsgVisitor(String::new());
            event.record(&mut visitor);
            if !visitor.0.is_empty() {
                self.0.lock().unwrap().push(visitor.0);
            }
        }
    }

    fn mode_bits(path: &Path) -> u32 {
        fs::symlink_metadata(path)
            .expect("stat path")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn missing_dir_created_0700_and_ensure_data_dir_uses_home() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("nested").join("state");
        ensure_private_dir(&target).expect("create missing dir");
        assert_eq!(mode_bits(&target), 0o700);

        let fake_home = tempfile::tempdir().expect("fake home");
        let _guard = HomeGuard::set(fake_home.path());
        let data_dir = crate::ensure_data_dir().expect("ensure_data_dir");
        assert_eq!(data_dir, fake_home.path().join(".canopy"));
        assert_eq!(mode_bits(&data_dir), 0o700);
        assert_eq!(mode_bits(&data_dir.join("logs")), 0o700);
    }

    #[test]
    fn existing_0755_dir_tightened_to_0700_and_logs_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("state");
        fs::create_dir(&dir).expect("mkdir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
        assert_eq!(mode_bits(&dir), 0o755);

        let captured = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(InfoCapture(captured.clone()));
        tracing::subscriber::with_default(subscriber, || {
            ensure_private_dir(&dir).expect("first ensure");
            ensure_private_dir(&dir).expect("second ensure");
        });

        assert_eq!(mode_bits(&dir), 0o700);
        let logs = captured.lock().unwrap();
        assert_eq!(logs.len(), 1, "expected exactly 1 INFO log, got {logs:?}");
        assert!(logs[0].contains("mode 755 -> 0700"), "log was: {}", logs[0]);
    }

    #[test]
    fn symlink_dir_is_rejected_and_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("real");
        fs::create_dir(&target).expect("mkdir");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).expect("chmod");
        let link = tmp.path().join("link");
        symlink(&target, &link).expect("symlink");

        let err = ensure_private_dir(&link).expect_err("symlink must error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(mode_bits(&target), 0o755);
    }

    #[test]
    fn tighten_tree_recursively_strips_group_other_and_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let d1_file = root.join("daemon.log");
        fs::write(&d1_file, "hi").unwrap();
        fs::set_permissions(&d1_file, fs::Permissions::from_mode(0o644)).unwrap();

        let lance_dir = root.join("rag").join("vectors.lancedb").join("a");
        fs::create_dir_all(&lance_dir).unwrap();
        for d in [
            root.join("rag"),
            root.join("rag/vectors.lancedb"),
            lance_dir.clone(),
        ] {
            fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let d3_file = lance_dir.join("b.lance");
        fs::write(&d3_file, "vec").unwrap();
        fs::set_permissions(&d3_file, fs::Permissions::from_mode(0o644)).unwrap();

        let sub_dir = root.join("prompts");
        fs::create_dir(&sub_dir).unwrap();
        fs::set_permissions(&sub_dir, fs::Permissions::from_mode(0o755)).unwrap();

        let exe_file = sub_dir.join("hook.sh");
        fs::write(&exe_file, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&exe_file, fs::Permissions::from_mode(0o755)).unwrap();

        let already_private = root.join("secret.toml");
        fs::write(&already_private, "k = 1\n").unwrap();
        fs::set_permissions(&already_private, fs::Permissions::from_mode(0o600)).unwrap();

        // Expected tightened: d1_file, d3_file, sub_dir, exe_file = 4.
        let count = tighten_tree(root);
        assert_eq!(count, 4);
        assert_eq!(mode_bits(&d1_file), 0o600);
        assert_eq!(mode_bits(&d3_file), 0o600);
        assert_eq!(mode_bits(&sub_dir), 0o700);
        assert_eq!(mode_bits(&exe_file), 0o700);
        assert_eq!(mode_bits(&already_private), 0o600);

        assert_eq!(tighten_tree(root), 0);
    }

    #[test]
    fn tighten_tree_skips_symlinks_without_modifying_outside_target() {
        let outside = tempfile::tempdir().expect("outside");
        let outside_file = outside.path().join("shared.txt");
        fs::write(&outside_file, "keep").unwrap();
        fs::set_permissions(&outside_file, fs::Permissions::from_mode(0o644)).unwrap();

        let root = tempfile::tempdir().expect("root");
        let link = root.path().join("escape");
        symlink(&outside_file, &link).unwrap();

        assert_eq!(tighten_tree(root.path()), 0);
        assert_eq!(mode_bits(&outside_file), 0o644);
    }

    #[test]
    fn new_database_and_wal_shm_are_0600() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("background_agents.db");
        let db = Database::new(&db_path).expect("Database::new");
        assert_eq!(mode_bits(&db_path), 0o600);

        db.exec_test_sql(
            "CREATE TABLE IF NOT EXISTS t_priv (x INTEGER); INSERT INTO t_priv VALUES (1);",
        )
        .expect("write in WAL mode");

        let wal = tmp.path().join("background_agents.db-wal");
        let shm = tmp.path().join("background_agents.db-shm");
        assert!(wal.exists(), "WAL file must exist after write");
        assert!(shm.exists(), "SHM file must exist after write");
        assert_eq!(mode_bits(&wal), 0o600);
        assert_eq!(mode_bits(&shm), 0o600);

        // Existing db must not be truncated when reopened via new_safe.
        let _db2 = Database::new_safe(&db_path, tmp.path()).expect("Database::new_safe");
        assert!(fs::metadata(&db_path).unwrap().len() > 0);
    }

    #[test]
    fn open_private_log_and_append_log_create_0600_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let daemon_log = tmp.path().join("daemon.log");
        let _f = open_private_log(&daemon_log).expect("open_private_log");
        assert_eq!(mode_bits(&daemon_log), 0o600);

        let tui_log = tmp.path().join("logs").join("tui.log");
        crate::tui::logging::append_log(&tui_log, "hello");
        assert_eq!(mode_bits(&tui_log), 0o600);
    }
}
