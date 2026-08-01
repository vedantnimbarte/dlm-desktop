//! Model scanning and VRAM planning against **real** checkpoints on disk.
//!
//! The unit tests cover the quant-compatibility rules in isolation; these prove
//! the parts that only a real safetensors file can: that `MmapStore` opens the
//! checkpoint, that `LayerCatalog` measures a layer, and that the resulting
//! plan is the same arithmetic `dlm profile` would print.
//!
//! Skipped unless `DLM_MODELS_DIR` points at a directory of checkpoints:
//!
//! ```sh
//! DLM_MODELS_DIR=../../dlm/models cargo test --test model_fit -- --nocapture
//! ```

use dlm_desktop_lib::models::{self, VramSource};
use std::path::PathBuf;

fn models_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("DLM_MODELS_DIR").ok()?);
    dir.is_dir().then_some(dir)
}

macro_rules! dir_or_skip {
    ($name:ident) => {
        match models_dir() {
            Some(d) => d,
            None => {
                eprintln!("skipping {}: set DLM_MODELS_DIR", stringify!($name));
                return;
            }
        }
    };
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}

#[test]
fn scan_finds_checkpoints_and_reads_their_geometry() {
    let dir = dir_or_skip!(scan_finds_checkpoints_and_reads_their_geometry);
    let found = models::scan(&dir).expect("scan failed");
    assert!(!found.is_empty(), "no models found under {}", dir.display());

    for m in &found {
        eprintln!(
            "{:<28} {:>3} layers  hidden {:<5} {:>7.2} GiB  {}{}",
            m.name,
            m.num_layers,
            m.hidden_size,
            gib(m.checkpoint_bytes),
            if m.has_tokenizer {
                "tok"
            } else {
                "NO TOKENIZER"
            },
            m.error
                .as_ref()
                .map(|e| format!("  [{e}]"))
                .unwrap_or_default(),
        );
    }

    // At least one must have decoded cleanly, or the library integration is
    // broken rather than the checkpoints being exotic.
    assert!(
        found.iter().any(|m| m.error.is_none() && m.num_layers > 0),
        "every model failed to decode"
    );
}

/// The headline feature: how the resident/streamed split changes with `--quant`.
#[test]
fn fit_preview_shows_quantization_buying_resident_layers() {
    let dir = dir_or_skip!(fit_preview_shows_quantization_buying_resident_layers);
    let found = models::scan(&dir).expect("scan failed");
    let model = found
        .iter()
        .find(|m| m.error.is_none() && m.num_layers > 0)
        .expect("no decodable model");

    // Plan against a fixed 6 GiB budget so the assertion does not depend on
    // whatever GPU (or none) this machine has.
    let preview = models::fit_preview(
        std::path::Path::new(&model.path),
        4096,
        1,
        Some(1.0),
        Some(6.0),
    )
    .expect("fit_preview failed");

    assert!(
        matches!(preview.vram, VramSource::Manual { .. }),
        "a manual budget must win over the device query"
    );

    eprintln!(
        "\n{} — native {} — {} layers, planning against 6 GiB\n",
        preview.model.name, preview.native_quant, preview.model.num_layers
    );
    eprintln!(
        "{:<11} {:>9} {:>10} {:>8} {:>12}  note",
        "quant", "resident", "of total", "passes", "per-layer"
    );
    for row in &preview.rows {
        if !row.available {
            eprintln!(
                "{:<11} {:>9} {:>10} {:>8} {:>12}  {}",
                row.quant,
                "—",
                "—",
                "—",
                "—",
                row.unavailable_reason.as_deref().unwrap_or("")
            );
            continue;
        }
        eprintln!(
            "{:<11} {:>9} {:>9.0}% {:>8} {:>10.1} MiB  {}",
            row.quant,
            row.layers_to_load,
            row.resident_fraction * 100.0,
            row.stream_passes,
            row.per_layer_weight_bytes as f64 / (1u64 << 20) as f64,
            if row.fits_entirely {
                "fits entirely — no streaming"
            } else {
                "streams"
            }
        );
    }

    let available: Vec<_> = preview.rows.iter().filter(|r| r.available).collect();
    assert!(!available.is_empty(), "no quant option was available");

    for row in &available {
        assert!(row.num_layers > 0, "plan reports zero layers");
        assert!(
            row.layers_to_load >= 1,
            "streaming always needs at least one resident slot"
        );
        assert!(
            row.layers_to_load <= row.num_layers,
            "cannot hold more layers than the model has"
        );
        assert_eq!(
            row.fits_entirely,
            row.layers_to_load >= row.num_layers,
            "fits_entirely must agree with the layer counts"
        );
    }

    // The core claim the UI makes to the user: a smaller precision never costs
    // resident layers, and a shrinking layer is why.
    if let (Some(base), Some(int4)) = (
        available.iter().find(|r| r.quant == "checkpoint"),
        available.iter().find(|r| r.quant == "int4"),
    ) {
        assert!(
            int4.per_layer_weight_bytes <= base.per_layer_weight_bytes,
            "int4 layer ({}) should not exceed the checkpoint layer ({})",
            int4.per_layer_weight_bytes,
            base.per_layer_weight_bytes
        );
        assert!(
            int4.layers_to_load >= base.layers_to_load,
            "int4 should never fit fewer layers than the checkpoint dtype"
        );
    }
}

/// With no GPU and no manual budget there is no honest plan to draw, and the
/// rows must say so rather than defaulting to some invented budget.
#[test]
fn without_a_vram_figure_rows_are_marked_unavailable() {
    let dir = dir_or_skip!(without_a_vram_figure_rows_are_marked_unavailable);
    let found = models::scan(&dir).expect("scan failed");
    let model = found
        .iter()
        .find(|m| m.error.is_none() && m.num_layers > 0)
        .expect("no decodable model");

    let preview = models::fit_preview(std::path::Path::new(&model.path), 4096, 1, None, None)
        .expect("fit_preview failed");

    match preview.vram {
        // No GPU on this machine: every row must be unavailable with a reason.
        VramSource::Unavailable { .. } => {
            assert!(
                preview.rows.iter().all(|r| !r.available),
                "rows must not be planned without a VRAM figure"
            );
            assert!(
                preview.rows.iter().all(|r| r.unavailable_reason.is_some()),
                "every unavailable row must explain itself"
            );
        }
        // A GPU is present: the device figure is a legitimate basis for a plan.
        VramSource::Device { free, total } => {
            eprintln!("device VRAM: {:.2} / {:.2} GiB free", gib(free), gib(total));
            assert!(total > 0);
        }
        VramSource::Manual { .. } => panic!("no manual budget was given"),
    }
}
