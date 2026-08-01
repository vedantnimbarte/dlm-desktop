//! Typed client for the dlm server's HTTP API.
//!
//! [`http`] is the transport; this module is the vocabulary — the handful of
//! routes dlm actually exposes (`dlm/src/server/engine.rs:729`):
//!
//! | route | used for |
//! |---|---|
//! | `GET /health` | liveness while a spawned server binds its port |
//! | `GET /v1/models` | which model this server is actually serving |
//! | `POST /v1/chat/completions` | chat, streamed token by token |
//! | `GET /metrics` | aggregate counters (the fallback when telemetry is off) |
//! | `GET /v1/telemetry` | per-layer flow events (added by engine change A4) |
//!
//! The same code path serves a locally-spawned sidecar and a remote attach.

pub mod http;

pub use http::{Cancel, Endpoint, Flow, HttpError, SseFrame};

use serde::{Deserialize, Serialize};

/// One chat turn, in the OpenAI shape dlm parses (`ChatMessage`,
/// `dlm/src/server/openai.rs:20`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
}

/// Per-request sampling knobs. All optional: omitted fields let the server keep
/// its own defaults rather than this client inventing them.
///
/// The floats are `f64` deliberately. As `f32` a temperature of 0.7 widens to
/// `0.699999988079071` on the wire, which is harmless to the sampler but lands
/// verbatim in the API log panel and reads like a bug.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sampling {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stop: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    #[serde(flatten)]
    sampling: &'a Sampling,
}

/// A model as `/v1/models` reports it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelCard {
    pub id: String,
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub owned_by: String,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelCard>,
}

/// Streaming chunk shape (`Chunk`/`ChunkChoice`/`Delta`,
/// `dlm/src/server/engine.rs:508`). Every field is optional because the first
/// chunk carries only the role and the last carries only a finish reason.
#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: usize,
    #[serde(default)]
    pub completion_tokens: usize,
}

/// What a streaming chat call hands back to the caller, piece by piece.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// A visible token delta to append to the assistant message.
    Delta(String),
    /// The model stopped; `reason` is `"stop"` or `"length"`.
    Finished {
        reason: Option<String>,
        usage: Option<Usage>,
    },
}

/// Aggregate server counters, parsed from the Prometheus text at `/metrics`.
///
/// The streaming gauges are `None` on a fully-resident run: dlm omits them
/// entirely rather than reporting zeros (`dlm/src/server/engine.rs:672`), and
/// that distinction matters to the UI — "not streaming" is a different state
/// from "streaming with no hits".
#[derive(Debug, Clone, Default, Serialize)]
pub struct ServerMetrics {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub stream: Option<StreamGauges>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StreamGauges {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub prefetched: u64,
    pub depth: u64,
    pub expert_hits: u64,
    pub expert_misses: u64,
    pub expert_evictions: u64,
}

impl StreamGauges {
    /// Share of layer fetches served from the resident window. `None` when
    /// nothing has been fetched yet — a 0.0 there would render as a scary red
    /// bar before the first token.
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.hits + self.misses;
        (total > 0).then(|| self.hits as f64 / total as f64)
    }
}

/// Is the server up and serving?
pub fn health(endpoint: &Endpoint) -> bool {
    matches!(
        http::request(endpoint, "GET", "/health", None),
        Ok(r) if (200..300).contains(&r.status)
    )
}

/// The models this server reports. dlm binds one model per process, so this is
/// a one-element list in practice — but it is the authoritative answer to "what
/// am I actually talking to", which matters after a respawn.
pub fn models(endpoint: &Endpoint) -> Result<Vec<ModelCard>, HttpError> {
    let resp = http::request(endpoint, "GET", "/v1/models", None)?.ok_or_status()?;
    let parsed: ModelsResponse = serde_json::from_slice(&resp.body)
        .map_err(|e| HttpError::Protocol(format!("/v1/models: {e}")))?;
    Ok(parsed.data)
}

/// Scrape and parse `/metrics`.
pub fn metrics(endpoint: &Endpoint) -> Result<ServerMetrics, HttpError> {
    let resp = http::request(endpoint, "GET", "/metrics", None)?.ok_or_status()?;
    Ok(parse_prometheus(&resp.text()))
}

/// Run a chat completion, streaming deltas to `on_event`.
///
/// Returns `Err(HttpError::Cancelled)` if `cancel` was flipped — the caller
/// keeps whatever text arrived before that, which is exactly what "stop"
/// should do.
pub fn chat_stream(
    endpoint: &Endpoint,
    model: &str,
    messages: &[ChatMessage],
    sampling: &Sampling,
    cancel: &Cancel,
    mut on_event: impl FnMut(ChatEvent) -> Flow,
) -> Result<(), HttpError> {
    let body = serde_json::to_vec(&ChatRequest {
        model,
        messages,
        stream: true,
        sampling,
    })
    .map_err(|e| HttpError::Protocol(format!("serialize chat request: {e}")))?;

    http::stream_sse(
        endpoint,
        "POST",
        "/v1/chat/completions",
        Some(&body),
        cancel,
        |frame| {
            if frame.is_done() {
                return Flow::Stop;
            }
            let chunk: ChatChunk = match serde_json::from_str(&frame.data) {
                Ok(c) => c,
                // A frame we cannot parse is not worth killing a live
                // generation over; skip it and keep reading.
                Err(_) => return Flow::Continue,
            };
            let mut flow = Flow::Continue;
            for choice in &chunk.choices {
                if let Some(text) = &choice.delta.content {
                    if !text.is_empty() {
                        flow = on_event(ChatEvent::Delta(text.clone()));
                    }
                }
                if let Some(reason) = &choice.finish_reason {
                    flow = on_event(ChatEvent::Finished {
                        reason: Some(reason.clone()),
                        usage: chunk.usage,
                    });
                }
            }
            // A usage-only trailer (speculative decoding sends one with no
            // choices) still closes out the turn.
            if chunk.choices.is_empty() && chunk.usage.is_some() {
                flow = on_event(ChatEvent::Finished {
                    reason: None,
                    usage: chunk.usage,
                });
            }
            flow
        },
    )
}

/// Parse the subset of Prometheus text dlm emits: bare `name value` lines with
/// `#`-prefixed comments and no labels.
fn parse_prometheus(text: &str) -> ServerMetrics {
    let mut m = ServerMetrics::default();
    let mut gauges = StreamGauges::default();
    let mut saw_stream = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(v) = value.trim().parse::<f64>() else {
            continue;
        };
        let v = v as u64;
        match name {
            "dlm_requests_total" => m.requests = v,
            "dlm_prompt_tokens_total" => m.prompt_tokens = v,
            "dlm_completion_tokens_total" => m.completion_tokens = v,
            "dlm_stream_layer_hits_total" => {
                gauges.hits = v;
                saw_stream = true;
            }
            "dlm_stream_layer_misses_total" => {
                gauges.misses = v;
                saw_stream = true;
            }
            "dlm_stream_layer_evictions_total" => {
                gauges.evictions = v;
                saw_stream = true;
            }
            "dlm_stream_layer_prefetched_total" => {
                gauges.prefetched = v;
                saw_stream = true;
            }
            "dlm_stream_prefetch_depth" => {
                gauges.depth = v;
                saw_stream = true;
            }
            "dlm_stream_expert_hits_total" => {
                gauges.expert_hits = v;
                saw_stream = true;
            }
            "dlm_stream_expert_misses_total" => {
                gauges.expert_misses = v;
                saw_stream = true;
            }
            "dlm_stream_expert_evictions_total" => {
                gauges.expert_evictions = v;
                saw_stream = true;
            }
            _ => {}
        }
    }
    m.stream = saw_stream.then_some(gauges);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly the bytes dlm writes on a resident (non-streaming) run.
    const RESIDENT: &str = "\
# HELP dlm_requests_total Completed generation requests.
# TYPE dlm_requests_total counter
dlm_requests_total 7
# HELP dlm_prompt_tokens_total Prompt tokens processed.
# TYPE dlm_prompt_tokens_total counter
dlm_prompt_tokens_total 120
# HELP dlm_completion_tokens_total Tokens generated.
# TYPE dlm_completion_tokens_total counter
dlm_completion_tokens_total 340
";

    const STREAMING: &str = "\
dlm_requests_total 2
dlm_prompt_tokens_total 10
dlm_completion_tokens_total 20
dlm_stream_layer_hits_total 30
dlm_stream_layer_misses_total 10
dlm_stream_layer_evictions_total 28
dlm_stream_layer_prefetched_total 9
dlm_stream_prefetch_depth 3
";

    #[test]
    fn parses_the_resident_form_and_leaves_stream_absent() {
        let m = parse_prometheus(RESIDENT);
        assert_eq!(m.requests, 7);
        assert_eq!(m.prompt_tokens, 120);
        assert_eq!(m.completion_tokens, 340);
        // Absent, not zeroed: "not streaming" must stay distinguishable from
        // "streaming with no activity".
        assert!(m.stream.is_none());
    }

    #[test]
    fn parses_the_streaming_form() {
        let m = parse_prometheus(STREAMING);
        let s = m.stream.expect("streaming gauges present");
        assert_eq!(s.hits, 30);
        assert_eq!(s.misses, 10);
        assert_eq!(s.evictions, 28);
        assert_eq!(s.prefetched, 9);
        assert_eq!(s.depth, 3);
        assert_eq!(s.hit_rate(), Some(0.75));
    }

    #[test]
    fn hit_rate_is_none_before_any_fetch() {
        assert_eq!(StreamGauges::default().hit_rate(), None);
    }

    #[test]
    fn garbage_lines_are_ignored_not_fatal() {
        let m = parse_prometheus("nonsense\ndlm_requests_total notanumber\ndlm_requests_total 4\n");
        assert_eq!(m.requests, 4);
    }

    #[test]
    fn chat_request_flattens_sampling_next_to_the_messages() {
        let msgs = vec![ChatMessage::user("hi")];
        let sampling = Sampling {
            temperature: Some(0.7),
            max_tokens: Some(64),
            ..Default::default()
        };
        let json = serde_json::to_value(ChatRequest {
            model: "m",
            messages: &msgs,
            stream: true,
            sampling: &sampling,
        })
        .unwrap();
        assert_eq!(json["stream"], true);
        assert_eq!(json["temperature"], 0.7);
        assert_eq!(json["max_tokens"], 64);
        // Unset knobs must not appear at all; sending nulls would override the
        // server's own defaults.
        assert!(json.get("top_p").is_none());
        assert!(json.get("seed").is_none());
    }
}
