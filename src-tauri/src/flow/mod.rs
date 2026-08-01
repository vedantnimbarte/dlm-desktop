//! Aggregates raw telemetry events into the state the flow view renders.
//!
//! Aggregation lives in Rust rather than the frontend for one reason: the
//! diagnostic rules in [`crate::diag`] are pure functions over this state, and
//! that makes them unit-testable against synthetic input. A rule that only
//! exists inside a React component can only be tested by driving a browser.
//!
//! Everything is computed over a **rolling window** rather than since-start
//! totals. A run that was bus-bound for its first ten seconds and is now
//! compute-bound should read as compute-bound; cumulative averages would hide
//! the change for minutes.

use crate::telemetry::{FlowEvent, Snapshot, Stage};
use serde::Serialize;
use std::collections::VecDeque;

/// How much history the rolling window keeps.
///
/// Long enough to smooth per-layer jitter on a slow streamed run (a single
/// layer load can take 50 ms), short enough that a change in behaviour shows up
/// while the user is still looking at it.
const WINDOW_US: u64 = 5_000_000; // 5 s

/// Hard cap on retained events, in case the engine outruns the window.
const MAX_EVENTS: usize = 20_000;

/// What a layer is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Residency {
    /// Never seen, or evicted and not yet reloaded.
    Absent,
    /// Materialized and available.
    Resident,
    /// Loaded ahead of demand by the prefetch worker.
    Prefetched,
    /// Most recent event was an eviction.
    Evicted,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerState {
    pub layer: u32,
    pub residency: Residency,
    /// Microseconds of engine time when this layer was last touched.
    pub last_seen_us: u64,
    /// Times this layer was evicted within the window — the thrash signal.
    pub evictions: u32,
    pub bytes: u64,
}

/// Totals for one stage over the window.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageStat {
    pub events: u64,
    pub bytes: u64,
    pub total_us: u64,
    /// Bytes per second **while this stage was running**, not wall-clock
    /// averaged. `None` when the stage moves no bytes (compute, evict) or
    /// recorded no time.
    pub bytes_per_sec: Option<f64>,
    /// Any contributing event carried a wall-clock rather than device-measured
    /// duration. The UI renders these edges as estimates.
    pub estimated: bool,
}

/// The complete picture the flow view draws.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowState {
    pub snapshot: Option<Snapshot>,
    pub mmap_read: StageStat,
    pub ram_hit: StageStat,
    pub ram_miss: StageStat,
    pub dequant: StageStat,
    pub pin_stage: StageStat,
    pub h2d: StageStat,
    pub compute: StageStat,
    pub evict: StageStat,
    pub prefetch: StageStat,
    pub layers: Vec<LayerState>,
    /// Live prefetch depth, as last reported.
    pub prefetch_depth: Option<u32>,
    /// Fraction of events that reached us, from the engine's drop counter.
    /// `1.0` means nothing was lost.
    pub sampled_fraction: f64,
    pub dropped: u64,
    /// Highest decode step observed.
    pub token: u64,
    /// Seconds of engine time covered by these totals.
    pub window_secs: f64,
    /// True once any event has arrived — the UI shows "waiting for activity"
    /// rather than an empty diagram until then.
    pub active: bool,
}

impl FlowState {
    /// Share of windowed time spent moving bytes versus computing.
    ///
    /// The single number the whole flow view exists to produce. `None` until
    /// both sides have been observed — a ratio against zero compute would read
    /// as infinitely bus-bound the moment the first layer loads.
    pub fn transfer_vs_compute(&self) -> Option<f64> {
        let transfer = self.transfer_us();
        let compute = self.compute.total_us;
        (transfer > 0 && compute > 0).then(|| transfer as f64 / compute as f64)
    }

    /// Time spent getting bytes to where compute can use them.
    ///
    /// `ram_hit` is excluded deliberately: a hit costs essentially nothing and
    /// counting it would dilute the very ratio this is measuring.
    pub fn transfer_us(&self) -> u64 {
        self.ram_miss.total_us + self.pin_stage.total_us + self.h2d.total_us
    }
}

/// Rolling-window aggregator.
pub struct Aggregator {
    events: VecDeque<FlowEvent>,
    snapshot: Option<Snapshot>,
    /// Last cumulative drop count seen from the engine.
    dropped: u64,
    /// Events received since the stream opened, for the sampled fraction.
    received: u64,
}

impl Default for Aggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl Aggregator {
    pub fn new() -> Self {
        Self {
            events: VecDeque::new(),
            snapshot: None,
            dropped: 0,
            received: 0,
        }
    }

    pub fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.snapshot = Some(snapshot);
    }

    /// Reset everything — used when the connection changes, so one model's
    /// numbers never bleed into another's.
    pub fn clear(&mut self) {
        self.events.clear();
        self.snapshot = None;
        self.dropped = 0;
        self.received = 0;
    }

    pub fn ingest(&mut self, events: Vec<FlowEvent>, dropped: u64) {
        self.dropped = self.dropped.max(dropped);
        self.received += events.len() as u64;
        for e in events {
            self.events.push_back(e);
        }
        self.evict_old();
    }

    /// Drop events that have fallen out of the window.
    ///
    /// Bounded by the engine's own clock (`t_us`), not the host's: the two are
    /// different clocks, and on a remote connection they are on different
    /// machines entirely.
    fn evict_old(&mut self) {
        let Some(newest) = self.events.back().map(|e| e.t_us) else {
            return;
        };
        let cutoff = newest.saturating_sub(WINDOW_US);
        while let Some(front) = self.events.front() {
            if front.t_us < cutoff || self.events.len() > MAX_EVENTS {
                self.events.pop_front();
            } else {
                break;
            }
        }
    }

    /// Build the current state.
    pub fn state(&self) -> FlowState {
        let mut s = FlowState {
            snapshot: self.snapshot.clone(),
            dropped: self.dropped,
            active: !self.events.is_empty(),
            ..Default::default()
        };

        // Cumulative drops against everything that was ever produced.
        let produced = self.received + self.dropped;
        s.sampled_fraction = if produced == 0 {
            1.0
        } else {
            self.received as f64 / produced as f64
        };

        if self.events.is_empty() {
            // Still report the model's shape so the diagram can be drawn
            // greyed-out rather than blank.
            s.layers = self.blank_layers();
            return s;
        }

        let oldest = self.events.front().map(|e| e.t_us).unwrap_or(0);
        let newest = self.events.back().map(|e| e.t_us).unwrap_or(0);
        s.window_secs = (newest.saturating_sub(oldest)) as f64 / 1e6;

        let num_layers = self
            .snapshot
            .as_ref()
            .map(|sn| sn.num_layers)
            .unwrap_or_else(|| self.events.iter().map(|e| e.layer).max().unwrap_or(0) + 1);
        let mut layers: Vec<LayerState> = (0..num_layers)
            .map(|layer| LayerState {
                layer,
                residency: Residency::Absent,
                last_seen_us: 0,
                evictions: 0,
                bytes: 0,
            })
            .collect();

        for e in &self.events {
            s.token = s.token.max(e.token);

            let stat = match e.stage {
                Stage::MmapRead => &mut s.mmap_read,
                Stage::Dequant => &mut s.dequant,
                Stage::RamHit => &mut s.ram_hit,
                Stage::RamMiss => &mut s.ram_miss,
                Stage::PinStage => &mut s.pin_stage,
                Stage::H2d => &mut s.h2d,
                Stage::Compute => &mut s.compute,
                Stage::Evict => &mut s.evict,
                Stage::Prefetch => &mut s.prefetch,
                Stage::Unknown => continue,
            };
            stat.events += 1;
            stat.bytes += e.bytes;
            stat.total_us += e.dur_us as u64;
            stat.estimated |= e.estimated;

            if let Some(depth) = e.depth {
                s.prefetch_depth = Some(depth);
            }

            // Layer residency. Events arrive in emission order, so the last one
            // to touch a layer wins.
            if let Some(state) = layers.get_mut(e.layer as usize) {
                state.last_seen_us = state.last_seen_us.max(e.t_us);
                if e.bytes > 0 {
                    state.bytes = e.bytes;
                }
                match e.stage {
                    Stage::Prefetch if e.bytes > 0 => state.residency = Residency::Prefetched,
                    Stage::MmapRead | Stage::RamHit | Stage::RamMiss | Stage::H2d => {
                        state.residency = Residency::Resident
                    }
                    Stage::Compute => state.residency = Residency::Resident,
                    _ => {}
                }
            }
            // An eviction names its victim; the layer field is the incoming one.
            if e.stage == Stage::Evict {
                if let Some(victim) = e.victim {
                    if let Some(state) = layers.get_mut(victim as usize) {
                        state.residency = Residency::Evicted;
                        state.evictions += 1;
                        state.last_seen_us = state.last_seen_us.max(e.t_us);
                    }
                }
            }
        }

        for stat in [
            &mut s.mmap_read,
            &mut s.ram_hit,
            &mut s.ram_miss,
            &mut s.dequant,
            &mut s.pin_stage,
            &mut s.h2d,
            &mut s.compute,
        ] {
            if stat.bytes > 0 && stat.total_us > 0 {
                stat.bytes_per_sec = Some(stat.bytes as f64 / (stat.total_us as f64 / 1e6));
            }
        }

        s.layers = layers;
        s
    }

    fn blank_layers(&self) -> Vec<LayerState> {
        let n = self.snapshot.as_ref().map(|s| s.num_layers).unwrap_or(0);
        (0..n)
            .map(|layer| LayerState {
                layer,
                residency: Residency::Absent,
                last_seen_us: 0,
                evictions: 0,
                bytes: 0,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t_us: u64, layer: u32, stage: Stage, bytes: u64, dur_us: u32) -> FlowEvent {
        FlowEvent {
            t_us,
            seq: t_us,
            token: 1,
            layer,
            stage,
            bytes,
            dur_us,
            estimated: false,
            victim: None,
            depth: None,
        }
    }

    #[test]
    fn an_empty_aggregator_is_inactive_but_not_broken() {
        let a = Aggregator::new();
        let s = a.state();
        assert!(!s.active);
        assert_eq!(s.sampled_fraction, 1.0, "nothing lost when nothing produced");
        assert!(s.transfer_vs_compute().is_none());
    }

    #[test]
    fn stage_totals_and_bandwidth_are_computed_per_stage() {
        let mut a = Aggregator::new();
        a.ingest(
            vec![
                ev(0, 0, Stage::RamMiss, 1_000_000, 1000), // 1 MB in 1 ms
                ev(100, 0, Stage::Compute, 0, 500),
            ],
            0,
        );
        let s = a.state();
        assert!(s.active);
        assert_eq!(s.ram_miss.bytes, 1_000_000);
        assert_eq!(s.ram_miss.total_us, 1000);
        // 1 MB in 1 ms is 1 GB/s.
        assert_eq!(s.ram_miss.bytes_per_sec, Some(1e9));
        // Compute moves no bytes, so it has no bandwidth to report.
        assert_eq!(s.compute.bytes_per_sec, None);
    }

    /// The headline ratio: time moving bytes against time computing.
    #[test]
    fn transfer_vs_compute_reflects_a_bus_bound_run() {
        let mut a = Aggregator::new();
        a.ingest(
            vec![
                ev(0, 0, Stage::RamMiss, 30_000_000, 47_000),
                ev(1, 0, Stage::Compute, 0, 6_000),
            ],
            0,
        );
        let ratio = a.state().transfer_vs_compute().expect("both sides present");
        assert!(ratio > 7.0, "47ms transfer vs 6ms compute, got {ratio}");
    }

    /// A cache hit is nearly free; counting it as transfer time would dilute
    /// the ratio it is meant to measure.
    #[test]
    fn ram_hits_do_not_count_as_transfer_time() {
        let mut a = Aggregator::new();
        a.ingest(
            vec![
                ev(0, 0, Stage::RamHit, 30_000_000, 0),
                ev(1, 0, Stage::Compute, 0, 6_000),
            ],
            0,
        );
        let s = a.state();
        assert_eq!(s.transfer_us(), 0);
        assert!(s.transfer_vs_compute().is_none());
    }

    #[test]
    fn events_outside_the_window_are_dropped() {
        let mut a = Aggregator::new();
        a.ingest(vec![ev(0, 0, Stage::Compute, 0, 10)], 0);
        // Far beyond the window; the first event must age out.
        a.ingest(vec![ev(WINDOW_US * 3, 1, Stage::Compute, 0, 20)], 0);
        let s = a.state();
        assert_eq!(s.compute.events, 1);
        assert_eq!(s.compute.total_us, 20);
    }

    #[test]
    fn residency_tracks_load_and_eviction() {
        let mut a = Aggregator::new();
        let mut evict = ev(10, 3, Stage::Evict, 0, 0);
        evict.victim = Some(0);
        a.ingest(
            vec![ev(0, 0, Stage::MmapRead, 100, 5), ev(5, 3, Stage::MmapRead, 100, 5), evict],
            0,
        );
        let s = a.state();
        // Layer 0 was loaded, then evicted to make room for layer 3.
        assert_eq!(s.layers[0].residency, Residency::Evicted);
        assert_eq!(s.layers[0].evictions, 1);
        assert_eq!(s.layers[3].residency, Residency::Resident);
    }

    #[test]
    fn prefetch_depth_is_reported_from_the_latest_event() {
        let mut a = Aggregator::new();
        let mut e = ev(0, 0, Stage::Prefetch, 0, 0);
        e.depth = Some(3);
        a.ingest(vec![e], 0);
        assert_eq!(a.state().prefetch_depth, Some(3));
    }

    /// The UI must be able to say "sampled at N%" honestly.
    #[test]
    fn sampled_fraction_accounts_for_engine_side_drops() {
        let mut a = Aggregator::new();
        a.ingest(vec![ev(0, 0, Stage::Compute, 0, 1); 90], 10);
        let s = a.state();
        assert!(
            (s.sampled_fraction - 0.9).abs() < 1e-9,
            "90 received of 100 produced, got {}",
            s.sampled_fraction
        );
        assert_eq!(s.dropped, 10);
    }

    /// An estimated duration anywhere in a stage taints the whole edge, so the
    /// UI can never draw it as a measurement.
    #[test]
    fn estimated_durations_propagate_to_the_stage() {
        let mut a = Aggregator::new();
        let mut e = ev(0, 0, Stage::H2d, 1000, 10);
        e.estimated = true;
        a.ingest(vec![ev(0, 0, Stage::H2d, 1000, 10), e], 0);
        assert!(a.state().h2d.estimated);
    }

    #[test]
    fn clear_resets_everything_between_connections() {
        let mut a = Aggregator::new();
        a.ingest(vec![ev(0, 0, Stage::Compute, 0, 10)], 5);
        a.clear();
        let s = a.state();
        assert!(!s.active);
        assert_eq!(s.dropped, 0);
        assert_eq!(s.compute.events, 0);
    }

    /// A newer engine's unrecognised stage must not corrupt the totals.
    #[test]
    fn unknown_stages_are_ignored() {
        let mut a = Aggregator::new();
        a.ingest(vec![ev(0, 0, Stage::Unknown, 999, 999)], 0);
        let s = a.state();
        assert_eq!(s.compute.events, 0);
        assert_eq!(s.mmap_read.events, 0);
    }
}
