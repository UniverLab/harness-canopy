//! Host system metrics for paired devices: the sidebar `SysInfo` sample in a
//! remote-shaped struct.
//!
//! [`snapshot`] serves the sampler's last sample without sampling on the
//! caller: the first call spawns one background thread that refreshes the
//! cache every 10 seconds, so any number of remote watchers share one
//! cadence and never start extra sampler processes of their own.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;

use super::{GpuInfo, PowerSource, SystemInfo};

/// GPU slice of [`HostMetrics`]; absent values are `None` (null on the wire).
#[derive(Debug, Default, Clone, Serialize)]
pub struct GpuMetrics {
    pub name: Option<String>,
    pub usage: Option<f32>,
    pub temperature_c: Option<f32>,
    pub vram_used_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
}

/// Power slice of [`HostMetrics`]: `source` is `"battery"` or `"gpu"`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct PowerMetrics {
    pub watts: Option<f32>,
    pub limit_watts: Option<f32>,
    pub source: Option<String>,
}

/// One host sample for the `host.metrics` frame: eight keys, `None` (null)
/// wherever the sampler has no reading. Nothing is skipped on serialize:
/// absent keys stay present as null.
#[derive(Debug, Default, Clone, Serialize)]
pub struct HostMetrics {
    pub cpu_usage: Option<f32>,
    pub cpu_temperature_c: Option<f32>,
    pub cpu_temp_source: Option<String>,
    pub memory_used_mb: Option<u64>,
    pub memory_total_mb: Option<u64>,
    pub gpu: Option<GpuMetrics>,
    pub power: Option<PowerMetrics>,
    pub sampled_at: Option<String>,
}

impl HostMetrics {
    /// Shape one [`SystemInfo`] sample for the wire. Memory divides to MiB;
    /// a zero total (unknown host memory) reads as absent rather than zero.
    /// `sampled_at` is now, RFC 3339.
    pub fn from_system_info(info: &SystemInfo) -> Self {
        let (memory_used_mb, memory_total_mb) = if info.memory_total > 0 {
            (
                Some(info.memory_used / 1_048_576),
                Some(info.memory_total / 1_048_576),
            )
        } else {
            (None, None)
        };
        Self {
            cpu_usage: Some(info.cpu_usage),
            cpu_temperature_c: info.cpu_temperature,
            cpu_temp_source: info.cpu_temp_source.clone(),
            memory_used_mb,
            memory_total_mb,
            gpu: info.gpu_info.as_ref().map(gpu_metrics),
            power: power_metrics(info),
            sampled_at: Some(Utc::now().to_rfc3339()),
        }
    }
}

/// Shape the GPU slice; the sampler's VRAM figures are already MiB.
fn gpu_metrics(gpu: &GpuInfo) -> GpuMetrics {
    GpuMetrics {
        name: (!gpu.name.is_empty()).then(|| gpu.name.clone()),
        usage: gpu.usage,
        temperature_c: gpu.temperature,
        vram_used_mb: gpu.vram_used,
        vram_total_mb: gpu.vram_total,
    }
}

/// Shape the power slice, present when the sample names any power reading.
fn power_metrics(info: &SystemInfo) -> Option<PowerMetrics> {
    if info.power_watts.is_none() && info.power_limit_watts.is_none() && info.power_source.is_none()
    {
        return None;
    }
    Some(PowerMetrics {
        watts: info.power_watts,
        limit_watts: info.power_limit_watts,
        source: info.power_source.map(|source| match source {
            PowerSource::Battery => "battery".to_string(),
            PowerSource::Gpu => "gpu".to_string(),
        }),
    })
}

fn cache() -> &'static Mutex<Option<HostMetrics>> {
    static CACHE: OnceLock<Mutex<Option<HostMetrics>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// The sampler's last sample, or a null sample before the first refresh.
/// The first call spawns the one sampler thread (a [`SystemInfo`] refresh
/// every 10 seconds); later calls only read the cache.
pub fn snapshot() -> HostMetrics {
    static START: std::sync::Once = std::sync::Once::new();
    START.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("host-metrics-sampler".to_string())
            .spawn(sample_loop);
    });
    cache()
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_default()
}

fn sample_loop() {
    let mut info = SystemInfo::new();
    loop {
        let sample = HostMetrics::from_system_info(&info);
        if let Ok(mut guard) = cache().lock() {
            *guard = Some(sample);
        }
        std::thread::sleep(Duration::from_secs(10));
        info.update();
    }
}
