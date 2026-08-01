//! Subscriber for dlm's `/v1/telemetry` stream.
//!
//! Mirrors the wire types in `dlm/src/telemetry/mod.rs`. They are duplicated
//! rather than imported from the linked crate on purpose: the app can attach to
//! a **remote** server running a different dlm build, so the shapes here are a
//! wire contract, not a shared struct. Importing them would make a version
//! mismatch a silent deserialization failure instead of a visible one.
//!
//! The stream is a plain SSE connection carrying two frame types — `snapshot`
//! (the static shape of the run) and `flow` (batches of measured events) — read
//! with the same client everything else uses.

use crate::client::{self, Cancel, Endpoint, Flow};
use serde::{Deserialize, Serialize};

/// A stage of the layer data path. Unknown values are preserved rather than
/// rejected, so a newer engine emitting a stage this build has never heard of
/// degrades to "shown but unrecognised" instead of killing the stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    MmapRead,
    Dequant,
    RamHit,
    RamMiss,
    PinStage,
    H2d,
    Compute,
    Evict,
    Prefetch,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowEvent {
    #[serde(default)]
    pub t_us: u64,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub token: u64,
    #[serde(default)]
    pub layer: u32,
    pub stage: Stage,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub dur_us: u32,
    /// Wall-clock rather than device-measured. The UI must render these
    /// differently — presenting an estimate as a measurement is the one thing
    /// this whole feature must not do.
    #[serde(default)]
    pub estimated: bool,
    #[serde(default)]
    pub victim: Option<u32>,
    #[serde(default)]
    pub depth: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Batch {
    #[serde(default)]
    pub events: Vec<FlowEvent>,
    /// Cumulative drops. Differenced by the consumer to know what it missed.
    #[serde(default)]
    pub dropped: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    #[serde(default)]
    pub num_layers: u32,
    #[serde(default)]
    pub resident_layers: u32,
    #[serde(default)]
    pub layer_bytes: Vec<u64>,
    #[serde(default)]
    pub pinned_bytes: u64,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub estimated_h2d: bool,
}

/// What the subscriber hands back for each frame.
#[derive(Debug, Clone)]
pub enum Frame {
    Snapshot(Snapshot),
    Flow(Batch),
}

/// Subscribe to `/v1/telemetry` and drive `on_frame` until the stream ends or
/// `cancel` is flipped.
///
/// Subscribing is what makes the engine start collecting, so dropping this
/// connection is also what makes it stop — there is no separate "off" call to
/// forget.
pub fn subscribe(
    endpoint: &Endpoint,
    cancel: &Cancel,
    mut on_frame: impl FnMut(Frame) -> Flow,
) -> Result<(), client::HttpError> {
    client::http::stream_sse(
        endpoint,
        "GET",
        "/v1/telemetry",
        None,
        cancel,
        |frame| match frame.event.as_deref() {
            Some("snapshot") => match serde_json::from_str::<Snapshot>(&frame.data) {
                Ok(s) => on_frame(Frame::Snapshot(s)),
                Err(_) => Flow::Continue,
            },
            Some("flow") => match serde_json::from_str::<Batch>(&frame.data) {
                Ok(b) => on_frame(Frame::Flow(b)),
                // A batch we cannot parse is not worth tearing down a live
                // stream over; skip it and keep reading.
                Err(_) => Flow::Continue,
            },
            // Keep-alive comments and anything unrecognised.
            _ => Flow::Continue,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_event_shape_the_engine_emits() {
        let json = r#"{"tUs":128412,"seq":9,"token":3,"layer":17,"stage":"h2d",
                       "bytes":58720256,"durUs":5140}"#;
        let e: FlowEvent = serde_json::from_str(json).unwrap();
        assert_eq!(e.layer, 17);
        assert_eq!(e.stage, Stage::H2d);
        assert_eq!(e.bytes, 58_720_256);
        assert_eq!(e.dur_us, 5140);
        assert!(!e.estimated);
    }

    /// The engine omits `estimated`/`victim`/`depth` when unset, so they must
    /// default rather than fail the whole batch.
    #[test]
    fn omitted_optional_fields_default() {
        let e: FlowEvent =
            serde_json::from_str(r#"{"layer":1,"stage":"compute"}"#).unwrap();
        assert_eq!(e.dur_us, 0);
        assert!(e.victim.is_none());
        assert!(e.depth.is_none());
    }

    #[test]
    fn eviction_and_prefetch_detail_round_trips() {
        let e: FlowEvent =
            serde_json::from_str(r#"{"layer":4,"stage":"evict","victim":9}"#).unwrap();
        assert_eq!(e.victim, Some(9));

        let e: FlowEvent =
            serde_json::from_str(r#"{"layer":4,"stage":"prefetch","depth":3}"#).unwrap();
        assert_eq!(e.depth, Some(3));
    }

    /// A newer engine must not be able to kill the stream by adding a stage.
    #[test]
    fn an_unknown_stage_degrades_rather_than_failing() {
        let e: FlowEvent =
            serde_json::from_str(r#"{"layer":1,"stage":"someFutureStage"}"#).unwrap();
        assert_eq!(e.stage, Stage::Unknown);
    }

    /// Bytes captured verbatim from a running `dlm serve --telemetry`. A
    /// hand-written fixture can only prove the types agree with themselves;
    /// this proves they agree with the engine.
    #[test]
    fn a_real_engine_batch_parses() {
        let raw = r#"{"events":[
            {"tUs":2067164,"seq":0,"token":0,"layer":0,"stage":"ramMiss","bytes":31465472,"durUs":16633},
            {"tUs":2067168,"seq":1,"token":0,"layer":0,"stage":"mmapRead","bytes":31465472,"durUs":16639},
            {"tUs":2067180,"seq":2,"token":0,"layer":0,"stage":"prefetch","bytes":0,"durUs":0,"depth":1},
            {"tUs":2073907,"seq":3,"token":0,"layer":0,"stage":"compute","bytes":0,"durUs":6727}
        ],"dropped":0}"#;
        let batch: Batch = serde_json::from_str(raw).expect("real batch must parse");
        assert_eq!(batch.events.len(), 4);
        assert_eq!(batch.events[0].stage, Stage::RamMiss);
        assert_eq!(batch.events[1].stage, Stage::MmapRead);
        assert_eq!(batch.events[2].depth, Some(1));
        assert_eq!(batch.events[3].stage, Stage::Compute);
        assert_eq!(batch.events[3].dur_us, 6727);
    }

    #[test]
    fn batch_carries_the_drop_count() {
        let b: Batch = serde_json::from_str(
            r#"{"events":[{"layer":1,"stage":"ramHit"}],"dropped":42}"#,
        )
        .unwrap();
        assert_eq!(b.events.len(), 1);
        assert_eq!(b.dropped, 42);
    }

    #[test]
    fn snapshot_parses_and_marks_estimated_transfers() {
        let s: Snapshot = serde_json::from_str(
            r#"{"numLayers":28,"residentLayers":2,"layerBytes":[1,2,3],
                "pinnedBytes":4096,"streaming":true,"device":"cpu","estimatedH2d":true}"#,
        )
        .unwrap();
        assert_eq!(s.num_layers, 28);
        assert_eq!(s.resident_layers, 2);
        assert!(s.streaming);
        assert_eq!(s.device, "cpu");
        assert!(
            s.estimated_h2d,
            "a CPU run has no device-measured transfer time"
        );
    }
}
