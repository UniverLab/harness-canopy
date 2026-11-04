//! Windows and WSL host metrics via PowerShell and WMI.

use serde::Deserialize;

use super::platform::{
    bytes_to_megabytes, infer_gpu_vendor, is_cpu_temperature_label, is_gpu_temperature_label,
    normalize_temperature, run_powershell_json,
};
use super::{GpuInfo, HostMetrics};
use crate::system::gpu::try_get_nvidia_gpu_info;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct WindowsHostSnapshot {
    pub cpu_temperature_c: Option<f32>,
    pub cpu_usage_percent: Option<f32>,
    pub installed_memory_bytes: Option<u64>,
    pub visible_memory_bytes: Option<u64>,
    pub free_memory_bytes: Option<u64>,
    pub gpu_usage_percent: Option<f32>,
    pub lhm_temps: Option<OneOrMany<HwmonSensor>>,
    pub ohm_temps: Option<OneOrMany<HwmonSensor>>,
}

/// One temperature sensor row from LibreHardwareMonitor / OpenHardwareMonitor.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct HwmonSensor {
    name: Option<String>,
    value: Option<f32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WindowsVideoController {
    name: Option<String>,
    adapter_compatibility: Option<String>,
    adapter_ram: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(item) => vec![item],
            Self::Many(items) => items,
        }
    }

    fn as_slice(&self) -> &[T] {
        match self {
            Self::One(item) => std::slice::from_ref(item),
            Self::Many(items) => items,
        }
    }
}

/// CPU/GPU temperatures picked out of a Windows host snapshot.
struct WindowsTemps {
    cpu: Option<f32>,
    cpu_source: Option<String>,
    gpu: Option<f32>,
}

/// Pick temperatures from the snapshot: CPU is LibreHardwareMonitor first,
/// then OpenHardwareMonitor, then the MSAcpi value; GPU uses the same two
/// hardware-monitor providers. Selection lives on the Rust side so canned
/// JSON can test it without LibreHardwareMonitor installed.
fn select_windows_temps(snapshot: &WindowsHostSnapshot) -> WindowsTemps {
    let cpu = find_sensor_temp(snapshot.lhm_temps.as_ref(), is_cpu_temperature_label)
        .map(|temp| (temp, "librehardwaremonitor"))
        .or_else(|| {
            find_sensor_temp(snapshot.ohm_temps.as_ref(), is_cpu_temperature_label)
                .map(|temp| (temp, "openhardwaremonitor"))
        })
        .or_else(|| normalize_temperature(snapshot.cpu_temperature_c).map(|temp| (temp, "msacpi")));
    let (cpu_temp, cpu_source) = cpu
        .map(|(temp, source)| (Some(temp), Some(source.to_string())))
        .unwrap_or((None, None));
    let gpu = find_sensor_temp(snapshot.lhm_temps.as_ref(), is_gpu_temperature_label)
        .or_else(|| find_sensor_temp(snapshot.ohm_temps.as_ref(), is_gpu_temperature_label));
    WindowsTemps {
        cpu: cpu_temp,
        cpu_source,
        gpu,
    }
}

fn find_sensor_temp(
    sensors: Option<&OneOrMany<HwmonSensor>>,
    matches: fn(&str) -> bool,
) -> Option<f32> {
    sensors?.as_slice().iter().find_map(|sensor| {
        let name = sensor.name.as_deref()?;
        if matches(name) {
            normalize_temperature(sensor.value)
        } else {
            None
        }
    })
}

/// Fold a hardware-monitor GPU temperature into `gpu_info`: an existing
/// reading (e.g. nvidia-smi) wins, otherwise fill the gap, creating the
/// `GpuInfo` when there is none yet.
fn merge_hwmon_gpu_temp(gpu_info: &mut Option<GpuInfo>, hwmon_temp: Option<f32>) {
    let Some(temp) = hwmon_temp else {
        return;
    };
    match gpu_info.as_mut() {
        Some(gpu) if gpu.temperature.is_none() => gpu.temperature = Some(temp),
        None => {
            *gpu_info = Some(GpuInfo {
                temperature: Some(temp),
                ..GpuInfo::default()
            });
        }
        _ => {}
    }
}

/// Collect CPU, memory, and GPU metrics from the Windows host.
pub(super) fn get_windows_host_metrics() -> HostMetrics {
    let snapshot = get_windows_host_snapshot();
    let temps = select_windows_temps(&snapshot);
    let mut gpu_info = try_get_nvidia_gpu_info().or_else(get_windows_gpu_info);
    merge_hwmon_gpu_temp(&mut gpu_info, temps.gpu);

    if let Some(usage) = snapshot.gpu_usage_percent {
        if let Some(gpu) = gpu_info.as_mut() {
            gpu.usage = Some(usage);
        }
    }

    let memory_total = snapshot
        .installed_memory_bytes
        .or(snapshot.visible_memory_bytes)
        .filter(|total| *total > 0);

    let memory_used = match (snapshot.visible_memory_bytes, snapshot.free_memory_bytes) {
        (Some(visible), Some(free)) if visible >= free => Some(visible - free),
        _ => None,
    }
    .map(|used| match memory_total {
        Some(total) => used.min(total),
        None => used,
    });

    HostMetrics {
        cpu_usage: snapshot.cpu_usage_percent,
        cpu_temperature: temps.cpu,
        cpu_temp_source: temps.cpu_source,
        memory_used,
        memory_total,
        gpu_info,
    }
}

fn get_windows_host_snapshot() -> WindowsHostSnapshot {
    let script = concat!(
        "$os = Get-CimInstance Win32_OperatingSystem; ",
        "$dimms = Get-CimInstance Win32_PhysicalMemory | Measure-Object -Property Capacity -Sum; ",
        "$cpuCounter = (Get-Counter '\\Processor(_Total)\\% Processor Time' -ErrorAction SilentlyContinue).CounterSamples | Select-Object -First 1 -ExpandProperty CookedValue; ",
        "$thermal = Get-CimInstance -Namespace root/wmi -Class MSAcpi_ThermalZoneTemperature -ErrorAction SilentlyContinue; ",
        "$lhm = Get-CimInstance -Namespace root/LibreHardwareMonitor -Class Sensor -ErrorAction SilentlyContinue | Where-Object { $_.SensorType -eq 'Temperature' } | Select-Object Name,Value; ",
        "$ohm = Get-CimInstance -Namespace root/OpenHardwareMonitor -Class Sensor -ErrorAction SilentlyContinue | Where-Object { $_.SensorType -eq 'Temperature' } | Select-Object Name,Value; ",
        "$cpuTemp = $null; ",
        "if ($thermal) { ",
        "  $temps = @($thermal | Where-Object { $_.CurrentTemperature -gt 0 } | ForEach-Object { ($_.CurrentTemperature / 10) - 273.15 }); ",
        "  if ($temps.Count -gt 0) { $cpuTemp = [math]::Round((($temps | Measure-Object -Average).Average), 1) } ",
        "} ",
        "$gpuUsage = $null; ",
        "try { ",
        "  $gpuCounters = (Get-Counter '\\GPU Engine(*)\\Utilization Percentage' -ErrorAction Stop).CounterSamples; ",
        "  $gpu3d = @($gpuCounters | Where-Object { $_.InstanceName -match 'engtype_3D' }); ",
        "  $samples = if ($gpu3d.Count -gt 0) { $gpu3d } else { $gpuCounters }; ",
        "  if ($samples.Count -gt 0) { ",
        "    $sum = ($samples | Measure-Object -Property CookedValue -Sum).Sum; ",
        "    if ($null -ne $sum) { $gpuUsage = [math]::Min([math]::Round([double]$sum, 1), 100) } ",
        "  } ",
        "} catch {} ",
        "[pscustomobject]@{",
        "CpuTemperatureC = $cpuTemp; ",
        "LhmTemps = $lhm; ",
        "OhmTemps = $ohm; ",
        "CpuUsagePercent = if ($null -ne $cpuCounter) { [math]::Round([double]$cpuCounter, 1) } else { $null }; ",
        "InstalledMemoryBytes = [uint64]($dimms.Sum); ",
        "VisibleMemoryBytes = [uint64]($os.TotalVisibleMemorySize * 1KB); ",
        "FreeMemoryBytes = [uint64]($os.FreePhysicalMemory * 1KB); ",
        "GpuUsagePercent = $gpuUsage",
        "} | ConvertTo-Json -Compress"
    );

    run_powershell_json::<WindowsHostSnapshot>(script).unwrap_or_default()
}

fn get_windows_gpu_info() -> Option<GpuInfo> {
    let script = concat!(
        "Get-CimInstance Win32_VideoController ",
        "| Where-Object { $_.Name -and $_.Name -notmatch 'Microsoft Basic|Remote Display|Hyper-V' } ",
        "| Select-Object Name,AdapterCompatibility,AdapterRAM ",
        "| ConvertTo-Json -Compress"
    );

    let controllers = run_powershell_json::<OneOrMany<WindowsVideoController>>(script)?
        .into_vec()
        .into_iter()
        .filter(|c| c.name.as_ref().is_some_and(|n| !n.trim().is_empty()))
        .collect::<Vec<_>>();

    let controller = controllers.into_iter().next()?;
    let name = controller.name?.trim().to_string();
    let vendor = controller
        .adapter_compatibility
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| infer_gpu_vendor(&name));

    Some(GpuInfo {
        name,
        vendor,
        vram_total: controller.adapter_ram.map(bytes_to_megabytes),
        ..GpuInfo::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_from(json: &str) -> WindowsHostSnapshot {
        serde_json::from_str(json).expect("canned snapshot should parse")
    }

    #[test]
    fn lhm_cpu_beats_ohm_and_msacpi() {
        let snapshot = snapshot_from(
            r#"{"CpuTemperatureC": 50.0,
                "LhmTemps": [{"Name": "CPU Package", "Value": 62.5}],
                "OhmTemps": {"Name": "CPU Package", "Value": 61.0}}"#,
        );
        let temps = select_windows_temps(&snapshot);
        assert_eq!(temps.cpu, Some(62.5));
        assert_eq!(temps.cpu_source.as_deref(), Some("librehardwaremonitor"));
    }

    #[test]
    fn ohm_cpu_beats_msacpi_when_lhm_absent() {
        let snapshot = snapshot_from(
            r#"{"CpuTemperatureC": 50.0,
                "LhmTemps": null,
                "OhmTemps": [{"Name": "Core (TCTL/TDIE)", "Value": 61.0}]}"#,
        );
        let temps = select_windows_temps(&snapshot);
        assert_eq!(temps.cpu, Some(61.0));
        assert_eq!(temps.cpu_source.as_deref(), Some("openhardwaremonitor"));
    }

    #[test]
    fn msacpi_cpu_used_when_no_hardware_monitor() {
        let snapshot = snapshot_from(r#"{"CpuTemperatureC": 50.0}"#);
        let temps = select_windows_temps(&snapshot);
        assert_eq!(temps.cpu, Some(50.0));
        assert_eq!(temps.cpu_source.as_deref(), Some("msacpi"));
    }

    #[test]
    fn invalid_msacpi_yields_no_cpu_temp() {
        let snapshot = snapshot_from(r#"{"CpuTemperatureC": 0.0}"#);
        let temps = select_windows_temps(&snapshot);
        assert!(temps.cpu.is_none());
        assert!(temps.cpu_source.is_none());
    }

    #[test]
    fn all_absent_yields_no_temps() {
        let snapshot = snapshot_from(r#"{}"#);
        let temps = select_windows_temps(&snapshot);
        assert!(temps.cpu.is_none());
        assert!(temps.cpu_source.is_none());
        assert!(temps.gpu.is_none());
    }

    #[test]
    fn non_cpu_labels_do_not_win_over_msacpi() {
        let snapshot = snapshot_from(
            r#"{"CpuTemperatureC": 50.0,
                "LhmTemps": [{"Name": "GPU Core", "Value": 70.0}]}"#,
        );
        let temps = select_windows_temps(&snapshot);
        assert_eq!(temps.cpu, Some(50.0));
        assert_eq!(temps.cpu_source.as_deref(), Some("msacpi"));
        assert_eq!(temps.gpu, Some(70.0));
    }

    #[test]
    fn gpu_package_sensor_does_not_become_cpu_temp() {
        let snapshot = snapshot_from(
            r#"{"CpuTemperatureC": 50.0,
                "LhmTemps": [{"Name": "GPU Package", "Value": 70.0}]}"#,
        );
        let temps = select_windows_temps(&snapshot);
        assert_eq!(temps.cpu, Some(50.0));
        assert_eq!(temps.cpu_source.as_deref(), Some("msacpi"));
        assert_eq!(temps.gpu, Some(70.0));
    }

    #[test]
    fn gpu_prefers_lhm_over_ohm() {
        let snapshot = snapshot_from(
            r#"{"LhmTemps": [{"Name": "GPU Core", "Value": 70.0}],
                "OhmTemps": [{"Name": "GPU Core", "Value": 71.0}]}"#,
        );
        let temps = select_windows_temps(&snapshot);
        assert_eq!(temps.gpu, Some(70.0));
    }

    #[test]
    fn existing_gpu_temp_wins_over_hwmon() {
        let mut gpu_info = Some(GpuInfo {
            temperature: Some(55.0),
            ..GpuInfo::default()
        });
        merge_hwmon_gpu_temp(&mut gpu_info, Some(70.0));
        assert_eq!(gpu_info.as_ref().and_then(|g| g.temperature), Some(55.0));
    }

    #[test]
    fn hwmon_gpu_temp_fills_missing_reading() {
        let mut gpu_info = Some(GpuInfo {
            name: "Test GPU".to_string(),
            ..GpuInfo::default()
        });
        merge_hwmon_gpu_temp(&mut gpu_info, Some(70.0));
        let gpu = gpu_info.as_ref().expect("gpu info should remain");
        assert_eq!(gpu.temperature, Some(70.0));
        assert_eq!(gpu.name, "Test GPU");
    }

    #[test]
    fn hwmon_gpu_temp_creates_gpu_info_when_none() {
        let mut gpu_info = None;
        merge_hwmon_gpu_temp(&mut gpu_info, Some(70.0));
        assert_eq!(gpu_info.as_ref().and_then(|g| g.temperature), Some(70.0));
    }

    #[test]
    fn absent_hwmon_gpu_temp_changes_nothing() {
        let mut gpu_info = None;
        merge_hwmon_gpu_temp(&mut gpu_info, None);
        assert!(gpu_info.is_none());
    }
}
