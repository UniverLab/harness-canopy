//! Settings for the daemon itself. Read from the `[daemon]` table in
//! `config.toml`.

use serde::{Deserialize, Serialize};

use super::settings;

/// Settings for the daemon's own resource footprint. Read from the
/// `[daemon]` table in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// CPU cap for every agent and gate the daemon spawns, in percent of
    /// one core (600 = 6 cores). Written as `CPUQuota=` into the systemd
    /// user unit at setup and re-applied live on `config set` without a
    /// daemon restart. `0` disables the cap.
    #[serde(default = "default_cpu_quota_percent")]
    pub cpu_quota_percent: u32,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            cpu_quota_percent: default_cpu_quota_percent(),
        }
    }
}

fn default_cpu_quota_percent() -> u32 {
    settings::default_int("daemon.cpu_quota_percent") as u32
}

/// The host-dependent default for `daemon.cpu_quota_percent`: half the
/// host's logical CPUs, at least one full core. A registry `Literal`
/// cannot hold a host-dependent default, so the registry names this
/// function through [`settings::DefaultValue::Computed`] instead.
pub fn default_quota_percent() -> i64 {
    std::thread::available_parallelism()
        .map(|cpus| quota_percent_for_cpus(cpus.get()))
        .unwrap_or(200)
}

/// Pure form of [`default_quota_percent`] over an explicit CPU count, so
/// tests can pin the math without touching the host.
pub fn quota_percent_for_cpus(cpus: usize) -> i64 {
    let half_percent = cpus.saturating_mul(100) as i64 / 2;
    half_percent.max(100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_default_is_half_the_host_in_percent() {
        assert_eq!(quota_percent_for_cpus(12), 600);
        assert_eq!(quota_percent_for_cpus(8), 400);
    }

    #[test]
    fn quota_default_is_at_least_one_full_core() {
        assert_eq!(quota_percent_for_cpus(1), 100);
        assert_eq!(quota_percent_for_cpus(0), 100);
    }

    #[test]
    fn daemon_config_default_matches_the_registry() {
        assert_eq!(
            DaemonConfig::default().cpu_quota_percent as i64,
            settings::default_int("daemon.cpu_quota_percent")
        );
    }

    #[test]
    fn daemon_config_round_trips_via_config_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        std::fs::create_dir_all(&canopy_dir).unwrap();
        std::fs::write(
            canopy_dir.join("config.toml"),
            "[daemon]\ncpu_quota_percent = 400\n",
        )
        .unwrap();

        let loaded = super::super::canopy_config::CanopyConfig::load(&canopy_dir);
        assert_eq!(loaded.daemon.cpu_quota_percent, 400);
    }
}
