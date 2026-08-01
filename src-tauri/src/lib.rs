//! dlm-desktop — Tauri backend.
//!
//! Layout mirrors the responsibilities:
//!
//! - [`client`] — HTTP/SSE transport and the typed dlm API.
//! - [`engine`] — owning a local `dlm serve` or attaching to a remote one,
//!   plus the `ServeConfig` behind the settings panel.
//!
//! Commands below are the whole surface the frontend can reach. Long-running
//! work (a chat completion) runs on a worker thread and reports back through
//! Tauri events rather than blocking the command, so the UI stays responsive
//! and "stop" can take effect mid-stream.

pub mod client;
pub mod engine;

use client::{Cancel, ChatEvent, ChatMessage, Flow, Sampling, ServerMetrics};
use engine::{Capabilities, LogLine, ServeConfig, Status, Supervisor};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

/// Shared backend state.
pub struct AppState {
    supervisor: Arc<Supervisor>,
    /// Cancel token for the in-flight chat request, if any. Replacing it is how
    /// a new request supersedes an abandoned one.
    chat_cancel: Mutex<Option<Cancel>>,
    next_request_id: AtomicU64,
    /// User override for the dlm binary location (Settings).
    binary_override: Mutex<Option<PathBuf>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            supervisor: Arc::new(Supervisor::new()),
            chat_cancel: Mutex::new(None),
            next_request_id: AtomicU64::new(1),
            binary_override: Mutex::new(None),
        }
    }
}

/// Errors crossing into JS. Rust error types are flattened to a message plus a
/// coarse kind, so the UI can distinguish "no binary" (offer to browse for it)
/// from "engine said no" (show its words).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiError {
    kind: String,
    message: String,
}

impl From<engine::EngineError> for UiError {
    fn from(e: engine::EngineError) -> Self {
        let kind = match &e {
            engine::EngineError::BinaryNotFound => "binaryNotFound",
            engine::EngineError::Spawn { .. } => "spawn",
            engine::EngineError::ExitedEarly { .. } => "exitedEarly",
            engine::EngineError::StartTimeout(_) => "startTimeout",
            engine::EngineError::Client(_) => "client",
            engine::EngineError::Other(_) => "other",
        };
        Self {
            kind: kind.into(),
            message: e.to_string(),
        }
    }
}

impl From<client::HttpError> for UiError {
    fn from(e: client::HttpError) -> Self {
        Self {
            kind: "client".into(),
            message: e.to_string(),
        }
    }
}

type UiResult<T> = std::result::Result<T, UiError>;

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

#[tauri::command]
fn engine_status(state: State<'_, AppState>) -> Status {
    state.supervisor.status()
}

#[tauri::command]
fn engine_capabilities(state: State<'_, AppState>) -> Option<Capabilities> {
    state.supervisor.capabilities()
}

#[tauri::command]
fn engine_logs(state: State<'_, AppState>) -> Vec<LogLine> {
    state.supervisor.logs()
}

/// Locate the dlm binary and report what it supports, without starting it.
/// Called on app start so the UI can say "dlm 0.4.0 found" or offer to browse.
#[tauri::command]
fn probe_engine(state: State<'_, AppState>) -> UiResult<Capabilities> {
    let override_path = state.binary_override.lock().unwrap().clone();
    let binary = engine::resolve_binary(override_path)?;
    Ok(engine::probe_capabilities(&binary))
}

#[tauri::command]
fn set_binary_override(state: State<'_, AppState>, path: Option<String>) {
    *state.binary_override.lock().unwrap() = path.filter(|p| !p.is_empty()).map(PathBuf::from);
}

/// Start a local server. Blocks until healthy or failed — a model load is the
/// one thing the user is genuinely waiting on, and the UI shows the elapsed
/// time from `engine_status` while this runs.
#[tauri::command(async)]
fn start_local(state: State<'_, AppState>, config: ServeConfig) -> UiResult<Status> {
    let override_path = state.binary_override.lock().unwrap().clone();
    Ok(state.supervisor.spawn_local(config, override_path)?)
}

#[tauri::command(async)]
fn attach_remote(
    state: State<'_, AppState>,
    address: String,
    api_key: Option<String>,
) -> UiResult<Status> {
    Ok(state.supervisor.attach_remote(&address, api_key)?)
}

#[tauri::command]
fn disconnect(state: State<'_, AppState>) {
    state.supervisor.disconnect();
}

#[tauri::command]
fn server_metrics(state: State<'_, AppState>) -> UiResult<ServerMetrics> {
    let endpoint = state
        .supervisor
        .endpoint()
        .ok_or_else(|| UiError {
            kind: "notConnected".into(),
            message: "not connected to a dlm server".into(),
        })?;
    Ok(client::metrics(&endpoint)?)
}

/// Render the exact command a config would run, for display. The API key is
/// redacted by `display_command`.
#[tauri::command]
fn preview_command(state: State<'_, AppState>, config: ServeConfig) -> String {
    let caps = state.supervisor.capabilities();
    let (binary, telemetry) = match &caps {
        Some(c) => (c.binary.clone(), c.telemetry),
        None => ("dlm".to_string(), false),
    };
    config.display_command(&binary, telemetry)
}

#[tauri::command]
fn default_serve_config() -> ServeConfig {
    ServeConfig::default()
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

/// Payload for the `chat:delta` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeltaEvent {
    request_id: u64,
    text: String,
}

/// Payload for `chat:done`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoneEvent {
    request_id: u64,
    /// `"stop"`, `"length"`, or `"cancelled"`.
    reason: String,
    prompt_tokens: Option<usize>,
    completion_tokens: Option<usize>,
    /// Wall-clock from request start to first visible token.
    ttft_ms: Option<u64>,
    /// Visible tokens per second over the generation phase.
    tokens_per_sec: Option<f64>,
}

/// Payload for `chat:error`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorEvent {
    request_id: u64,
    message: String,
}

/// Start a streaming chat completion.
///
/// Returns the request id immediately; deltas arrive as `chat:delta` events and
/// the turn ends with exactly one `chat:done` or `chat:error`.
#[tauri::command]
fn chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    messages: Vec<ChatMessage>,
    sampling: Sampling,
) -> UiResult<u64> {
    let endpoint = state.supervisor.endpoint().ok_or_else(|| UiError {
        kind: "notConnected".into(),
        message: "not connected to a dlm server".into(),
    })?;
    let model = match state.supervisor.status() {
        Status::Ready { model_id, .. } => model_id,
        _ => {
            return Err(UiError {
                kind: "notReady".into(),
                message: "the server is not ready yet".into(),
            })
        }
    };

    let request_id = state.next_request_id.fetch_add(1, Ordering::Relaxed);
    let cancel = Cancel::new();
    // Supersede any previous in-flight request: two concurrent generations
    // would interleave into the same conversation.
    if let Some(prev) = state.chat_cancel.lock().unwrap().replace(cancel.clone()) {
        prev.cancel();
    }

    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut first_token_at: Option<std::time::Duration> = None;
        let mut delta_count = 0usize;
        let mut done: Option<DoneEvent> = None;

        let result = client::chat_stream(
            &endpoint,
            &model,
            &messages,
            &sampling,
            &cancel,
            |event| match event {
                ChatEvent::Delta(text) => {
                    if first_token_at.is_none() {
                        first_token_at = Some(started.elapsed());
                    }
                    delta_count += 1;
                    let _ = app.emit("chat:delta", DeltaEvent { request_id, text });
                    Flow::Continue
                }
                ChatEvent::Finished { reason, usage } => {
                    let ttft = first_token_at.map(|d| d.as_millis() as u64);
                    // Rate over the generation phase only — including the
                    // prefill would understate a streamed model's decode speed.
                    let gen_secs = first_token_at
                        .map(|t| (started.elapsed() - t).as_secs_f64())
                        .unwrap_or(0.0);
                    let counted = usage.map(|u| u.completion_tokens).unwrap_or(delta_count);
                    done = Some(DoneEvent {
                        request_id,
                        reason: reason.unwrap_or_else(|| "stop".into()),
                        prompt_tokens: usage.map(|u| u.prompt_tokens),
                        completion_tokens: usage.map(|u| u.completion_tokens),
                        ttft_ms: ttft,
                        tokens_per_sec: (gen_secs > 0.0 && counted > 0)
                            .then(|| counted as f64 / gen_secs),
                    });
                    Flow::Continue
                }
            },
        );

        match result {
            Ok(()) => {
                let _ = app.emit(
                    "chat:done",
                    done.unwrap_or(DoneEvent {
                        request_id,
                        reason: "stop".into(),
                        prompt_tokens: None,
                        completion_tokens: None,
                        ttft_ms: first_token_at.map(|d| d.as_millis() as u64),
                        tokens_per_sec: None,
                    }),
                );
            }
            // A user-requested stop is a normal ending, not an error: the text
            // generated so far stays in the conversation.
            Err(client::HttpError::Cancelled) => {
                let _ = app.emit(
                    "chat:done",
                    DoneEvent {
                        request_id,
                        reason: "cancelled".into(),
                        prompt_tokens: None,
                        completion_tokens: None,
                        ttft_ms: first_token_at.map(|d| d.as_millis() as u64),
                        tokens_per_sec: None,
                    },
                );
            }
            Err(e) => {
                let _ = app.emit(
                    "chat:error",
                    ErrorEvent {
                        request_id,
                        message: e.to_string(),
                    },
                );
            }
        }
    });

    Ok(request_id)
}

/// Stop the in-flight generation. Safe to call when nothing is running.
#[tauri::command]
fn chat_stop(state: State<'_, AppState>) {
    if let Some(cancel) = state.chat_cancel.lock().unwrap().take() {
        cancel.cancel();
    }
}

// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let state = AppState::new();
            // Stream engine output to the UI as it is produced, so a slow model
            // load shows dlm's own progress rather than a blank panel.
            let handle = app.handle().clone();
            state
                .supervisor
                .set_log_sink(Arc::new(move |line: LogLine| {
                    let _ = handle.emit("engine:log", line);
                }));
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_status,
            engine_capabilities,
            engine_logs,
            probe_engine,
            set_binary_override,
            start_local,
            attach_remote,
            disconnect,
            server_metrics,
            preview_command,
            default_serve_config,
            chat_send,
            chat_stop,
        ])
        .run(tauri::generate_context!())
        .expect("error while running dlm-desktop");
}
