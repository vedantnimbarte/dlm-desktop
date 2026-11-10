//! Local model discovery and VRAM fit planning.
//!
//! This is the half of the app that uses dlm **as a library**. It has to answer
//! "will this model run, and how much of it will stream?" *before* any server
//! exists, which rules out asking a running engine. `ModelConfig`,
//! `LayerCatalog` and `VramProfiler` answer it directly from the checkpoint on
//! disk.
//!
//! The numbers here are the engine's own: the same `plan_from_catalog` call
//! `dlm profile` makes (`dlm/src/main.rs:900`), with the same native-scheme
//! resolution. Reimplementing the arithmetic would guarantee the UI eventually
//! disagreed with the engine it is describing.

use dlm::model::QuantScheme;
use dlm::{LayerCatalog, MmapStore, ModelConfig, VramProfiler};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A model directory found on disk. Built from `config.json` and file sizes
/// only — no mmap, no catalog — so scanning a directory of large checkpoints
/// stays instant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModel {
    pub path: String,
    pub name: String,
    /// Total bytes of `*.safetensors` in the directory.
    pub checkpoint_bytes: u64,
    pub num_layers: u32,
    pub hidden_size: u32,
    pub is_moe: bool,
    /// Rough parameter count from the geometry, for a human-readable size.
    pub estimated_params: u64,
    /// A real HF tokenizer is present (`tokenizer.json`, or
    /// `vocab.json` + `merges.txt`). Without one dlm falls back to a raw byte
    /// tokenizer, which produces gibberish on an instruct model.
    pub has_tokenizer: bool,
    /// For a `.gguf` file, its quantization as published ("Q4_K_M"); `None`
    /// for a safetensors directory.
    pub gguf_type: Option<String>,
    /// Set when `config.json` exists but dlm cannot decode the architecture.
    /// The entry is still listed — a model dlm refuses is more useful to show
    /// with the reason than to silently omit.
    pub error: Option<String>,
}

/// Where the free-VRAM figure behind a plan came from.
///
/// Made explicit because the whole fit preview is only as trustworthy as this
/// number, and a plan computed against a guess must not look like one computed
/// against the device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum VramSource {
    /// Queried from the live device. `via` names which query answered, since
    /// the engine's own and the driver's can disagree about availability.
    Device { free: u64, total: u64, via: String },
    /// The user's `--vram-budget-gb`.
    Manual { bytes: u64 },
    /// No GPU and no manual budget: no plan can be computed.
    Unavailable { reason: String },
}

/// One row of the fit table: what happens at a given weight precision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FitRow {
    /// `"checkpoint"`, `"int8"` or `"int4"`, matching the settings dropdown.
    pub quant: String,
    /// Whether dlm would accept this `--quant` for this checkpoint.
    pub available: bool,
    pub unavailable_reason: Option<String>,

    pub layers_to_load: u32,
    pub num_layers: u32,
    pub resident_fraction: f64,
    pub stream_passes: u32,
    pub per_layer_weight_bytes: u64,
    pub kv_total_bytes: u64,
    pub pinned_bytes: u64,
    pub usable_bytes: u64,
    /// Every layer fits at once — no streaming, the fast path.
    pub fits_entirely: bool,
}

/// The complete answer to "can I run this, and how?".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FitPreview {
    pub model: LocalModel,
    pub vram: VramSource,
    pub native_quant: String,
    pub context_length: u32,
    pub max_batch: u32,
    pub rows: Vec<FitRow>,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("{0} is not a directory")]
    NotADirectory(String),
    #[error("no config.json in {0}")]
    NoConfig(String),
    #[error("{0}")]
    Dlm(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ModelError>;

/// Does this directory look like a model checkpoint?
fn is_model_dir(dir: &Path) -> bool {
    dir.join("config.json").is_file() && has_safetensors(dir)
}

/// A single-file GGUF model, which dlm runs with no other files beside it.
fn is_gguf(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
}

/// `.gguf` files directly inside `dir`.
fn gguf_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| is_gguf(p))
                .collect()
        })
        .unwrap_or_default()
}

/// The config dlm would build for `path`: from GGUF metadata when the store
/// has it, else from `config.json` — the same split as `dlm::loader::open_model`,
/// without mapping the file a second time.
fn model_config(path: &Path, store: &MmapStore, quant: QuantScheme) -> Result<ModelConfig> {
    match store.gguf_metadata() {
        Some(meta) => ModelConfig::from_gguf(meta, store.locate("lm_head.weight").is_some(), quant),
        None => ModelConfig::from_path(path, quant),
    }
    .map_err(|e| ModelError::Dlm(e.to_string()))
}

/// Describe a `.gguf` file. Mapping it reads only the header, so this stays
/// cheap for a multi-gigabyte file.
fn describe_gguf(path: &Path) -> Result<LocalModel> {
    let name = path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let base = LocalModel {
        path: path.display().to_string(),
        name,
        checkpoint_bytes: std::fs::metadata(path)?.len(),
        num_layers: 0,
        hidden_size: 0,
        is_moe: false,
        estimated_params: 0,
        // The vocabulary and merges live in the file's metadata.
        has_tokenizer: true,
        gguf_type: Some("GGUF".into()),
        error: None,
    };
    let store = match MmapStore::open_path(path) {
        Ok(s) => s,
        Err(e) => {
            return Ok(LocalModel {
                error: Some(e.to_string()),
                ..base
            })
        }
    };
    let gguf_type = store
        .gguf_metadata()
        .and_then(dlm::storage::gguf::file_type_name)
        .map(String::from)
        .or(base.gguf_type.clone());
    Ok(match model_config(path, &store, QuantScheme::Fp16) {
        Ok(config) => LocalModel {
            num_layers: config.num_layers,
            hidden_size: config.hidden_size,
            is_moe: config.is_moe(),
            estimated_params: config.estimated_total_params(),
            gguf_type,
            ..base
        },
        Err(e) => LocalModel {
            error: Some(e.to_string()),
            gguf_type,
            ..base
        },
    })
}

fn has_safetensors(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("safetensors"))
            })
        })
        .unwrap_or(false)
}

fn safetensors_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| {
                    e.path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("safetensors"))
                })
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

fn has_tokenizer(dir: &Path) -> bool {
    dir.join("tokenizer.json").is_file()
        || (dir.join("vocab.json").is_file() && dir.join("merges.txt").is_file())
}

/// Read one model's metadata: a checkpoint directory or a `.gguf` file.
pub fn describe(dir: &Path) -> Result<LocalModel> {
    if is_gguf(dir) {
        return describe_gguf(dir);
    }
    if !dir.is_dir() {
        return Err(ModelError::NotADirectory(dir.display().to_string()));
    }
    if !dir.join("config.json").is_file() {
        return Err(ModelError::NoConfig(dir.display().to_string()));
    }

    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());

    let base = LocalModel {
        path: dir.display().to_string(),
        name,
        checkpoint_bytes: safetensors_bytes(dir),
        num_layers: 0,
        hidden_size: 0,
        is_moe: false,
        estimated_params: 0,
        has_tokenizer: has_tokenizer(dir),
        gguf_type: None,
        error: None,
    };

    // Fp16 here is a placeholder for reading geometry; the real precision is
    // resolved from the checkpoint in `fit_preview`. Geometry does not depend
    // on it.
    match ModelConfig::from_path(dir, QuantScheme::Fp16) {
        Ok(config) => Ok(LocalModel {
            num_layers: config.num_layers,
            hidden_size: config.hidden_size,
            is_moe: config.is_moe(),
            estimated_params: config.estimated_total_params(),
            ..base
        }),
        // Listed with the reason rather than hidden: "dlm cannot decode this
        // architecture" is exactly what the user needs to know.
        Err(e) => Ok(LocalModel {
            error: Some(e.to_string()),
            ..base
        }),
    }
}

/// Find models under `root`: checkpoint directories (and `root` itself if it
/// is one), plus `.gguf` files in `root` and one level down — `dlm pull`
/// puts GGUF downloads in a subfolder.
pub fn scan(root: &Path) -> Result<Vec<LocalModel>> {
    if !root.is_dir() {
        return Err(ModelError::NotADirectory(root.display().to_string()));
    }
    let mut out = Vec::new();
    if is_model_dir(root) {
        out.push(describe(root)?);
    }
    let mut ggufs = gguf_files(root);
    for entry in std::fs::read_dir(root)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if is_model_dir(&path) {
            if let Ok(m) = describe(&path) {
                out.push(m);
            }
        } else {
            ggufs.extend(gguf_files(&path));
        }
    }
    out.extend(ggufs.iter().filter_map(|p| describe_gguf(p).ok()));
    out.sort_by_key(|m| m.name.to_lowercase());
    Ok(out)
}

/// Free VRAM, or an explanation of why there is no figure.
///
/// A manual budget always wins: it is the same override `--vram-budget-gb` is,
/// and a user who set it wants to plan against it.
pub fn vram_source(manual_budget_gb: Option<f64>) -> VramSource {
    if let Some(gb) = manual_budget_gb.filter(|g| *g > 0.0) {
        return VramSource::Manual {
            bytes: (gb * (1u64 << 30) as f64) as u64,
        };
    }
    // The engine's own query first: it reports what the engine would actually
    // see, which is the figure a plan should be built on when it is available.
    if let Ok(mem) = dlm::gpu::mem_get_info() {
        return VramSource::Device {
            free: mem.free,
            total: mem.total,
            via: "engine".into(),
        };
    }
    // Fall back to the driver. A CPU-only dlm build cannot query VRAM, but that
    // says nothing about whether a GPU exists — and refusing to plan for a card
    // that is sitting right there would make the fit table useless on exactly
    // the machines it is most useful for.
    if let Some((free, total, name)) = crate::hw::primary_gpu_memory() {
        return VramSource::Device {
            free,
            total,
            via: format!("driver ({name})"),
        };
    }
    VramSource::Unavailable {
        reason: "no GPU detected, and this dlm build cannot query VRAM".into(),
    }
}

fn scheme_name(s: QuantScheme) -> &'static str {
    match s {
        QuantScheme::F32 => "f32",
        QuantScheme::Fp16 => "fp16",
        QuantScheme::Int8 => "int8",
        QuantScheme::Int4 => "int4",
    }
}

/// Mirrors dlm's `resolve_quant` (`dlm/src/main.rs:946`): which `--quant`
/// values this checkpoint actually accepts.
///
/// Kept in step with the engine deliberately. Offering the user an option dlm
/// will reject at startup is worse than not offering it — they would pick it,
/// wait through a load, and get a clap error.
fn quant_availability(
    requested: Option<QuantScheme>,
    native: QuantScheme,
    is_packed_int4: bool,
    is_gguf: bool,
) -> std::result::Result<QuantScheme, String> {
    let Some(requested) = requested else {
        return Ok(native);
    };
    // dlm refuses --quant on any GGUF, even one naming the file's own
    // precision: its weights are already quantized.
    if is_gguf {
        return Err("a GGUF file is already quantized; dlm reads it as saved".into());
    }
    if requested == native {
        return Ok(requested);
    }
    if native == QuantScheme::Int4 && is_packed_int4 {
        return Err("already a packed 4-bit checkpoint; it loads at its own int4 precision".into());
    }
    match requested {
        QuantScheme::Int4 | QuantScheme::Int8 => Ok(requested),
        other => Err(format!(
            "dlm does not convert between float widths ({} checkpoint)",
            scheme_name(other)
        )),
    }
}

/// Compute the fit table for a model.
///
/// This mmaps the checkpoint to build a [`LayerCatalog`], which gives the
/// *measured* largest-layer size and the real pinned-zone cost instead of a
/// geometry estimate. mmap does not read the file, so it stays cheap even for a
/// multi-gigabyte checkpoint.
pub fn fit_preview(
    dir: &Path,
    context_length: u32,
    max_batch: u32,
    safety_margin_gb: Option<f64>,
    vram_budget_gb: Option<f64>,
) -> Result<FitPreview> {
    let model = describe(dir)?;
    let vram = vram_source(vram_budget_gb);

    // `open_path` takes a checkpoint directory or a single `.gguf` file.
    let store = MmapStore::open_path(dir).map_err(|e| ModelError::Dlm(e.to_string()))?;
    let catalog = LayerCatalog::build(&store);
    let native = dlm::loader::checkpoint_scheme(&store).unwrap_or(QuantScheme::Fp16);
    let is_packed_int4 = store
        .locate("model.layers.0.self_attn.q_proj.qweight")
        .is_some();
    let gguf_type = store
        .gguf_metadata()
        .map(|m| dlm::storage::gguf::file_type_name(m).unwrap_or("GGUF"));

    let mut profiler = VramProfiler::new(context_length).with_max_batch(max_batch);
    if let Some(gb) = safety_margin_gb {
        profiler = profiler.with_safety_margin_bytes((gb * (1u64 << 30) as f64) as u64);
    }

    let free_bytes = match vram {
        VramSource::Device { free, .. } => Some(free),
        VramSource::Manual { bytes } => Some(bytes),
        VramSource::Unavailable { .. } => None,
    };

    let options: [(&str, Option<QuantScheme>); 3] = [
        ("checkpoint", None),
        ("int8", Some(QuantScheme::Int8)),
        ("int4", Some(QuantScheme::Int4)),
    ];

    let mut rows = Vec::with_capacity(options.len());
    for (label, requested) in options {
        match quant_availability(requested, native, is_packed_int4, gguf_type.is_some()) {
            Err(reason) => rows.push(FitRow {
                quant: label.into(),
                available: false,
                unavailable_reason: Some(reason),
                layers_to_load: 0,
                num_layers: model.num_layers,
                resident_fraction: 0.0,
                stream_passes: 0,
                per_layer_weight_bytes: 0,
                kv_total_bytes: 0,
                pinned_bytes: 0,
                usable_bytes: 0,
                fits_entirely: false,
            }),
            Ok(effective) => {
                let config = model_config(dir, &store, effective)?;
                let Some(free) = free_bytes else {
                    // No VRAM figure: report the row as unavailable rather than
                    // inventing a budget and presenting the result as a plan.
                    rows.push(FitRow {
                        quant: label.into(),
                        available: false,
                        unavailable_reason: Some(
                            "no VRAM figure — set a manual budget to plan".into(),
                        ),
                        layers_to_load: 0,
                        num_layers: config.num_layers,
                        resident_fraction: 0.0,
                        stream_passes: 0,
                        per_layer_weight_bytes: 0,
                        kv_total_bytes: 0,
                        pinned_bytes: 0,
                        usable_bytes: 0,
                        fits_entirely: false,
                    });
                    continue;
                };
                let plan = profiler.plan_from_catalog(&config, &catalog, native, free);
                rows.push(FitRow {
                    quant: label.into(),
                    available: true,
                    unavailable_reason: None,
                    layers_to_load: plan.layers_to_load,
                    num_layers: plan.num_layers,
                    resident_fraction: plan.resident_fraction(),
                    stream_passes: plan.stream_passes(),
                    per_layer_weight_bytes: plan.per_layer_weight_bytes,
                    kv_total_bytes: plan.kv_total_bytes,
                    pinned_bytes: plan.pinned_bytes,
                    usable_bytes: plan.usable_bytes,
                    fits_entirely: plan.layers_to_load >= plan.num_layers,
                });
            }
        }
    }

    Ok(FitPreview {
        model,
        vram,
        // A GGUF is known by the name it was published under, so say both.
        native_quant: match gguf_type {
            Some(t) => format!("{t}, read as {}", scheme_name(native)),
            None => scheme_name(native).into(),
        },
        context_length,
        max_batch,
        rows,
    })
}

/// Default place to look for models: the `models/` directory beside the dlm
/// binary's repo, else the user's home. Only a starting point — the UI lets the
/// user point anywhere.
pub fn default_models_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    for base in [cwd.as_path()].into_iter().chain(cwd.ancestors()) {
        let candidate = base.join("models");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine's rule, mirrored: omitting `--quant` always works.
    #[test]
    fn omitted_quant_resolves_to_the_checkpoint_scheme() {
        assert_eq!(
            quant_availability(None, QuantScheme::Fp16, false, false),
            Ok(QuantScheme::Fp16)
        );
    }

    #[test]
    fn quantizing_down_from_floats_is_allowed() {
        assert_eq!(
            quant_availability(Some(QuantScheme::Int4), QuantScheme::Fp16, false, false),
            Ok(QuantScheme::Int4)
        );
        assert_eq!(
            quant_availability(Some(QuantScheme::Int8), QuantScheme::F32, false, false),
            Ok(QuantScheme::Int8)
        );
    }

    /// dlm refuses to convert between float widths; the UI must not offer it.
    #[test]
    fn converting_between_float_widths_is_refused() {
        assert!(
            quant_availability(Some(QuantScheme::F32), QuantScheme::Fp16, false, false).is_err()
        );
    }

    /// An already-packed GPTQ checkpoint has no floats left to quantize.
    #[test]
    fn packed_int4_refuses_further_quantization() {
        let err = quant_availability(Some(QuantScheme::Int8), QuantScheme::Int4, true, false)
            .expect_err("should refuse");
        assert!(err.contains("packed 4-bit"), "{err}");
    }

    /// ...but asking for int4 on a packed int4 checkpoint is just a no-op.
    #[test]
    fn packed_int4_accepts_its_own_scheme() {
        assert_eq!(
            quant_availability(Some(QuantScheme::Int4), QuantScheme::Int4, true, false),
            Ok(QuantScheme::Int4)
        );
    }

    /// dlm refuses `--quant` on any GGUF, even the file's own precision.
    #[test]
    fn gguf_refuses_every_quant_but_omitting_it() {
        assert_eq!(
            quant_availability(None, QuantScheme::Int4, false, true),
            Ok(QuantScheme::Int4)
        );
        assert!(
            quant_availability(Some(QuantScheme::Int4), QuantScheme::Int4, false, true).is_err()
        );
        assert!(
            quant_availability(Some(QuantScheme::Int8), QuantScheme::Int4, false, true).is_err()
        );
    }

    #[test]
    fn scan_finds_gguf_files_at_the_root_and_one_level_down() {
        let tmp = std::env::temp_dir().join(format!("dlm-desktop-gguf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("gguf")).unwrap();
        assert!(gguf_files(&tmp).is_empty());
        std::fs::write(tmp.join("a.gguf"), b"").unwrap();
        std::fs::write(tmp.join("gguf").join("b.GGUF"), b"").unwrap();
        std::fs::write(tmp.join("gguf").join("notes.txt"), b"").unwrap();
        // Empty files are not valid GGUF, so they are listed with an error
        // rather than dropped: "dlm cannot read this" is worth showing.
        let found = scan(&tmp).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        let names: Vec<_> = found.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["a", "b"], "{found:?}");
        assert!(found.iter().all(|m| m.gguf_type.is_some()));
    }

    /// A manual budget overrides every device query, exactly as
    /// `--vram-budget-gb` does.
    #[test]
    fn manual_budget_wins_over_the_device_query() {
        match vram_source(Some(6.0)) {
            VramSource::Manual { bytes } => assert_eq!(bytes, 6 * (1 << 30)),
            other => panic!("expected Manual, got {other:?}"),
        }
    }

    /// With no manual budget the answer depends on the machine, but it must
    /// always be coherent: a reported device has a non-zero total and free no
    /// greater than it, and an unavailable source explains itself. Never a
    /// fabricated figure.
    #[test]
    fn an_automatic_vram_figure_is_coherent_or_absent() {
        match vram_source(None) {
            VramSource::Device { free, total, via } => {
                assert!(total > 0, "a device must report a total");
                assert!(free <= total, "free cannot exceed total");
                assert!(!via.is_empty(), "the figure must name its source");
            }
            VramSource::Unavailable { reason } => {
                assert!(!reason.is_empty(), "absence must explain itself");
            }
            VramSource::Manual { .. } => panic!("no manual budget was given"),
        }
    }

    /// Zero or negative is not a budget; fall through to the device.
    #[test]
    fn a_nonsense_budget_is_ignored() {
        assert!(!matches!(vram_source(Some(0.0)), VramSource::Manual { .. }));
    }

    #[test]
    fn scanning_a_non_directory_is_an_error() {
        assert!(scan(Path::new("definitely/not/a/real/dir")).is_err());
    }

    #[test]
    fn describing_a_directory_without_a_config_is_an_error() {
        let tmp = std::env::temp_dir().join("dlm-desktop-test-empty");
        let _ = std::fs::create_dir_all(&tmp);
        assert!(matches!(describe(&tmp), Err(ModelError::NoConfig(_))));
    }
}
