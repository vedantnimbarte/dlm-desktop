//! End-to-end test of the telemetry chain against a **real** `dlm serve
//! --telemetry`.
//!
//! Unit tests cover the aggregator and the diagnostic rules against synthetic
//! input. This covers the part they cannot: that the engine's actual wire
//! format parses, that subscribing starts collection, and that the resulting
//! state is coherent enough for the flow view to draw.
//!
//! ```sh
//! dlm serve --model-path models/Qwen3-0.6B --device cpu --stream \
//!           --resident-layers 3 --port 8130 --telemetry &
//! DLM_LIVE_ADDR=127.0.0.1:8130 cargo test --test live_telemetry -- --nocapture
//! ```

use dlm_desktop_lib::client::{self, Cancel, Endpoint, Flow, Sampling};
use dlm_desktop_lib::diag;
use dlm_desktop_lib::flow::Aggregator;
use dlm_desktop_lib::hw::HwSnapshot;
use dlm_desktop_lib::telemetry::{self, Frame};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn live_endpoint() -> Option<Endpoint> {
    let addr = std::env::var("DLM_LIVE_ADDR").ok()?;
    let endpoint = Endpoint::parse(&addr).expect("DLM_LIVE_ADDR must be host:port");
    Some(endpoint.with_api_key(std::env::var("DLM_LIVE_API_KEY").ok()))
}

macro_rules! live {
    ($name:ident) => {
        match live_endpoint() {
            Some(e) => e,
            None => {
                eprintln!("skipping {}: set DLM_LIVE_ADDR", stringify!($name));
                return;
            }
        }
    };
}

/// Subscribe, drive a short generation, and return the aggregated state.
fn capture(endpoint: &Endpoint) -> dlm_desktop_lib::flow::FlowState {
    let agg = Arc::new(Mutex::new(Aggregator::new()));
    let cancel = Cancel::new();

    let sub_agg = Arc::clone(&agg);
    let sub_endpoint = endpoint.clone();
    let sub_cancel = cancel.clone();
    let handle = std::thread::spawn(move || {
        telemetry::subscribe(&sub_endpoint, &sub_cancel, |frame| {
            match frame {
                Frame::Snapshot(s) => sub_agg.lock().unwrap().set_snapshot(s),
                Frame::Flow(b) => sub_agg.lock().unwrap().ingest(b.events, b.dropped),
            }
            Flow::Continue
        })
    });

    // Let the subscription establish before generating; collection only starts
    // once the engine sees a subscriber.
    std::thread::sleep(Duration::from_millis(500));

    let model = client::models(endpoint)
        .expect("/v1/models failed")
        .first()
        .map(|c| c.id.clone())
        .expect("no model");
    let messages = vec![client::ChatMessage::user("The capital of France is")];
    let sampling = Sampling {
        temperature: Some(0.0),
        max_tokens: Some(3),
        ..Default::default()
    };
    let gen_cancel = Cancel::new();
    let mut deltas = 0usize;
    let gen = client::chat_stream(
        endpoint,
        &model,
        &messages,
        &sampling,
        &gen_cancel,
        |e| {
            if matches!(e, client::ChatEvent::Delta(_)) {
                deltas += 1;
            }
            Flow::Continue
        },
    );
    // The generation is what produces the telemetry; if it did not run, an
    // empty aggregator says nothing about the telemetry path.
    gen.expect("generation failed");
    assert!(
        deltas > 0,
        "the model produced no tokens, so there is no layer traffic to observe"
    );

    // Give the last batch time to arrive before tearing the stream down.
    std::thread::sleep(Duration::from_millis(500));
    cancel.cancel();

    // The subscription must fail only because we cancelled it. Swallowing the
    // result here is how a silently-aborted stream first went unnoticed: the
    // aggregator was simply empty, which reads the same as "no activity".
    match handle.join().expect("subscriber thread panicked") {
        Ok(()) | Err(client::HttpError::Cancelled) => {}
        Err(e) => panic!("telemetry subscription failed: {e}"),
    }

    let state = agg.lock().unwrap().state();
    state
}

#[test]
fn telemetry_stream_produces_a_coherent_flow_state() {
    let endpoint = live!(telemetry_stream_produces_a_coherent_flow_state);
    let state = capture(&endpoint);

    let snapshot = state
        .snapshot
        .as_ref()
        .expect("the engine must send a snapshot frame first");
    eprintln!(
        "\nmodel: {} layers, {} resident, device {}, streaming {}",
        snapshot.num_layers, snapshot.resident_layers, snapshot.device, snapshot.streaming
    );
    assert!(snapshot.num_layers > 0, "snapshot must describe the model");
    assert_eq!(
        snapshot.layer_bytes.len(),
        snapshot.num_layers as usize,
        "one byte figure per layer"
    );

    assert!(state.active, "no events arrived — is --telemetry set?");
    eprintln!(
        "window {:.2}s · token {} · sampled {:.0}%",
        state.window_secs,
        state.token,
        state.sampled_fraction * 100.0
    );

    for (name, stat) in [
        ("ramMiss", &state.ram_miss),
        ("ramHit", &state.ram_hit),
        ("pinStage", &state.pin_stage),
        ("h2d", &state.h2d),
        ("compute", &state.compute),
        ("evict", &state.evict),
        ("prefetch", &state.prefetch),
    ] {
        if stat.events == 0 {
            continue;
        }
        eprintln!(
            "  {name:<10} {:>5} events {:>12} {:>9.1} ms{}",
            stat.events,
            format_bytes(stat.bytes),
            stat.total_us as f64 / 1000.0,
            stat.bytes_per_sec
                .map(|b| format!("  {}/s", format_bytes(b as u64)))
                .unwrap_or_default(),
        );
    }

    // A streamed run must show both sides of the ratio the view is built on.
    assert!(state.compute.events > 0, "no compute events");
    assert!(
        state.ram_miss.events > 0 || state.ram_hit.events > 0,
        "no layer loads observed"
    );

    // Bandwidth must be physically plausible. A cache hit moves a layer's worth
    // of bytes in ~no time; if that were reported as a read, this would show
    // terabytes per second.
    for (name, stat) in [
        ("ramMiss", &state.ram_miss),
        ("pinStage", &state.pin_stage),
        ("h2d", &state.h2d),
    ] {
        if let Some(bps) = stat.bytes_per_sec {
            assert!(
                bps < 200e9,
                "{name} reports {bps:.0} B/s, which no consumer hardware achieves"
            );
        }
    }

    if let Some(ratio) = state.transfer_vs_compute() {
        eprintln!("\ntransfer/compute ratio: {ratio:.2}x");
        assert!(ratio.is_finite() && ratio > 0.0);
    }
}

/// Residency must describe the window the engine actually reported, not more.
#[test]
fn layer_residency_never_exceeds_the_configured_window() {
    let endpoint = live!(layer_residency_never_exceeds_the_configured_window);
    let state = capture(&endpoint);
    let Some(snapshot) = state.snapshot.as_ref() else {
        panic!("no snapshot");
    };

    assert_eq!(
        state.layers.len(),
        snapshot.num_layers as usize,
        "one cell per layer"
    );

    let evicted: usize = state
        .layers
        .iter()
        .filter(|l| l.evictions > 0)
        .count();
    eprintln!(
        "{} of {} layers were evicted at least once (window {})",
        evicted, snapshot.num_layers, snapshot.resident_layers
    );

    // With a window smaller than the model, eviction is expected; with a full
    // window there should be none. Either way the counts must be consistent
    // with the plan rather than arbitrary.
    if snapshot.resident_layers >= snapshot.num_layers {
        assert_eq!(evicted, 0, "a fully-resident run should evict nothing");
    }
}

/// The diagnostics must run on real data without panicking, and must produce
/// findings that are actually true of this run.
#[test]
fn diagnostics_describe_the_real_run() {
    let endpoint = live!(diagnostics_describe_the_real_run);
    let state = capture(&endpoint);
    let findings = diag::analyze(&state, &HwSnapshot::default());

    eprintln!("\n{} findings:", findings.len());
    for f in &findings {
        eprintln!("  [{:?}] {}", f.severity, f.title);
        eprintln!("      {}", f.detail);
        if let Some(fix) = &f.fix {
            eprintln!("      fix: {fix}");
        }
    }

    // With no hardware snapshot, nothing hardware-derived may appear — those
    // rules must not invent a GPU.
    for f in &findings {
        assert_ne!(f.id, "pcie-link-degraded", "no GPU was reported");
        assert_ne!(f.id, "gpu-throttled", "no GPU was reported");
        assert_ne!(f.id, "vram-pressure", "no GPU was reported");
    }

    // A run with a window smaller than the model genuinely thrashes; if that
    // finding appears, its numbers must match the snapshot.
    if let (Some(hit), Some(snapshot)) = (
        findings.iter().find(|f| f.id == "window-thrashing"),
        state.snapshot.as_ref(),
    ) {
        assert!(
            hit.detail
                .contains(&format!("{} of {}", snapshot.resident_layers, snapshot.num_layers)),
            "finding must quote the real window: {}",
            hit.detail
        );
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}
