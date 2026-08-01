//! Turning measurements into findings.
//!
//! Every rule is a pure function over `(FlowState, HwSnapshot)` so it can be
//! tested against synthetic input — including the cases that matter most, which
//! are the ones where a rule must *not* fire.
//!
//! Two principles run through all of them:
//!
//! 1. **A missing measurement is not evidence.** Where a counter is `None` the
//!    rule declines to fire rather than assuming a default. An unknown PCIe
//!    width must never produce "your link is degraded".
//! 2. **Every finding names a fix.** A diagnosis the user cannot act on is
//!    noise. Where the fix is a dlm flag, the flag is named, because the
//!    settings panel is one click away.

use crate::flow::FlowState;
use crate::hw::HwSnapshot;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// Worth knowing; not costing anything.
    Info,
    /// Costing throughput, and fixable.
    Warning,
    /// Actively broken or about to be.
    Critical,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// Stable identifier, so the UI can deduplicate across polls.
    pub id: &'static str,
    pub severity: Severity,
    pub title: String,
    /// What was measured, in the user's terms.
    pub detail: String,
    /// What to do about it.
    pub fix: Option<String>,
}

/// Ratio of transfer time to compute time above which a run is bus-bound.
///
/// Not 1.0: streaming is *expected* to cost more than compute on this engine,
/// and flagging that would fire on every healthy streamed run. 2.0 is the point
/// where the transfer is clearly dominant rather than merely larger.
const BUS_BOUND_RATIO: f64 = 2.0;

/// Below this hit rate, a configured RAM cache is not earning its memory.
const RAM_CACHE_USELESS_RATE: f64 = 0.05;

/// VRAM use above this fraction of total is close enough to OOM to say so.
const VRAM_PRESSURE: f64 = 0.92;

/// Evaluate every rule.
pub fn analyze(flow: &FlowState, hw: &HwSnapshot) -> Vec<Finding> {
    let mut out = Vec::new();
    pcie_link(&mut out, hw);
    thermal_throttle(&mut out, hw);
    bus_bound(&mut out, flow);
    ram_cache_ineffective(&mut out, flow);
    prefetch_starved(&mut out, flow);
    window_thrashing(&mut out, flow);
    vram_pressure(&mut out, hw);
    sampling(&mut out, flow);

    // Most severe first — the UI shows a truncated list.
    out.sort_by_key(|f| std::cmp::Reverse(f.severity));
    out
}

fn pcie_link(out: &mut Vec<Finding>, hw: &HwSnapshot) {
    for gpu in &hw.gpus {
        if !gpu.link_is_degraded() {
            continue;
        }
        let cur = describe_link(gpu.pcie_link_gen, gpu.pcie_link_width);
        let max = describe_link(gpu.pcie_max_link_gen, gpu.pcie_max_link_width);
        out.push(Finding {
            id: "pcie-link-degraded",
            severity: Severity::Warning,
            title: format!("PCIe link running at {cur}"),
            detail: format!(
                "{} negotiated {cur} but supports {max}. Layer streaming crosses this link \
                 every token, so a narrower link costs throughput in direct proportion.",
                gpu.name
            ),
            fix: Some(
                "Check the slot (x16 rather than a chipset x4), the riser cable, and whether \
                 another card is sharing lanes."
                    .into(),
            ),
        });
    }
}

fn thermal_throttle(out: &mut Vec<Finding>, hw: &HwSnapshot) {
    for gpu in &hw.gpus {
        if gpu.throttle_reasons.is_empty() {
            continue;
        }
        let temp = gpu
            .temperature_c
            .map(|t| format!(" at {t}°C"))
            .unwrap_or_default();
        let clocks = match (gpu.clock_mhz, gpu.max_clock_mhz) {
            (Some(cur), Some(max)) if cur < max => format!(" — clocks {cur} MHz of {max} MHz"),
            _ => String::new(),
        };
        out.push(Finding {
            id: "gpu-throttled",
            severity: Severity::Warning,
            title: format!(
                "{} is throttled ({})",
                gpu.name,
                gpu.throttle_reasons.join(", ")
            ),
            detail: format!(
                "The card is not running at full speed{temp}{clocks}. Compute time is inflated, \
                 which makes the run look more compute-bound than it is."
            ),
            fix: Some(
                "Improve airflow, or raise the power/thermal limit if the card allows it.".into(),
            ),
        });
    }
}

fn bus_bound(out: &mut Vec<Finding>, flow: &FlowState) {
    let Some(ratio) = flow.transfer_vs_compute() else {
        return;
    };
    if ratio < BUS_BOUND_RATIO {
        return;
    }
    let pct =
        100.0 * flow.transfer_us() as f64 / (flow.transfer_us() + flow.compute.total_us) as f64;
    out.push(Finding {
        id: "bus-bound",
        severity: Severity::Warning,
        title: format!("Bus-bound: {pct:.0}% of layer time is moving bytes"),
        detail: format!(
            "Transfer is taking {ratio:.1}x as long as compute. You are paying more to move \
             weights than to use them, which is what streaming costs when the resident window \
             is small."
        ),
        fix: Some(
            "Send fewer bytes: --quant int4 shrinks each layer 4x, which usually matters far \
             more than any other tuning. Raising --resident-layers helps only if the layers fit."
                .into(),
        ),
    });
}

fn ram_cache_ineffective(out: &mut Vec<Finding>, flow: &FlowState) {
    let hits = flow.ram_hit.events;
    let misses = flow.ram_miss.events;
    let total = hits + misses;
    // Needs enough samples to mean anything: the first pass over a model is all
    // misses by definition, and firing on that would be noise.
    if total < 32 {
        return;
    }
    let rate = hits as f64 / total as f64;
    if rate >= RAM_CACHE_USELESS_RATE {
        return;
    }
    out.push(Finding {
        id: "ram-cache-ineffective",
        severity: Severity::Info,
        title: format!("Host RAM cache hit rate {:.0}%", rate * 100.0),
        detail: format!(
            "{misses} of {total} layer fetches missed the cache. Layers are touched in order, \
             so an LRU smaller than the model evicts each layer just before it comes round \
             again — the cache holds memory without saving a read."
        ),
        fix: Some(
            "Either raise --ram-cache-gb above the model's total layer bytes so a full cycle \
             fits, or set it to 0 and reclaim the memory. A value in between buys little."
                .into(),
        ),
    });
}

fn prefetch_starved(out: &mut Vec<Finding>, flow: &FlowState) {
    // The engine queues prefetch requests (depth events, no bytes) and reports
    // landed prefetches separately (bytes > 0). Requests without landings mean
    // the worker never gets ahead.
    let requested = flow.prefetch.events;
    if requested < 16 {
        return;
    }
    let landed = flow.prefetch.bytes > 0;
    if landed {
        return;
    }
    out.push(Finding {
        id: "prefetch-starved",
        severity: Severity::Info,
        title: "Prefetch is not getting ahead".into(),
        detail: format!(
            "{requested} prefetch requests were queued in this window but none completed ahead \
             of demand, so no load latency is being hidden behind compute."
        ),
        fix: Some(
            "Raise --prefetch-depth, or use --auto-prefetch to tune it from measured \
             load-versus-compute time. Depth is clamped to the window size minus one, so a \
             window of 1 cannot prefetch at all."
                .into(),
        ),
    });
}

fn window_thrashing(out: &mut Vec<Finding>, flow: &FlowState) {
    let Some(snapshot) = &flow.snapshot else {
        return;
    };
    if !snapshot.streaming || snapshot.num_layers == 0 {
        return;
    }
    let evictions = flow.evict.events;
    // Every materialization, whether it reached the disk or not.
    let loads = flow.ram_miss.events + flow.ram_hit.events;
    if loads < snapshot.num_layers as u64 {
        return;
    }
    // Roughly one eviction per load means nothing is ever reused: every layer
    // is gone before it comes round again.
    if (evictions as f64) < 0.9 * loads as f64 {
        return;
    }
    out.push(Finding {
        id: "window-thrashing",
        severity: Severity::Info,
        title: "Resident window has no reuse".into(),
        detail: format!(
            "{evictions} evictions against {loads} loads: {} of {} layers stay resident, so a \
             layer is evicted long before it is needed again. The window is not caching — its \
             only job here is overlapping the next load with the current compute.",
            snapshot.resident_layers, snapshot.num_layers
        ),
        fix: Some(
            "Only two things change this: fewer bytes per layer (--quant int4) or more VRAM for \
             the window (--vram-budget-gb, or a lower --safety-margin-gb on a small card)."
                .into(),
        ),
    });
}

fn vram_pressure(out: &mut Vec<Finding>, hw: &HwSnapshot) {
    for gpu in &hw.gpus {
        let (Some(used), Some(total)) = (gpu.vram_used, gpu.vram_total) else {
            continue;
        };
        if total == 0 {
            continue;
        }
        let frac = used as f64 / total as f64;
        if frac < VRAM_PRESSURE {
            continue;
        }
        out.push(Finding {
            id: "vram-pressure",
            severity: Severity::Critical,
            title: format!("{} VRAM {:.0}% full", gpu.name, frac * 100.0),
            detail: format!(
                "{:.1} GiB of {:.1} GiB in use. An activation spike from here is an \
                 out-of-memory failure mid-generation.",
                used as f64 / (1u64 << 30) as f64,
                total as f64 / (1u64 << 30) as f64
            ),
            fix: Some(
                "Raise --safety-margin-gb, lower --resident-layers, or reduce --max-batch \
                 (each in-flight sequence holds its own KV cache)."
                    .into(),
            ),
        });
    }
}

fn sampling(out: &mut Vec<Finding>, flow: &FlowState) {
    if flow.sampled_fraction >= 0.999 {
        return;
    }
    out.push(Finding {
        id: "telemetry-sampled",
        severity: Severity::Info,
        title: format!("Telemetry sampled at {:.0}%", flow.sampled_fraction * 100.0),
        detail: format!(
            "The engine dropped {} events because they were produced faster than they could be \
             read. The figures shown are a sample, not a complete record.",
            flow.dropped
        ),
        fix: None,
    });
}

fn describe_link(gen: Option<u32>, width: Option<u32>) -> String {
    match (gen, width) {
        (Some(g), Some(w)) => format!("Gen{g} x{w}"),
        (Some(g), None) => format!("Gen{g}"),
        (None, Some(w)) => format!("x{w}"),
        (None, None) => "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::StageStat;
    use crate::hw::GpuInfo;
    use crate::telemetry::Snapshot;

    fn stat(events: u64, bytes: u64, total_us: u64) -> StageStat {
        StageStat {
            events,
            bytes,
            total_us,
            bytes_per_sec: None,
            estimated: false,
        }
    }

    fn found<'a>(f: &'a [Finding], id: &str) -> Option<&'a Finding> {
        f.iter().find(|x| x.id == id)
    }

    // --- PCIe -----------------------------------------------------------

    #[test]
    fn a_narrowed_link_is_reported_with_both_figures() {
        let hw = HwSnapshot {
            gpus: vec![GpuInfo {
                name: "Test GPU".into(),
                pcie_link_width: Some(4),
                pcie_max_link_width: Some(16),
                pcie_link_gen: Some(4),
                pcie_max_link_gen: Some(4),
                ..Default::default()
            }],
            ..Default::default()
        };
        let f = analyze(&FlowState::default(), &hw);
        let hit = found(&f, "pcie-link-degraded").expect("should fire");
        assert!(hit.title.contains("Gen4 x4"), "{}", hit.title);
        assert!(hit.detail.contains("Gen4 x16"), "{}", hit.detail);
        assert!(hit.fix.is_some());
    }

    #[test]
    fn a_full_link_does_not_fire() {
        let hw = HwSnapshot {
            gpus: vec![GpuInfo {
                pcie_link_width: Some(16),
                pcie_max_link_width: Some(16),
                pcie_link_gen: Some(4),
                pcie_max_link_gen: Some(4),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(found(&analyze(&FlowState::default(), &hw), "pcie-link-degraded").is_none());
    }

    /// The rule this module exists to enforce: no measurement, no finding.
    #[test]
    fn an_unknown_link_does_not_fire() {
        let hw = HwSnapshot {
            gpus: vec![GpuInfo::default()],
            ..Default::default()
        };
        assert!(found(&analyze(&FlowState::default(), &hw), "pcie-link-degraded").is_none());
    }

    // --- bus-bound ------------------------------------------------------

    #[test]
    fn a_transfer_dominated_run_is_flagged_bus_bound() {
        let flow = FlowState {
            ram_miss: stat(28, 800_000_000, 470_000),
            compute: stat(28, 0, 60_000),
            ..Default::default()
        };
        let f = analyze(&flow, &HwSnapshot::default());
        let hit = found(&f, "bus-bound").expect("should fire");
        assert!(hit.fix.as_ref().unwrap().contains("--quant int4"));
    }

    /// Streaming legitimately costs more than compute; flagging every streamed
    /// run would make the panel useless.
    #[test]
    fn a_mildly_transfer_heavy_run_is_not_flagged() {
        let flow = FlowState {
            ram_miss: stat(28, 100, 15_000),
            compute: stat(28, 0, 10_000),
            ..Default::default()
        };
        assert!(found(&analyze(&flow, &HwSnapshot::default()), "bus-bound").is_none());
    }

    #[test]
    fn bus_bound_needs_both_sides_measured() {
        let flow = FlowState {
            ram_miss: stat(28, 100, 470_000),
            compute: stat(0, 0, 0),
            ..Default::default()
        };
        assert!(found(&analyze(&flow, &HwSnapshot::default()), "bus-bound").is_none());
    }

    // --- RAM cache ------------------------------------------------------

    /// The exact case measured against the real engine: a 512 MiB cache over a
    /// 28-layer cyclic scan gets no hits at all.
    #[test]
    fn a_cache_that_never_hits_is_reported() {
        let flow = FlowState {
            ram_hit: stat(0, 0, 0),
            ram_miss: stat(449, 14_000_000_000, 20_000_000),
            ..Default::default()
        };
        let f = analyze(&flow, &HwSnapshot::default());
        let hit = found(&f, "ram-cache-ineffective").expect("should fire");
        assert!(hit.fix.as_ref().unwrap().contains("--ram-cache-gb"));
    }

    /// The same run with a cache big enough: 28 misses then 253 hits.
    #[test]
    fn a_cache_that_works_is_not_reported() {
        let flow = FlowState {
            ram_hit: stat(253, 8_000_000_000, 0),
            ram_miss: stat(28, 880_000_000, 1_300_000),
            ..Default::default()
        };
        assert!(found(
            &analyze(&flow, &HwSnapshot::default()),
            "ram-cache-ineffective"
        )
        .is_none());
    }

    /// A first pass is all misses by definition and must not fire.
    #[test]
    fn too_few_samples_does_not_fire() {
        let flow = FlowState {
            ram_hit: stat(0, 0, 0),
            ram_miss: stat(4, 100, 100),
            ..Default::default()
        };
        assert!(found(
            &analyze(&flow, &HwSnapshot::default()),
            "ram-cache-ineffective"
        )
        .is_none());
    }

    // --- prefetch -------------------------------------------------------

    #[test]
    fn queued_but_never_landed_prefetch_is_flagged() {
        let flow = FlowState {
            prefetch: stat(500, 0, 0),
            ..Default::default()
        };
        let f = analyze(&flow, &HwSnapshot::default());
        assert!(found(&f, "prefetch-starved").is_some());
    }

    #[test]
    fn landed_prefetch_is_not_flagged() {
        let flow = FlowState {
            prefetch: stat(500, 900_000_000, 0),
            ..Default::default()
        };
        assert!(found(&analyze(&flow, &HwSnapshot::default()), "prefetch-starved").is_none());
    }

    // --- thrashing ------------------------------------------------------

    #[test]
    fn one_eviction_per_load_is_reported_as_no_reuse() {
        let flow = FlowState {
            snapshot: Some(Snapshot {
                num_layers: 28,
                resident_layers: 3,
                streaming: true,
                ..Default::default()
            }),
            ram_miss: stat(280, 0, 0),
            evict: stat(278, 0, 0),
            ..Default::default()
        };
        let f = analyze(&flow, &HwSnapshot::default());
        let hit = found(&f, "window-thrashing").expect("should fire");
        assert!(hit.detail.contains("3 of 28"), "{}", hit.detail);
    }

    /// A fully-resident run evicts nothing and must stay silent.
    #[test]
    fn a_resident_run_is_not_reported_as_thrashing() {
        let flow = FlowState {
            snapshot: Some(Snapshot {
                num_layers: 28,
                resident_layers: 28,
                streaming: false,
                ..Default::default()
            }),
            ram_miss: stat(28, 0, 0),
            evict: stat(0, 0, 0),
            ..Default::default()
        };
        assert!(found(&analyze(&flow, &HwSnapshot::default()), "window-thrashing").is_none());
    }

    // --- VRAM -----------------------------------------------------------

    #[test]
    fn vram_near_full_is_critical() {
        let hw = HwSnapshot {
            gpus: vec![GpuInfo {
                name: "Test GPU".into(),
                vram_used: Some(7_800_000_000),
                vram_total: Some(8_000_000_000),
                ..Default::default()
            }],
            ..Default::default()
        };
        let f = analyze(&FlowState::default(), &hw);
        let hit = found(&f, "vram-pressure").expect("should fire");
        assert_eq!(hit.severity, Severity::Critical);
    }

    #[test]
    fn comfortable_vram_does_not_fire() {
        let hw = HwSnapshot {
            gpus: vec![GpuInfo {
                vram_used: Some(4_000_000_000),
                vram_total: Some(8_000_000_000),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(found(&analyze(&FlowState::default(), &hw), "vram-pressure").is_none());
    }

    // --- sampling + ordering --------------------------------------------

    #[test]
    fn dropped_events_are_disclosed() {
        let flow = FlowState {
            sampled_fraction: 0.82,
            dropped: 1800,
            ..Default::default()
        };
        let f = analyze(&flow, &HwSnapshot::default());
        let hit = found(&f, "telemetry-sampled").expect("should fire");
        assert!(hit.title.contains("82%"), "{}", hit.title);
    }

    #[test]
    fn complete_telemetry_is_not_mentioned() {
        let flow = FlowState {
            sampled_fraction: 1.0,
            ..Default::default()
        };
        assert!(found(&analyze(&flow, &HwSnapshot::default()), "telemetry-sampled").is_none());
    }

    #[test]
    fn findings_are_ordered_most_severe_first() {
        let flow = FlowState {
            sampled_fraction: 0.5,
            ram_miss: stat(28, 100, 470_000),
            compute: stat(28, 0, 60_000),
            ..Default::default()
        };
        let hw = HwSnapshot {
            gpus: vec![GpuInfo {
                vram_used: Some(7_900_000_000),
                vram_total: Some(8_000_000_000),
                ..Default::default()
            }],
            ..Default::default()
        };
        let f = analyze(&flow, &hw);
        assert_eq!(f[0].severity, Severity::Critical);
        assert!(f.windows(2).all(|w| w[0].severity >= w[1].severity));
    }

    /// A healthy, fully-resident run with a good GPU must produce nothing at
    /// all. A panel that always has something in it teaches the user to ignore
    /// it.
    #[test]
    fn a_healthy_run_produces_no_findings() {
        let flow = FlowState {
            snapshot: Some(Snapshot {
                num_layers: 28,
                resident_layers: 28,
                streaming: false,
                ..Default::default()
            }),
            compute: stat(280, 0, 100_000),
            sampled_fraction: 1.0,
            ..Default::default()
        };
        let hw = HwSnapshot {
            gpus: vec![GpuInfo {
                name: "Healthy GPU".into(),
                vram_used: Some(3_000_000_000),
                vram_total: Some(8_000_000_000),
                pcie_link_width: Some(16),
                pcie_max_link_width: Some(16),
                pcie_link_gen: Some(4),
                pcie_max_link_gen: Some(4),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(analyze(&flow, &hw).is_empty(), "{:?}", analyze(&flow, &hw));
    }
}
