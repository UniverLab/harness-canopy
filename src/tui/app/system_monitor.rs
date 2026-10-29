//! System-monitor thread, smoothing/interpolation helpers, and log-analysis
//! free functions moved from `mod.rs` (B5 decomposition).

use std::sync::{atomic::Ordering, Arc};

// ── Sample type ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SystemSample {
    pub cpu_usage: f32,
    pub mem_pct: f32,
    pub load: f32,
    pub cpu_temp: f32,
    pub gpu_usage: f32,
    pub gpu_temp: f32,
}

// ── Sampling ───────────────────────────────────────────────────

pub(crate) fn sample_from(info: &crate::system::SystemInfo) -> SystemSample {
    let mem_pct = if info.memory_total > 0 {
        (info.memory_used as f32 / info.memory_total as f32) * 100.0
    } else {
        0.0
    };

    SystemSample {
        cpu_usage: info.cpu_usage,
        mem_pct,
        load: info.load_average.unwrap_or(0.0) as f32,
        cpu_temp: info.cpu_temperature.unwrap_or(0.0),
        gpu_usage: info.gpu_info.as_ref().and_then(|g| g.usage).unwrap_or(0.0),
        gpu_temp: info
            .gpu_info
            .as_ref()
            .and_then(|g| g.temperature)
            .unwrap_or(0.0),
    }
}

// ── Adaptive polling ───────────────────────────────────────────

pub(crate) fn adaptive_change_score(prev: SystemSample, next: SystemSample) -> f32 {
    let cpu_delta = (next.cpu_usage - prev.cpu_usage).abs() / 100.0;
    let mem_delta = (next.mem_pct - prev.mem_pct).abs() / 100.0;
    let load_delta = ((next.load - prev.load).abs() / 2.0).clamp(0.0, 1.0);
    let cpu_temp_delta = ((next.cpu_temp - prev.cpu_temp).abs() / 20.0).clamp(0.0, 1.0);
    let gpu_usage_delta = (next.gpu_usage - prev.gpu_usage).abs() / 100.0;
    let gpu_temp_delta = ((next.gpu_temp - prev.gpu_temp).abs() / 20.0).clamp(0.0, 1.0);

    cpu_delta
        .max(mem_delta)
        .max(load_delta)
        .max(cpu_temp_delta)
        .max(gpu_usage_delta)
        .max(gpu_temp_delta)
        .clamp(0.0, 1.0)
}

pub(crate) fn adaptive_poll_interval_ms(change_score: f32) -> u64 {
    const MIN_MS: f32 = 500.0;
    const MAX_MS: f32 = 3_000.0;
    let score = change_score.clamp(0.0, 1.0);
    (MAX_MS - ((MAX_MS - MIN_MS) * score)) as u64
}

// ── Interpolation helpers ─────────────────────────────────────

pub(crate) fn lerp_f32(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

pub(crate) fn lerp_u64(from: u64, to: u64, t: f32) -> u64 {
    (from as f32 + (to as f32 - from as f32) * t).round() as u64
}

pub(crate) fn blend_optional_f32(current: Option<f32>, target: Option<f32>, t: f32) -> Option<f32> {
    match (current, target) {
        (Some(a), Some(b)) => Some(lerp_f32(a, b, t)),
        (_, value) => value,
    }
}

pub(crate) fn blend_optional_f64(current: Option<f64>, target: Option<f64>, t: f32) -> Option<f64> {
    match (current, target) {
        (Some(a), Some(b)) => Some(lerp_f32(a as f32, b as f32, t) as f64),
        (_, value) => value,
    }
}

pub(crate) fn blend_gpu_info(
    current: &Option<crate::system::GpuInfo>,
    target: &Option<crate::system::GpuInfo>,
    t: f32,
) -> Option<crate::system::GpuInfo> {
    match (current, target) {
        (Some(cur), Some(next)) => Some(crate::system::GpuInfo {
            name: if next.name.is_empty() {
                cur.name.clone()
            } else {
                next.name.clone()
            },
            vendor: if next.vendor.is_empty() {
                cur.vendor.clone()
            } else {
                next.vendor.clone()
            },
            usage: blend_optional_f32(cur.usage, next.usage, t),
            temperature: blend_optional_f32(cur.temperature, next.temperature, t),
            vram_used: match (cur.vram_used, next.vram_used) {
                (Some(a), Some(b)) => Some(lerp_u64(a, b, t)),
                (_, value) => value,
            },
            vram_total: next.vram_total.or(cur.vram_total),
            power_watts: blend_optional_f32(cur.power_watts, next.power_watts, t),
            power_limit_watts: next.power_limit_watts.or(cur.power_limit_watts),
        }),
        (_, value) => value.clone(),
    }
}

pub(crate) fn blend_system_info(
    current: &mut crate::system::SystemInfo,
    target: &crate::system::SystemInfo,
    t: f32,
) {
    current.cpu_usage = lerp_f32(current.cpu_usage, target.cpu_usage, t);
    current.cpu_cores = target.cpu_cores;
    current.cpu_temperature =
        blend_optional_f32(current.cpu_temperature, target.cpu_temperature, t);
    current.cpu_frequency_mhz = target.cpu_frequency_mhz;
    current.memory_used = lerp_u64(current.memory_used, target.memory_used, t);
    current.memory_total = target.memory_total;
    current.system_uptime = target.system_uptime;
    current.process_count = target.process_count;
    current.swap_used = lerp_u64(current.swap_used, target.swap_used, t);
    current.swap_total = target.swap_total;
    current.load_average = blend_optional_f64(current.load_average, target.load_average, t);
    current.gpu_info = blend_gpu_info(&current.gpu_info, &target.gpu_info, t);
    current.power_watts = blend_optional_f32(current.power_watts, target.power_watts, t);
    current.power_limit_watts = target.power_limit_watts;
    current.power_source = target.power_source;
}

/// True while the displayed system snapshot is still blending toward its
/// target — i.e. the dashboard would visibly move on the next draw.
/// Integers snap in [`blend_system_info`], so only the lerped floats need an
/// epsilon (invisible below it); everything else compares exactly.
pub(crate) fn system_settling(
    current: &crate::system::SystemInfo,
    target: &crate::system::SystemInfo,
) -> bool {
    (current.cpu_usage - target.cpu_usage).abs() > 0.05
        || current.cpu_cores != target.cpu_cores
        || opt_f32_settling(current.cpu_temperature, target.cpu_temperature)
        || current.cpu_frequency_mhz != target.cpu_frequency_mhz
        || current.memory_used != target.memory_used
        || current.memory_total != target.memory_total
        || current.system_uptime != target.system_uptime
        || current.process_count != target.process_count
        || current.swap_used != target.swap_used
        || current.swap_total != target.swap_total
        || opt_f64_settling(current.load_average, target.load_average)
        || gpu_settling(current.gpu_info.as_ref(), target.gpu_info.as_ref())
        || opt_f32_settling(current.power_watts, target.power_watts)
        || current.power_limit_watts != target.power_limit_watts
        || current.power_source != target.power_source
}

fn opt_f32_settling(current: Option<f32>, target: Option<f32>) -> bool {
    match (current, target) {
        (Some(a), Some(b)) => (a - b).abs() > 0.05,
        (None, None) => false,
        _ => true,
    }
}

fn opt_f64_settling(current: Option<f64>, target: Option<f64>) -> bool {
    match (current, target) {
        (Some(a), Some(b)) => (a - b).abs() > 0.01,
        (None, None) => false,
        _ => true,
    }
}

fn gpu_settling(
    current: Option<&crate::system::GpuInfo>,
    target: Option<&crate::system::GpuInfo>,
) -> bool {
    match (current, target) {
        (Some(a), Some(b)) => {
            a.name != b.name
                || a.vendor != b.vendor
                || opt_f32_settling(a.usage, b.usage)
                || opt_f32_settling(a.temperature, b.temperature)
                || a.vram_used != b.vram_used
                || a.vram_total != b.vram_total
                || opt_f32_settling(a.power_watts, b.power_watts)
                || a.power_limit_watts != b.power_limit_watts
        }
        (None, None) => false,
        _ => true,
    }
}

// ── Playground vector search (worker-thread body) ─────────────

/// Worker-thread body of the playground search (B23): loads/uses an
/// embedding client and queries the vector store on its own current-thread
/// runtime — a cold lazy model can take seconds to load, and none of that
/// may run on the UI thread.
pub(crate) fn playground_vector_search(
    query: &str,
    top_k: usize,
) -> anyhow::Result<Vec<crate::rag::vector_store::SearchResult>> {
    let canopy_dir = dirs::home_dir()
        .map(|h| h.join(".canopy"))
        .ok_or_else(|| anyhow::anyhow!("No home directory"))?;
    let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
    let model = config.embeddings_model.trim();
    if model.is_empty() {
        return Ok(Vec::new());
    }
    let dimensions = crate::rag::embedding_client::model_dimensions(model)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let store = crate::rag::vector_store::VectorStore::new(
            dimensions,
            Some(config.rag_vector_cache_entries),
        )
        .await?;
        let embedder = crate::rag::embedding_client::client_from_config(&config)?;
        let query_vec = embedder.embed(query)?;
        store.search_similar(&query_vec, top_k).await
    })
}

// ── System monitor thread ────────────────────────────────────

pub(crate) fn spawn_system_monitor(
    system_monitor_active: &Arc<std::sync::atomic::AtomicBool>,
) -> std::sync::mpsc::Receiver<crate::system::SystemInfo> {
    let system_monitor_active_bg = Arc::clone(system_monitor_active);
    let (system_info_tx, system_info_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let initial = crate::system::SystemInfo::new();
        let mut previous_sample = sample_from(&initial);
        let mut poll_interval_ms = adaptive_poll_interval_ms(0.3);
        let mut was_active = true;
        let _ = system_info_tx.send(initial);

        loop {
            if !system_monitor_active_bg.load(Ordering::Relaxed) {
                was_active = false;
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }

            if !was_active {
                // Immediate catch-up sample after becoming visible again.
                let mut info = crate::system::SystemInfo::default();
                info.update();
                previous_sample = sample_from(&info);
                poll_interval_ms = adaptive_poll_interval_ms(0.6);
                let _ = system_info_tx.send(info);
                was_active = true;
            }

            std::thread::sleep(std::time::Duration::from_millis(poll_interval_ms));
            let mut info = crate::system::SystemInfo::default();
            info.update();

            let current_sample = sample_from(&info);
            let change_score = adaptive_change_score(previous_sample, current_sample);
            let target_ms = adaptive_poll_interval_ms(change_score) as f32;
            poll_interval_ms = (poll_interval_ms as f32 * 0.6 + target_ms * 0.4) as u64;
            previous_sample = current_sample;

            let _ = system_info_tx.send(info);
        }
    });
    system_info_rx
}

// ── CLI usage loader ──────────────────────────────────────────

pub(crate) fn load_cli_usage() -> crate::domain::usage_stats::CliUsage {
    let mut usage = dirs::home_dir()
        .map(|h| crate::domain::usage_stats::CliUsage::load(&h.join(".canopy")))
        .unwrap_or_default();
    if usage.ensure_first_run() {
        let _ = dirs::home_dir().and_then(|h| usage.save(&h.join(".canopy")).ok().map(|_| ()));
    }
    usage
}

// ── Log-analysis free functions ───────────────────────────────

pub(crate) fn calculate_log_hash(raw_log: &str) -> u64 {
    raw_log.bytes().enumerate().fold(0u64, |acc, (idx, byte)| {
        acc.wrapping_add((byte as u64).wrapping_mul(idx as u64 + 1))
    })
}

pub(crate) fn log_contains_error(log_up: &str) -> bool {
    [
        "ERROR",
        "FAILED",
        "EXCEPTION",
        "PANIC",
        "SEGFAULT",
        "TIMED OUT",
        "CONNECTION REFUSED",
        "PERMISSION DENIED",
        "HALTED",
        "PROBLEMA",
        "FALLO",
        "FALLANDO",
    ]
    .iter()
    .any(|kw| log_up.contains(kw))
}

pub(crate) fn log_contains_success(log_up: &str) -> bool {
    [
        "SUCCESS",
        "ALL TESTS PASSED",
        "BUILD SUCCEEDED",
        "FINISHED",
        "COMPLETED",
        "DONE.",
        "STABILIZED",
        "READY",
        "CONVERGED",
        "DEPLOYED",
        "EXCELENTE",
        "COMPLETADO",
        "HECHO",
        "LISTO",
        "TERMINADO",
    ]
    .iter()
    .any(|kw| log_up.contains(kw))
}

pub(crate) fn log_contains_spawn(log_up: &str) -> bool {
    ["SPAWNING", "STARTING UP", "BOOTSTRAPPING", "INITIALIZING"]
        .iter()
        .any(|kw| log_up.contains(kw))
}
