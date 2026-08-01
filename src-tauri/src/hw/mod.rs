//! Host hardware counters.
//!
//! The flow view can say "the H2D stage took 47 ms". Only these counters can
//! say *why*: that the link negotiated at x4 instead of x16, or that the GPU is
//! thermally throttled, or that the NVMe is already saturated.
//!
//! # The rule this module exists to enforce
//!
//! Every field is optional and every backend degrades to `None` rather than a
//! plausible default. A fabricated PCIe width produces a confident, wrong
//! diagnosis — worse than no diagnosis, because the user acts on it. "Unknown"
//! is a legitimate answer and the diagnostics treat it as one.

use serde::Serialize;
use std::sync::Mutex;

/// A GPU as the driver reports it.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    pub name: String,
    pub vram_total: Option<u64>,
    pub vram_used: Option<u64>,
    pub utilization_pct: Option<u32>,
    pub temperature_c: Option<u32>,
    /// Current SM clock, MHz.
    pub clock_mhz: Option<u32>,
    /// Maximum SM clock the card will boost to, MHz.
    pub max_clock_mhz: Option<u32>,
    /// Why the card is not running at its maximum clock, if it is not.
    pub throttle_reasons: Vec<String>,
    /// Negotiated PCIe link, and what the hardware is capable of. A gap between
    /// them is the single most actionable finding this module can produce.
    pub pcie_link_width: Option<u32>,
    pub pcie_max_link_width: Option<u32>,
    pub pcie_link_gen: Option<u32>,
    pub pcie_max_link_gen: Option<u32>,
    /// Bytes/sec over PCIe as the driver counts them.
    pub pcie_tx_bytes_per_sec: Option<u64>,
    pub pcie_rx_bytes_per_sec: Option<u64>,
}

impl GpuInfo {
    /// Is the link narrower or slower than the hardware supports?
    pub fn link_is_degraded(&self) -> bool {
        let width_down = matches!(
            (self.pcie_link_width, self.pcie_max_link_width),
            (Some(cur), Some(max)) if cur < max
        );
        let gen_down = matches!(
            (self.pcie_link_gen, self.pcie_max_link_gen),
            (Some(cur), Some(max)) if cur < max
        );
        width_down || gen_down
    }

    /// Theoretical one-direction bandwidth of the negotiated link, bytes/sec.
    ///
    /// Per-lane figures are the standard encoded rates: Gen3 ~985 MB/s,
    /// Gen4 ~1.97 GB/s, Gen5 ~3.94 GB/s. Used to say "you are getting 40% of
    /// this link", which is only meaningful against the *negotiated* link.
    pub fn link_bandwidth(&self) -> Option<u64> {
        let width = self.pcie_link_width? as u64;
        let per_lane: u64 = match self.pcie_link_gen? {
            1 => 250_000_000,
            2 => 500_000_000,
            3 => 985_000_000,
            4 => 1_969_000_000,
            5 => 3_938_000_000,
            6 => 7_563_000_000,
            _ => return None,
        };
        Some(per_lane * width)
    }
}

/// CPU, memory and storage, from `sysinfo`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub cpu_pct: Option<f32>,
    pub ram_total: Option<u64>,
    pub ram_used: Option<u64>,
    /// Bytes/sec read by the engine process. This is the NVMe rate that matters
    /// — total disk activity would include everything else on the machine.
    pub engine_read_bytes_per_sec: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HwSnapshot {
    pub gpus: Vec<GpuInfo>,
    pub host: HostInfo,
    /// Why GPU counters are missing, when they are. Shown in the UI so the
    /// absence reads as a known limitation rather than a bug.
    pub gpu_unavailable_reason: Option<String>,
}

/// Anything that can report hardware state.
///
/// A trait rather than a function so the per-platform backends stay separable
/// and the diagnostics can be tested against a fake.
pub trait HwSource: Send + Sync {
    fn snapshot(&mut self) -> HwSnapshot;
}

/// The real backend for this platform.
pub struct SystemHw {
    system: sysinfo::System,
    nvml: NvmlState,
    /// Previous (timestamp, bytes-read) for the engine process, to difference
    /// into a rate.
    last_read: Option<(std::time::Instant, u64)>,
    /// PID of the engine process, when we own one.
    engine_pid: Option<u32>,
}

/// NVML is loaded lazily and its failure is remembered, so a machine with no
/// NVIDIA driver does not retry the load on every poll.
enum NvmlState {
    Uninit,
    Ready(Box<nvml_wrapper::Nvml>),
    Failed(String),
}

impl Default for SystemHw {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemHw {
    pub fn new() -> Self {
        Self {
            system: sysinfo::System::new(),
            nvml: NvmlState::Uninit,
            last_read: None,
            engine_pid: None,
        }
    }

    /// Track this process's disk reads as the engine's.
    pub fn set_engine_pid(&mut self, pid: Option<u32>) {
        if self.engine_pid != pid {
            // A new process starts a new rate series; differencing across a
            // restart would produce a nonsense spike.
            self.last_read = None;
        }
        self.engine_pid = pid;
    }

    fn nvml(&mut self) -> Option<&nvml_wrapper::Nvml> {
        if matches!(self.nvml, NvmlState::Uninit) {
            self.nvml = match nvml_wrapper::Nvml::init() {
                Ok(n) => NvmlState::Ready(Box::new(n)),
                Err(e) => NvmlState::Failed(e.to_string()),
            };
        }
        match &self.nvml {
            NvmlState::Ready(n) => Some(n),
            _ => None,
        }
    }

    fn nvml_error(&self) -> Option<String> {
        match &self.nvml {
            NvmlState::Failed(e) => Some(e.clone()),
            _ => None,
        }
    }
}

impl HwSource for SystemHw {
    fn snapshot(&mut self) -> HwSnapshot {
        let mut snap = HwSnapshot::default();

        // --- host ----------------------------------------------------------
        self.system.refresh_memory();
        self.system.refresh_cpu_usage();
        snap.host.ram_total = Some(self.system.total_memory());
        snap.host.ram_used = Some(self.system.used_memory());
        let cpus = self.system.cpus();
        if !cpus.is_empty() {
            let avg = cpus.iter().map(|c| c.cpu_usage()).sum::<f32>() / cpus.len() as f32;
            snap.host.cpu_pct = Some(avg);
        }

        if let Some(pid) = self.engine_pid {
            let pid = sysinfo::Pid::from_u32(pid);
            self.system.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                true,
                sysinfo::ProcessRefreshKind::new().with_disk_usage(),
            );
            if let Some(proc) = self.system.process(pid) {
                let total = proc.disk_usage().total_read_bytes;
                let now = std::time::Instant::now();
                if let Some((prev_at, prev_bytes)) = self.last_read {
                    let secs = now.duration_since(prev_at).as_secs_f64();
                    if secs > 0.0 && total >= prev_bytes {
                        snap.host.engine_read_bytes_per_sec =
                            Some(((total - prev_bytes) as f64 / secs) as u64);
                    }
                }
                self.last_read = Some((now, total));
            }
        }

        // --- gpu -----------------------------------------------------------
        if let Some(nvml) = self.nvml() {
            let count = nvml.device_count().unwrap_or(0);
            for i in 0..count {
                let Ok(dev) = nvml.device_by_index(i) else {
                    continue;
                };
                let mem = dev.memory_info().ok();
                let util = dev.utilization_rates().ok();
                let mut info = GpuInfo {
                    name: dev.name().unwrap_or_else(|_| format!("GPU {i}")),
                    vram_total: mem.as_ref().map(|m| m.total),
                    vram_used: mem.as_ref().map(|m| m.used),
                    utilization_pct: util.map(|u| u.gpu),
                    temperature_c: dev
                        .temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)
                        .ok(),
                    clock_mhz: dev
                        .clock_info(nvml_wrapper::enum_wrappers::device::Clock::SM)
                        .ok(),
                    max_clock_mhz: dev
                        .max_clock_info(nvml_wrapper::enum_wrappers::device::Clock::SM)
                        .ok(),
                    pcie_link_width: dev.current_pcie_link_width().ok(),
                    pcie_max_link_width: dev.max_pcie_link_width().ok(),
                    pcie_link_gen: dev.current_pcie_link_gen().ok(),
                    pcie_max_link_gen: dev.max_pcie_link_gen().ok(),
                    ..Default::default()
                };

                // Reported in KB/s.
                use nvml_wrapper::enum_wrappers::device::PcieUtilCounter;
                info.pcie_tx_bytes_per_sec = dev
                    .pcie_throughput(PcieUtilCounter::Send)
                    .ok()
                    .map(|kb| kb as u64 * 1024);
                info.pcie_rx_bytes_per_sec = dev
                    .pcie_throughput(PcieUtilCounter::Receive)
                    .ok()
                    .map(|kb| kb as u64 * 1024);

                if let Ok(reasons) = dev.current_throttle_reasons() {
                    info.throttle_reasons = describe_throttle(reasons);
                }
                snap.gpus.push(info);
            }
        }
        if snap.gpus.is_empty() {
            snap.gpu_unavailable_reason = Some(self.nvml_error().unwrap_or_else(|| {
                "no NVIDIA GPU detected (AMD and Apple GPUs are not yet instrumented)".into()
            }));
        }

        snap
    }
}

/// Human-readable throttle causes, keeping only the ones a user can act on.
///
/// `GpuIdle` and `ApplicationsClocksSetting` are omitted: neither indicates a
/// problem, and listing them would train the user to ignore this field.
fn describe_throttle(
    reasons: nvml_wrapper::bitmasks::device::ThrottleReasons,
) -> Vec<String> {
    use nvml_wrapper::bitmasks::device::ThrottleReasons as R;
    let mut out = Vec::new();
    if reasons.contains(R::SW_THERMAL_SLOWDOWN) || reasons.contains(R::HW_THERMAL_SLOWDOWN) {
        out.push("thermal".into());
    }
    if reasons.contains(R::HW_POWER_BRAKE_SLOWDOWN) || reasons.contains(R::SW_POWER_CAP) {
        out.push("power cap".into());
    }
    if reasons.contains(R::HW_SLOWDOWN) {
        out.push("hardware slowdown".into());
    }
    if reasons.contains(R::SYNC_BOOST) {
        out.push("sync boost".into());
    }
    out
}

/// Process-wide hardware poller.
pub struct HwMonitor {
    source: Mutex<Box<dyn HwSource>>,
}

impl HwMonitor {
    pub fn new() -> Self {
        Self {
            source: Mutex::new(Box::new(SystemHw::new())),
        }
    }

    pub fn snapshot(&self) -> HwSnapshot {
        self.source.lock().unwrap().snapshot()
    }
}

impl Default for HwMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_width_link_is_not_degraded() {
        let gpu = GpuInfo {
            pcie_link_width: Some(16),
            pcie_max_link_width: Some(16),
            pcie_link_gen: Some(4),
            pcie_max_link_gen: Some(4),
            ..Default::default()
        };
        assert!(!gpu.link_is_degraded());
    }

    #[test]
    fn a_narrowed_link_is_degraded() {
        let gpu = GpuInfo {
            pcie_link_width: Some(4),
            pcie_max_link_width: Some(16),
            ..Default::default()
        };
        assert!(gpu.link_is_degraded());
    }

    #[test]
    fn an_older_generation_link_is_degraded() {
        let gpu = GpuInfo {
            pcie_link_gen: Some(1),
            pcie_max_link_gen: Some(4),
            ..Default::default()
        };
        assert!(gpu.link_is_degraded());
    }

    /// Unknown must never read as degraded: this is the rule the whole module
    /// exists to enforce.
    #[test]
    fn unknown_link_state_is_not_reported_as_degraded() {
        assert!(!GpuInfo::default().link_is_degraded());
        let partial = GpuInfo {
            pcie_link_width: Some(4),
            pcie_max_link_width: None,
            ..Default::default()
        };
        assert!(!partial.link_is_degraded());
    }

    #[test]
    fn link_bandwidth_matches_the_generation_table() {
        let gpu = GpuInfo {
            pcie_link_width: Some(16),
            pcie_link_gen: Some(4),
            ..Default::default()
        };
        // Gen4 x16 ≈ 31.5 GB/s one direction.
        let bw = gpu.link_bandwidth().unwrap();
        assert!(
            (31_000_000_000..32_000_000_000).contains(&bw),
            "got {bw}"
        );
    }

    #[test]
    fn link_bandwidth_is_unknown_without_both_figures() {
        let gpu = GpuInfo {
            pcie_link_width: Some(16),
            pcie_link_gen: None,
            ..Default::default()
        };
        assert!(gpu.link_bandwidth().is_none());
    }

    /// The real backend must work on a machine with no GPU — which is most
    /// developer machines, and this one.
    #[test]
    fn the_system_backend_degrades_without_a_gpu() {
        let mut hw = SystemHw::new();
        let snap = hw.snapshot();
        if snap.gpus.is_empty() {
            assert!(
                snap.gpu_unavailable_reason.is_some(),
                "an absent GPU must explain itself"
            );
        }
        // Host counters should work everywhere.
        assert!(snap.host.ram_total.unwrap_or(0) > 0);
    }
}
