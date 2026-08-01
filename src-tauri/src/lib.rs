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
pub mod models;
pub mod store;

use client::{Cancel, ChatEvent, ChatMessage, Flow, Sampling, ServerMetrics};
use engine::{Capabilities, LogLine, ServeConfig, Status, Supervisor};
use models::{FitPreview, LocalModel};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use store::{ApiLogEntry, Conversation, SearchHit, StoredMessage};
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
    /// Conversation history. `None` only if the database could not be opened,
    /// in which case chat still works and nothing is persisted — losing history
    /// is better than refusing to start.
    store: Option<store::Store>,
}

impl AppState {
    fn new(db_path: Option<PathBuf>) -> Self {
        let store = db_path.and_then(|p| match store::Store::open(&p) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("dlm-desktop: could not open {}: {e}", p.display());
                None
            }
        });
        Self {
            supervisor: Arc::new(Supervisor::new()),
            chat_cancel: Mutex::new(None),
            next_request_id: AtomicU64::new(1),
            binary_override: Mutex::new(None),
            store,
        }
    }

    fn store(&self) -> UiResult<&store::Store> {
        self.store.as_ref().ok_or_else(|| UiError {
            kind: "noStore".into(),
            message: "conversation history is unavailable (database could not be opened)".into(),
        })
    }
}

/// How much of a request or response body the API log keeps. Enough to see the
/// shape of what was sent without storing a full conversation twice.
const PREVIEW_LIMIT: usize = 2000;

/// Truncate on a char boundary, marking that it was truncated.
fn preview(s: &str) -> String {
    if s.len() <= PREVIEW_LIMIT {
        return s.to_string();
    }
    let mut end = PREVIEW_LIMIT;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes total)", &s[..end], s.len())
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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

impl From<store::StoreError> for UiError {
    fn from(e: store::StoreError) -> Self {
        Self {
            kind: "store".into(),
            message: e.to_string(),
        }
    }
}

impl From<models::ModelError> for UiError {
    fn from(e: models::ModelError) -> Self {
        Self {
            kind: "model".into(),
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
// Models
// ---------------------------------------------------------------------------

/// Directory to scan for checkpoints when the user has not chosen one.
#[tauri::command]
fn default_models_root() -> Option<String> {
    models::default_models_root().map(|p| p.display().to_string())
}

#[tauri::command(async)]
fn scan_models(root: String) -> UiResult<Vec<LocalModel>> {
    Ok(models::scan(Path::new(&root))?)
}

/// "Will it fit, and how much will stream?" — computed from the checkpoint on
/// disk with dlm's own `VramProfiler`, before any server is started.
#[tauri::command(async)]
fn model_fit(
    path: String,
    context_length: u32,
    max_batch: u32,
    safety_margin_gb: Option<f64>,
    vram_budget_gb: Option<f64>,
) -> UiResult<FitPreview> {
    Ok(models::fit_preview(
        Path::new(&path),
        context_length,
        max_batch,
        safety_margin_gb,
        vram_budget_gb,
    )?)
}

// ---------------------------------------------------------------------------
// Conversation history
// ---------------------------------------------------------------------------

#[tauri::command]
fn list_conversations(state: State<'_, AppState>) -> UiResult<Vec<Conversation>> {
    Ok(state.store()?.list_conversations()?)
}

#[tauri::command]
fn create_conversation(
    state: State<'_, AppState>,
    id: String,
    title: String,
) -> UiResult<Conversation> {
    Ok(state.store()?.create_conversation(&id, &title, now_ms())?)
}

#[tauri::command]
fn rename_conversation(state: State<'_, AppState>, id: String, title: String) -> UiResult<()> {
    Ok(state.store()?.rename_conversation(&id, &title, now_ms())?)
}

#[tauri::command]
fn delete_conversation(state: State<'_, AppState>, id: String) -> UiResult<()> {
    Ok(state.store()?.delete_conversation(&id)?)
}

#[tauri::command]
fn conversation_messages(
    state: State<'_, AppState>,
    conversation_id: String,
) -> UiResult<Vec<StoredMessage>> {
    Ok(state.store()?.messages(&conversation_id)?)
}

#[tauri::command]
fn append_message(
    state: State<'_, AppState>,
    conversation_id: String,
    id: String,
    role: String,
    content: String,
    meta: Option<String>,
) -> UiResult<i64> {
    let store = state.store()?;
    let seq = store.next_seq(&conversation_id)?;
    store.append_message(&StoredMessage {
        id,
        conversation_id,
        seq,
        role,
        content,
        ts: now_ms(),
        meta,
    })?;
    Ok(seq)
}

/// Persist a streamed message once it is complete.
///
/// Called at the end of a turn rather than per token: writing on every delta
/// would be thousands of transactions for one answer, and a partial answer is
/// not worth persisting anyway.
#[tauri::command]
fn finalize_message(
    state: State<'_, AppState>,
    id: String,
    content: String,
    meta: Option<String>,
) -> UiResult<()> {
    Ok(state.store()?.update_message(&id, &content, meta.as_deref())?)
}

/// Drop a message and everything after it — "regenerate" and "edit & branch".
#[tauri::command]
fn truncate_conversation(
    state: State<'_, AppState>,
    conversation_id: String,
    seq: i64,
) -> UiResult<()> {
    Ok(state.store()?.truncate_from(&conversation_id, seq)?)
}

#[tauri::command]
fn search_messages(
    state: State<'_, AppState>,
    query: String,
    limit: Option<i64>,
) -> UiResult<Vec<SearchHit>> {
    Ok(state.store()?.search(&query, limit.unwrap_or(50))?)
}

#[tauri::command]
fn api_log(state: State<'_, AppState>, limit: Option<i64>) -> UiResult<Vec<ApiLogEntry>> {
    Ok(state.store()?.api_log(limit.unwrap_or(200))?)
}

#[tauri::command]
fn clear_api_log(state: State<'_, AppState>) -> UiResult<()> {
    Ok(state.store()?.clear_api_log()?)
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

    // Previewed now, while `messages` is still owned here: the log records what
    // was actually sent, not a reconstruction.
    let request_preview = preview(&serde_json::to_string(&messages).unwrap_or_default());

    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let started_at = now_ms();
        let mut first_token_at: Option<std::time::Duration> = None;
        let mut delta_count = 0usize;
        let mut done: Option<DoneEvent> = None;
        let mut answer = String::new();

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
                    // Kept only up to the preview cap, so a long generation
                    // does not accumulate a second copy of itself here.
                    if answer.len() < PREVIEW_LIMIT {
                        answer.push_str(&text);
                    }
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

        // One API-log row per turn, whatever the outcome. A request that failed
        // or was cancelled is exactly the one someone opens this panel to look
        // at, so it is recorded on every path rather than only on success.
        let (status, error) = match &result {
            Ok(()) => (Some(200), None),
            Err(client::HttpError::Cancelled) => (None, Some("cancelled by user".to_string())),
            Err(client::HttpError::Status { status, body }) => {
                (Some(*status as i64), Some(preview(body)))
            }
            Err(e) => (None, Some(e.to_string())),
        };
        let entry = ApiLogEntry {
            id: 0,
            ts: started_at,
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            status,
            latency_ms: Some(started.elapsed().as_millis() as i64),
            ttft_ms: first_token_at.map(|d| d.as_millis() as i64),
            prompt_tokens: done.as_ref().and_then(|d| d.prompt_tokens).map(|n| n as i64),
            completion_tokens: done
                .as_ref()
                .and_then(|d| d.completion_tokens)
                .map(|n| n as i64),
            request_preview: Some(request_preview),
            response_preview: (!answer.is_empty()).then(|| preview(&answer)),
            error,
        };
        if let Some(store) = app.state::<AppState>().store.as_ref() {
            let _ = store.log_api(&entry);
        }
        let _ = app.emit("api:logged", &entry);

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
            // History lives in the platform's app-data directory so it survives
            // reinstalls and does not litter the working directory.
            let db_path = app
                .path()
                .app_data_dir()
                .map(|dir| dir.join("history.sqlite3"))
                .ok();
            let state = AppState::new(db_path);
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
            default_models_root,
            scan_models,
            model_fit,
            list_conversations,
            create_conversation,
            rename_conversation,
            delete_conversation,
            conversation_messages,
            append_message,
            finalize_message,
            truncate_conversation,
            search_messages,
            api_log,
            clear_api_log,
        ])
        .run(tauri::generate_context!())
        .expect("error while running dlm-desktop");
}
