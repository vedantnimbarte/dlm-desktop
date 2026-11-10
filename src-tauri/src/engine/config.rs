//! `ServeConfig` — the desktop-side mirror of dlm's `ServeArgs`
//! (`dlm/src/cli.rs:184`), and the argv it turns into.
//!
//! This is the single source of truth behind the settings panel. Two rules keep
//! it honest:
//!
//! 1. **Defaults match dlm's defaults exactly.** A field left alone here
//!    produces no flag at all, so the server keeps its own default rather than
//!    this app pinning a value that dlm might later change.
//! 2. **The argv is shown to the user verbatim.** Whatever [`ServeConfig::to_argv`]
//!    returns is what runs and what the UI displays for copy-paste into a
//!    terminal, so there is never a hidden flag doing work the user cannot see.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// dlm's `--quant` values (`QuantArg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quant {
    /// Keep the checkpoint's own dtype — dlm's behaviour when `--quant` is absent.
    Checkpoint,
    Int8,
    Int4,
}

impl Quant {
    fn flag_value(self) -> Option<&'static str> {
        match self {
            Quant::Checkpoint => None,
            Quant::Int8 => Some("int8"),
            Quant::Int4 => Some("int4"),
        }
    }
}

/// dlm's `--kv-quant` values (`KvQuantArg`). `f16` is dlm's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KvQuant {
    F16,
    /// dlm still accepts `none` as an alias for exact f32.
    #[serde(alias = "none")]
    F32,
    Int8,
    Int4,
}

impl KvQuant {
    fn as_str(self) -> &'static str {
        match self {
            KvQuant::F16 => "f16",
            KvQuant::F32 => "f32",
            KvQuant::Int8 => "int8",
            KvQuant::Int4 => "int4",
        }
    }
}

/// dlm's `--speculative` values. `auto` is dlm's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speculative {
    Auto,
    Off,
    Ngram,
    Draft,
}

impl Speculative {
    fn as_str(self) -> &'static str {
        match self {
            Speculative::Auto => "auto",
            Speculative::Off => "off",
            Speculative::Ngram => "ngram",
            Speculative::Draft => "draft",
        }
    }
}

/// dlm's `--device`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    /// Leave `--device` off and let dlm pick (gpu on a cuda-kernels build).
    Auto,
    Cpu,
    Gpu,
}

/// Everything the app can put on a `dlm serve` command line.
///
/// Distributed/cluster flags are deliberately absent: for a cluster the app
/// attaches to a coordinator someone else started, rather than trying to launch
/// one from a desktop UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServeConfig {
    pub model_path: PathBuf,
    pub port: u16,
    pub host: String,

    // --- memory / precision -------------------------------------------------
    pub quant: Quant,
    pub kv_quant: KvQuant,
    pub vram_budget_gb: Option<f64>,
    pub safety_margin_gb: Option<f64>,
    pub ram_cache_gb: Option<f64>,
    pub expert_cache_gb: Option<f64>,
    pub context_length: u32,

    // --- streaming ----------------------------------------------------------
    pub stream: bool,
    pub resident_layers: Option<usize>,
    pub prefetch_depth: usize,
    pub auto_prefetch: bool,

    // --- compute ------------------------------------------------------------
    pub device: Device,
    pub multi_gpu_ids: Vec<u32>,
    pub max_batch: usize,
    /// `--gpu-layers`: first N layers on the GPU, the rest on the CPU.
    pub gpu_layers: Option<u32>,

    // --- generation ---------------------------------------------------------
    /// `auto` (dlm's default: detect from the checkpoint) or a template name.
    /// A string, not an enum, so a template dlm adds later needs no app change.
    pub chat_template: String,
    pub eos_token: Option<u32>,
    /// `None` keeps dlm's device-dependent default (64 on GPU, off on CPU).
    pub prefix_cache_size: Option<usize>,

    // --- speculative decoding ----------------------------------------------
    pub speculative: Speculative,
    pub draft_model_path: Option<PathBuf>,
    pub draft_gamma: usize,

    // --- server -------------------------------------------------------------
    pub api_key: Option<String>,
    /// Emits `--telemetry`. Only sent when the installed dlm advertises the
    /// flag (see `Capabilities` in the supervisor) — an older engine would
    /// otherwise refuse to start with a clap error.
    pub telemetry: bool,
}

impl Default for ServeConfig {
    /// Mirrors `ServeArgs`' clap defaults so that a default config produces the
    /// shortest possible argv.
    fn default() -> Self {
        Self {
            model_path: PathBuf::new(),
            port: 8000,
            host: "127.0.0.1".into(),
            quant: Quant::Checkpoint,
            kv_quant: KvQuant::F16,
            vram_budget_gb: None,
            safety_margin_gb: None,
            ram_cache_gb: None,
            expert_cache_gb: None,
            context_length: 8192,
            stream: false,
            resident_layers: None,
            prefetch_depth: 1,
            auto_prefetch: false,
            device: Device::Auto,
            multi_gpu_ids: Vec::new(),
            max_batch: 8,
            gpu_layers: None,
            chat_template: "auto".into(),
            eos_token: None,
            prefix_cache_size: None,
            speculative: Speculative::Auto,
            draft_model_path: None,
            draft_gamma: 4,
            api_key: None,
            telemetry: false,
        }
    }
}

impl ServeConfig {
    /// Build the argument vector for `dlm serve`.
    ///
    /// `telemetry_supported` comes from probing the installed binary; passing
    /// `false` silently drops the flag so the server still starts (the UI falls
    /// back to `/metrics` polling and says so) instead of dying on an unknown
    /// argument.
    pub fn to_argv(&self, telemetry_supported: bool) -> Vec<String> {
        let d = ServeConfig::default();
        let mut a: Vec<String> = vec!["serve".into()];

        push_val(
            &mut a,
            "--model-path",
            self.model_path.display().to_string(),
        );

        if self.port != d.port {
            push_val(&mut a, "--port", self.port.to_string());
        }
        if self.host != d.host {
            push_val(&mut a, "--host", self.host.clone());
        }
        if let Some(q) = self.quant.flag_value() {
            push_val(&mut a, "--quant", q.to_string());
        }
        if self.kv_quant != d.kv_quant {
            push_val(&mut a, "--kv-quant", self.kv_quant.as_str().to_string());
        }
        if let Some(v) = self.vram_budget_gb {
            push_val(&mut a, "--vram-budget-gb", fmt_gb(v));
        }
        if let Some(v) = self.safety_margin_gb {
            push_val(&mut a, "--safety-margin-gb", fmt_gb(v));
        }
        if let Some(v) = self.ram_cache_gb {
            push_val(&mut a, "--ram-cache-gb", fmt_gb(v));
        }
        if let Some(v) = self.expert_cache_gb {
            push_val(&mut a, "--expert-cache-gb", fmt_gb(v));
        }
        if self.context_length != d.context_length {
            push_val(&mut a, "--context-length", self.context_length.to_string());
        }
        if self.stream {
            a.push("--stream".into());
        }
        if let Some(n) = self.resident_layers {
            push_val(&mut a, "--resident-layers", n.to_string());
        }
        if self.prefetch_depth != d.prefetch_depth {
            push_val(&mut a, "--prefetch-depth", self.prefetch_depth.to_string());
        }
        if self.auto_prefetch {
            a.push("--auto-prefetch".into());
        }
        match self.device {
            Device::Auto => {}
            Device::Cpu => push_val(&mut a, "--device", "cpu".into()),
            Device::Gpu => push_val(&mut a, "--device", "gpu".into()),
        }
        if !self.multi_gpu_ids.is_empty() {
            let ids = self
                .multi_gpu_ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            push_val(&mut a, "--multi-gpu-ids", ids);
        }
        if self.max_batch != d.max_batch {
            push_val(&mut a, "--max-batch", self.max_batch.to_string());
        }
        if let Some(n) = self.gpu_layers {
            push_val(&mut a, "--gpu-layers", n.to_string());
        }
        let template = self.chat_template.trim();
        if !template.is_empty() && template != d.chat_template {
            push_val(&mut a, "--chat-template", template.to_string());
        }
        if let Some(t) = self.eos_token {
            push_val(&mut a, "--eos-token", t.to_string());
        }
        if let Some(n) = self.prefix_cache_size {
            push_val(&mut a, "--prefix-cache-size", n.to_string());
        }
        if self.speculative != d.speculative {
            push_val(
                &mut a,
                "--speculative",
                self.speculative.as_str().to_string(),
            );
        }
        if let Some(p) = &self.draft_model_path {
            push_val(&mut a, "--draft-model-path", p.display().to_string());
        }
        // Gamma now sizes n-gram lookup's proposals too, not only a draft
        // model's, so it is meaningful whenever speculation can run.
        if self.draft_gamma != d.draft_gamma && self.speculative != Speculative::Off {
            push_val(&mut a, "--draft-gamma", self.draft_gamma.to_string());
        }
        if let Some(k) = self.api_key.as_ref().filter(|k| !k.is_empty()) {
            push_val(&mut a, "--api-key", k.clone());
        }
        if self.telemetry && telemetry_supported {
            a.push("--telemetry".into());
        }
        a
    }

    /// The argv rendered as a copy-pasteable command line.
    ///
    /// The API key is redacted: this string goes on screen and into the log
    /// panel, and a key that leaks there is a key that leaks into screenshots.
    pub fn display_command(&self, binary: &str, telemetry_supported: bool) -> String {
        let mut out = String::from(binary);
        let argv = self.to_argv(telemetry_supported);
        let mut it = argv.iter().peekable();
        while let Some(arg) = it.next() {
            out.push(' ');
            if arg == "--api-key" {
                out.push_str("--api-key ****");
                let _ = it.next(); // swallow the value
                continue;
            }
            out.push_str(&quote_if_needed(arg));
        }
        out
    }

    /// Which changes need the server restarted? For dlm the answer is "all of
    /// them" — every one of these flags is read once at startup — so this is a
    /// statement of fact for the UI, not a policy.
    pub fn requires_restart(&self, other: &Self) -> bool {
        self != other
    }
}

fn push_val(argv: &mut Vec<String>, flag: &str, value: String) {
    argv.push(flag.to_string());
    argv.push(value);
}

/// Format a GiB value without a trailing `.0`, so the displayed command reads
/// `--vram-budget-gb 6` rather than `--vram-budget-gb 6.0`.
fn fmt_gb(v: f64) -> String {
    if (v.fract()).abs() < f64::EPSILON {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn quote_if_needed(s: &str) -> String {
    if s.is_empty() || s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ServeConfig {
        ServeConfig {
            model_path: PathBuf::from("models/Qwen3-0.6B"),
            ..Default::default()
        }
    }

    /// A default config must produce nothing but the model path: anything else
    /// would be this app overriding a dlm default the user never touched.
    #[test]
    fn default_config_emits_only_the_model_path() {
        let argv = cfg().to_argv(true);
        assert_eq!(
            argv,
            vec!["serve", "--model-path", "models/Qwen3-0.6B"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn streaming_flags_render_in_dlm_spelling() {
        let c = ServeConfig {
            stream: true,
            resident_layers: Some(2),
            prefetch_depth: 3,
            auto_prefetch: true,
            quant: Quant::Int4,
            ..cfg()
        };
        let argv = c.to_argv(true);
        let joined = argv.join(" ");
        assert!(joined.contains("--stream"), "{joined}");
        assert!(joined.contains("--resident-layers 2"), "{joined}");
        assert!(joined.contains("--prefetch-depth 3"), "{joined}");
        assert!(joined.contains("--auto-prefetch"), "{joined}");
        assert!(joined.contains("--quant int4"), "{joined}");
    }

    /// `Quant::Checkpoint` means "don't pass --quant at all" — dlm then keeps
    /// the checkpoint's own dtype. Emitting `--quant none` would be wrong; dlm
    /// has no such value.
    #[test]
    fn checkpoint_quant_emits_no_flag() {
        assert!(!cfg().to_argv(true).contains(&"--quant".to_string()));
    }

    #[test]
    fn telemetry_is_dropped_when_the_engine_does_not_support_it() {
        let c = ServeConfig {
            telemetry: true,
            ..cfg()
        };
        assert!(c.to_argv(true).contains(&"--telemetry".to_string()));
        assert!(!c.to_argv(false).contains(&"--telemetry".to_string()));
    }

    /// Gamma also sizes n-gram lookup now, so it goes out whenever speculation
    /// is not switched off.
    #[test]
    fn draft_gamma_is_dropped_only_when_speculation_is_off() {
        let c = ServeConfig {
            draft_gamma: 8,
            ..cfg()
        };
        assert!(c.to_argv(true).contains(&"--draft-gamma".to_string()));

        let c = ServeConfig {
            speculative: Speculative::Off,
            ..c
        };
        let argv = c.to_argv(true).join(" ");
        assert!(!argv.contains("--draft-gamma"), "{argv}");
        assert!(argv.contains("--speculative off"), "{argv}");
    }

    /// dlm's own defaults (`auto` template, `f16` KV, device-dependent prefix
    /// cache) must produce no flag; explicit choices must.
    #[test]
    fn new_engine_defaults_stay_off_the_command_line() {
        let c = ServeConfig {
            chat_template: "gemma".into(),
            kv_quant: KvQuant::F32,
            prefix_cache_size: Some(0),
            gpu_layers: Some(16),
            ..cfg()
        };
        let joined = c.to_argv(true).join(" ");
        assert!(joined.contains("--chat-template gemma"), "{joined}");
        assert!(joined.contains("--kv-quant f32"), "{joined}");
        assert!(joined.contains("--prefix-cache-size 0"), "{joined}");
        assert!(joined.contains("--gpu-layers 16"), "{joined}");
    }

    /// Older UI state said `none` for exact f32; it must still deserialize.
    #[test]
    fn kv_quant_none_reads_as_f32() {
        let k: KvQuant = serde_json::from_str("\"none\"").unwrap();
        assert_eq!(k, KvQuant::F32);
    }

    #[test]
    fn multi_gpu_ids_are_comma_joined() {
        let c = ServeConfig {
            multi_gpu_ids: vec![0, 1, 3],
            ..cfg()
        };
        assert!(c.to_argv(true).join(" ").contains("--multi-gpu-ids 0,1,3"));
    }

    /// The displayed command lands in the log panel and in screenshots.
    #[test]
    fn displayed_command_redacts_the_api_key() {
        let c = ServeConfig {
            api_key: Some("s3cret-token".into()),
            ..cfg()
        };
        let shown = c.display_command("dlm", true);
        assert!(shown.contains("--api-key ****"), "{shown}");
        assert!(!shown.contains("s3cret-token"), "{shown}");
        // ...but the real argv still carries it.
        assert!(c.to_argv(true).contains(&"s3cret-token".to_string()));
    }

    #[test]
    fn gb_values_lose_a_pointless_trailing_zero() {
        let c = ServeConfig {
            vram_budget_gb: Some(6.0),
            safety_margin_gb: Some(0.5),
            ..cfg()
        };
        let joined = c.to_argv(true).join(" ");
        assert!(joined.contains("--vram-budget-gb 6 "), "{joined}");
        assert!(joined.contains("--safety-margin-gb 0.5"), "{joined}");
    }

    #[test]
    fn paths_with_spaces_are_quoted_for_display_only() {
        let c = ServeConfig {
            model_path: PathBuf::from("C:/My Models/Qwen"),
            ..cfg()
        };
        assert!(c
            .display_command("dlm", true)
            .contains("\"C:/My Models/Qwen\""));
        // The spawned argv passes the raw path; the OS handles it, not a shell.
        assert!(c.to_argv(true).contains(&"C:/My Models/Qwen".to_string()));
    }
}
