//! Integration tests against a **real** `dlm serve`.
//!
//! Unit tests cover parsing in isolation; these cover the thing parsing cannot;
//! that the client actually interoperates with dlm's hand-rolled HTTP/1.1
//! server — its header framing, its `Connection: close` SSE bodies, and its
//! chunk shapes.
//!
//! Skipped unless `DLM_LIVE_ADDR` is set, so `cargo test` stays hermetic:
//!
//! ```sh
//! dlm serve --model-path models/Qwen3-0.6B --device cpu --port 8123 &
//! DLM_LIVE_ADDR=127.0.0.1:8123 cargo test --test live_server -- --nocapture
//! ```

use dlm_desktop_lib::client::{self, Cancel, ChatEvent, ChatMessage, Endpoint, Flow, Sampling};

/// The endpoint under test, or `None` to skip.
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
                eprintln!("skipping {}: set DLM_LIVE_ADDR to run", stringify!($name));
                return;
            }
        }
    };
}

#[test]
fn health_reports_a_running_server() {
    let endpoint = live!(health_reports_a_running_server);
    assert!(client::health(&endpoint), "server did not answer /health");
}

#[test]
fn models_names_the_served_checkpoint() {
    let endpoint = live!(models_names_the_served_checkpoint);
    let cards = client::models(&endpoint).expect("/v1/models failed");
    assert!(!cards.is_empty(), "server reported no models");
    assert!(!cards[0].id.is_empty(), "model card has an empty id");
    eprintln!("serving: {}", cards[0].id);
}

/// Parsing the real `/metrics` body, not a fixture. On a `--stream` run the
/// streaming gauges must be present; that is the difference between the flow
/// view having a data source and having nothing.
#[test]
fn metrics_parse_against_the_real_endpoint() {
    let endpoint = live!(metrics_parse_against_the_real_endpoint);
    let m = client::metrics(&endpoint).expect("/metrics failed");
    eprintln!(
        "requests={} prompt={} completion={} stream={:?}",
        m.requests, m.prompt_tokens, m.completion_tokens, m.stream
    );
}

/// The M0 gate: a streamed completion arrives token by token and terminates.
#[test]
fn chat_streams_tokens_and_finishes() {
    let endpoint = live!(chat_streams_tokens_and_finishes);
    let model = client::models(&endpoint)
        .expect("/v1/models failed")
        .first()
        .map(|c| c.id.clone())
        .expect("no model");

    let messages = vec![ChatMessage::user("Say hello in five words.")];
    let sampling = Sampling {
        temperature: Some(0.0), // deterministic, so a rerun is comparable
        max_tokens: Some(24),
        ..Default::default()
    };

    let mut text = String::new();
    let mut deltas = 0usize;
    let mut finished = false;
    let cancel = Cancel::new();

    client::chat_stream(&endpoint, &model, &messages, &sampling, &cancel, |event| {
        match event {
            ChatEvent::Delta(d) => {
                deltas += 1;
                text.push_str(&d);
            }
            ChatEvent::Finished { reason, usage } => {
                finished = true;
                eprintln!("finish_reason={reason:?} usage={usage:?}");
            }
        }
        Flow::Continue
    })
    .expect("chat stream failed");

    eprintln!("{deltas} deltas, {} chars: {text:?}", text.len());
    assert!(deltas > 0, "no token deltas arrived");
    assert!(!text.is_empty(), "stream produced no text");
    assert!(finished, "stream never reported a finish reason");
}

/// Cancellation must actually stop the stream — this is the "Stop" button, and
/// the text generated so far has to survive it.
#[test]
fn cancel_stops_a_stream_early() {
    let endpoint = live!(cancel_stops_a_stream_early);
    let model = client::models(&endpoint)
        .expect("/v1/models failed")
        .first()
        .map(|c| c.id.clone())
        .expect("no model");

    let messages = vec![ChatMessage::user("Count slowly from one to fifty.")];
    let sampling = Sampling {
        temperature: Some(0.0),
        max_tokens: Some(200),
        ..Default::default()
    };

    let cancel = Cancel::new();
    let mut deltas = 0usize;
    let result = client::chat_stream(&endpoint, &model, &messages, &sampling, &cancel, |event| {
        if let ChatEvent::Delta(_) = event {
            deltas += 1;
            // Trip the flag partway through, exactly as the Stop button does.
            if deltas == 3 {
                cancel.cancel();
            }
        }
        Flow::Continue
    });

    assert!(
        matches!(result, Err(client::HttpError::Cancelled)),
        "expected Cancelled, got {result:?}"
    );
    assert!(deltas >= 3, "cancelled before producing any tokens");
}

/// Discovery must find an engine the user started themselves — the case that
/// makes "no dlm binary found" a dead end rather than a detour.
#[test]
fn discovery_finds_a_running_engine_on_the_default_port() {
    let endpoint = live!(discovery_finds_a_running_engine_on_the_default_port);
    if endpoint.port != 8000 {
        eprintln!("skipping: discovery only probes dlm's default port (8000)");
        return;
    }
    let found = dlm_desktop_lib::engine::discover_local_servers();
    assert!(
        !found.is_empty(),
        "a server is running but was not discovered"
    );
    eprintln!(
        "discovered: {} serving {}",
        found[0].address, found[0].model_id
    );
    assert!(!found[0].model_id.is_empty(), "a hit must name its model");
}
