//! Connection supervision: own a local `dlm serve`, or attach to a remote one.
//!
//! dlm binds **one model per process** by design, so "load a different model"
//! is "restart the server". That is not hidden here — the supervisor exposes it
//! as an explicit [`Status::Starting`] phase with the model being loaded, so
//! the UI can show a real state rather than a spinner over a stalled chat box.
//!
//! Both connection kinds converge on the same [`client::Endpoint`], so
//! everything above this module is indifferent to who owns the process.

pub mod config;

pub use config::{ChatTemplate, Device, KvQuant, Quant, ServeConfig};

use crate::client::{self, Endpoint};
use serde::Serialize;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Lines kept from the server's stdout/stderr. Enough to diagnose a failed
/// start (dlm prints its VRAM plan and any fallback warnings there) without
/// growing without bound over a long session.
const LOG_CAPACITY: usize = 2000;

/// How long to wait for a spawned server to answer `/health`. A cold start
/// mmaps the checkpoint and may quantize every layer, which on a large model
/// over a slow disk is genuinely minutes — failing at 30s would report a
/// working setup as broken.
const START_TIMEOUT: Duration = Duration::from_secs(600);

const HEALTH_POLL: Duration = Duration::from_millis(250);

/// Where the connection is in its lifecycle.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Status {
    Disconnected,
    /// A local server is booting. `model` is the directory being loaded.
    Starting {
        model: String,
        /// Seconds since the spawn, so the UI can say "still loading (45s)"
        /// rather than looking hung.
        elapsed_secs: u64,
    },
    Ready {
        model_id: String,
        endpoint: String,
        /// True when this app owns the process (and may therefore stop it).
        local: bool,
    },
    /// Something went wrong; `detail` is the engine's own words where possible.
    Failed {
        detail: String,
    },
}

/// One captured line of engine output.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    /// Milliseconds since the supervisor started, for ordering in the UI.
    pub t_ms: u64,
    pub stream: LogStream,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogStream {
    Stdout,
    Stderr,
    /// Emitted by the supervisor itself (spawn, exit, retry).
    Supervisor,
}

/// What the installed engine binary can do.
///
/// Probed once per binary path. The telemetry flag is the one that matters:
/// passing `--telemetry` to a dlm that predates engine change A4 makes clap
/// abort before the server ever binds, so the app asks first rather than
/// producing a confusing startup failure.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub binary: String,
    pub version: String,
    pub telemetry: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("no dlm binary found. Install it, or set the binary path in Settings.")]
    BinaryNotFound,
    #[error("failed to start {binary}: {source}")]
    Spawn {
        binary: String,
        #[source]
        source: std::io::Error,
    },
    #[error("dlm exited during startup ({status}). Last output:\n{tail}")]
    ExitedEarly { status: String, tail: String },
    #[error("dlm did not become healthy within {0:?}")]
    StartTimeout(Duration),
    #[error("{0}")]
    Client(#[from] client::HttpError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// Receives each captured log line. Named so the supervisor's field stays
/// readable rather than a four-deep nested generic.
pub type LogSink = Arc<dyn Fn(LogLine) + Send + Sync>;

/// A locally-owned server process.
struct LocalServer {
    child: Child,
}

impl Drop for LocalServer {
    /// The child must not outlive the app. Without this, closing the window
    /// would leave a server holding VRAM with no UI attached to it.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Inner {
    status: Status,
    endpoint: Option<Endpoint>,
    local: Option<LocalServer>,
    config: Option<ServeConfig>,
    caps: Option<Capabilities>,
    log: VecDeque<LogLine>,
    started: Instant,
}

/// Owns the connection to exactly one dlm server at a time.
pub struct Supervisor {
    inner: Mutex<Inner>,
    /// Called for every captured log line, so the UI can stream engine output
    /// live. Kept as a plain callback to leave Tauri types out of this module.
    on_log: Mutex<Option<LogSink>>,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                status: Status::Disconnected,
                endpoint: None,
                local: None,
                config: None,
                caps: None,
                log: VecDeque::with_capacity(LOG_CAPACITY),
                started: Instant::now(),
            }),
            on_log: Mutex::new(None),
        }
    }

    pub fn set_log_sink(&self, sink: LogSink) {
        *self.on_log.lock().unwrap() = Some(sink);
    }

    /// Current status.
    ///
    /// This is also where a crashed local server is noticed. A dead child is
    /// detected on read rather than by a dedicated monitor thread: the UI polls
    /// status anyway, and `try_wait` here avoids sharing a `Child` across
    /// threads just to observe it. There is deliberately **no** auto-restart —
    /// the common crash is an out-of-memory model load, and silently retrying
    /// that would loop while telling the user nothing.
    pub fn status(&self) -> Status {
        let exited = self.child_exit_status();
        if let Some(status) = exited {
            let was_running = {
                let inner = self.inner.lock().unwrap();
                matches!(inner.status, Status::Ready { .. } | Status::Starting { .. })
            };
            if was_running {
                let tail = self.log_tail(20);
                let detail = format!("dlm exited unexpectedly ({status}). Last output:\n{tail}");
                self.set_status(Status::Failed {
                    detail: detail.clone(),
                });
                let local = self.inner.lock().unwrap().local.take();
                drop(local);
                self.push_log(LogStream::Supervisor, format!("engine exited: {status}"));
                return Status::Failed { detail };
            }
        }

        let inner = self.inner.lock().unwrap();
        // Refresh the elapsed counter on read so a slow load visibly ticks.
        match &inner.status {
            Status::Starting { model, .. } => Status::Starting {
                model: model.clone(),
                elapsed_secs: inner.started.elapsed().as_secs(),
            },
            other => other.clone(),
        }
    }

    pub fn endpoint(&self) -> Option<Endpoint> {
        self.inner.lock().unwrap().endpoint.clone()
    }

    pub fn config(&self) -> Option<ServeConfig> {
        self.inner.lock().unwrap().config.clone()
    }

    pub fn capabilities(&self) -> Option<Capabilities> {
        self.inner.lock().unwrap().caps.clone()
    }

    pub fn logs(&self) -> Vec<LogLine> {
        self.inner.lock().unwrap().log.iter().cloned().collect()
    }

    fn push_log(&self, stream: LogStream, text: String) {
        let line = {
            let mut inner = self.inner.lock().unwrap();
            let line = LogLine {
                t_ms: inner.started.elapsed().as_millis() as u64,
                stream,
                text,
            };
            if inner.log.len() == LOG_CAPACITY {
                inner.log.pop_front();
            }
            inner.log.push_back(line.clone());
            line
        };
        // Notify outside the lock: the sink re-enters the app and must not be
        // able to deadlock against a status read.
        let sink = self.on_log.lock().unwrap().clone();
        if let Some(sink) = sink {
            sink(line);
        }
    }

    fn set_status(&self, status: Status) {
        self.inner.lock().unwrap().status = status;
    }

    /// Stop whatever is currently connected. Idempotent.
    pub fn disconnect(&self) {
        let local = {
            let mut inner = self.inner.lock().unwrap();
            inner.endpoint = None;
            inner.config = None;
            inner.status = Status::Disconnected;
            inner.local.take()
        };
        if local.is_some() {
            self.push_log(LogStream::Supervisor, "stopping dlm serve".into());
        }
        drop(local); // Drop impl kills and reaps
    }

    /// Point at a server this app does not own.
    ///
    /// Verifies liveness *and* identity: a reachable port is not proof it is a
    /// dlm server, so `/v1/models` must answer with a model card before this
    /// reports success.
    pub fn attach_remote(&self, address: &str, api_key: Option<String>) -> Result<Status> {
        self.disconnect();
        let endpoint = Endpoint::parse(address)
            .map_err(EngineError::Other)?
            .with_api_key(api_key);

        self.push_log(
            LogStream::Supervisor,
            format!("attaching to {}", endpoint.base_url()),
        );

        let cards = client::models(&endpoint)?;
        let model_id = cards
            .first()
            .map(|c| c.id.clone())
            .ok_or_else(|| EngineError::Other("server reported no models".into()))?;

        let status = Status::Ready {
            model_id,
            endpoint: endpoint.base_url(),
            local: false,
        };
        {
            let mut inner = self.inner.lock().unwrap();
            inner.endpoint = Some(endpoint);
            inner.status = status.clone();
        }
        Ok(status)
    }

    /// Spawn and own a `dlm serve` for `config`, waiting until it is healthy.
    ///
    /// Takes `&Arc<Self>` because the stdout/stderr capture threads outlive the
    /// call and need a handle to push log lines through.
    pub fn spawn_local(
        self: &Arc<Self>,
        config: ServeConfig,
        binary_override: Option<PathBuf>,
    ) -> Result<Status> {
        self.disconnect();

        let binary = resolve_binary(binary_override)?;
        let caps = probe_capabilities(&binary);
        let argv = config.to_argv(caps.telemetry);

        self.push_log(
            LogStream::Supervisor,
            format!("$ {}", config.display_command(&caps.binary, caps.telemetry)),
        );
        if config.telemetry && !caps.telemetry {
            // Say it out loud rather than quietly showing a flow view built on
            // nothing: this dlm cannot emit per-layer events.
            self.push_log(
                LogStream::Supervisor,
                format!(
                    "note: dlm {} has no --telemetry flag; the flow view will fall back to /metrics polling",
                    caps.version
                ),
            );
        }

        let mut cmd = Command::new(&binary);
        cmd.args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        no_console_window(&mut cmd);

        let mut child = cmd.spawn().map_err(|source| EngineError::Spawn {
            binary: binary.display().to_string(),
            source,
        })?;

        self.capture(child.stdout.take(), LogStream::Stdout);
        self.capture(child.stderr.take(), LogStream::Stderr);

        let endpoint = Endpoint::local(config.port).with_api_key(config.api_key.clone());
        {
            let mut inner = self.inner.lock().unwrap();
            inner.started = Instant::now();
            inner.status = Status::Starting {
                model: config.model_path.display().to_string(),
                elapsed_secs: 0,
            };
            inner.endpoint = Some(endpoint.clone());
            inner.config = Some(config.clone());
            inner.caps = Some(caps);
            inner.local = Some(LocalServer { child });
        }

        match self.await_healthy(&endpoint) {
            Ok(model_id) => {
                let status = Status::Ready {
                    model_id,
                    endpoint: endpoint.base_url(),
                    local: true,
                };
                self.set_status(status.clone());
                self.push_log(LogStream::Supervisor, "server ready".into());
                Ok(status)
            }
            Err(e) => {
                let detail = e.to_string();
                self.set_status(Status::Failed {
                    detail: detail.clone(),
                });
                self.push_log(LogStream::Supervisor, format!("start failed: {detail}"));
                // Leave nothing running behind a failed start.
                let local = self.inner.lock().unwrap().local.take();
                drop(local);
                Err(e)
            }
        }
    }

    /// Poll `/health` until the server answers, the child dies, or we time out.
    fn await_healthy(&self, endpoint: &Endpoint) -> Result<String> {
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            // A child that has already exited will never become healthy;
            // detecting that turns a 10-minute wait into an immediate, and far
            // more useful, error carrying dlm's own message.
            if let Some(status) = self.child_exit_status() {
                let tail = self.log_tail(20);
                return Err(EngineError::ExitedEarly { status, tail });
            }
            if client::health(endpoint) {
                let cards = client::models(endpoint)?;
                return cards
                    .first()
                    .map(|c| c.id.clone())
                    .ok_or_else(|| EngineError::Other("server reported no models".into()));
            }
            if Instant::now() >= deadline {
                return Err(EngineError::StartTimeout(START_TIMEOUT));
            }
            std::thread::sleep(HEALTH_POLL);
        }
    }

    /// `Some(description)` once the owned child has exited.
    fn child_exit_status(&self) -> Option<String> {
        let mut inner = self.inner.lock().unwrap();
        let local = inner.local.as_mut()?;
        match local.child.try_wait() {
            Ok(Some(status)) => Some(match status.code() {
                Some(code) => format!("exit code {code}"),
                None => "terminated by signal".to_string(),
            }),
            _ => None,
        }
    }

    fn log_tail(&self, n: usize) -> String {
        let inner = self.inner.lock().unwrap();
        let skip = inner.log.len().saturating_sub(n);
        inner
            .log
            .iter()
            .skip(skip)
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Drain a child pipe into the log on its own thread.
    fn capture<R: std::io::Read + Send + 'static>(
        self: &Arc<Self>,
        pipe: Option<R>,
        stream: LogStream,
    ) {
        let Some(pipe) = pipe else { return };
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            let reader = BufReader::new(pipe);
            for line in reader.lines() {
                match line {
                    Ok(text) => this.push_log(stream, text),
                    Err(_) => break,
                }
            }
        });
    }
}

/// Find the dlm binary: explicit override, then `PATH`, then the locations the
/// official installers use (`install.ps1` / `install.sh`).
pub fn resolve_binary(override_path: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = override_path {
        return if p.is_file() {
            Ok(p)
        } else {
            Err(EngineError::BinaryNotFound)
        };
    }
    let exe = if cfg!(windows) { "dlm.exe" } else { "dlm" };

    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    for dir in installer_dirs() {
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(EngineError::BinaryNotFound)
}

fn installer_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Programs").join("dlm"));
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(&home).join(".local").join("bin"));
    }
    dirs
}

/// Ask the binary what it is and what it supports.
///
/// `dlm serve --help` is the authority on whether `--telemetry` exists; parsing
/// the help text is cheap, runs once, and avoids guessing from a version number
/// (a user may well be running a build from a branch).
pub fn probe_capabilities(binary: &std::path::Path) -> Capabilities {
    let version = run_capture(binary, &["--version"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let help = run_capture(binary, &["serve", "--help"]).unwrap_or_default();
    Capabilities {
        binary: binary.display().to_string(),
        version,
        telemetry: help.contains("--telemetry"),
    }
}

fn run_capture(binary: &std::path::Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(binary);
    cmd.args(args).stdin(Stdio::null());
    no_console_window(&mut cmd);
    let out = cmd.output().ok()?;
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    // clap writes `--help` to stdout but errors to stderr; take both so a
    // non-zero exit still tells us something.
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(s)
}

/// Keep a console window from flashing up on Windows every time we spawn or
/// probe the CLI.
#[cfg(windows)]
fn no_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_console_window(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binary_override_is_rejected() {
        let err = resolve_binary(Some(PathBuf::from("/definitely/not/here/dlm")));
        assert!(matches!(err, Err(EngineError::BinaryNotFound)));
    }

    #[test]
    fn a_fresh_supervisor_is_disconnected_and_quiet() {
        let s = Supervisor::new();
        assert!(matches!(s.status(), Status::Disconnected));
        assert!(s.endpoint().is_none());
        assert!(s.logs().is_empty());
    }

    #[test]
    fn disconnect_is_idempotent() {
        let s = Supervisor::new();
        s.disconnect();
        s.disconnect();
        assert!(matches!(s.status(), Status::Disconnected));
    }

    #[test]
    fn log_ring_buffer_drops_the_oldest_lines() {
        let s = Supervisor::new();
        for i in 0..LOG_CAPACITY + 50 {
            s.push_log(LogStream::Stdout, format!("line {i}"));
        }
        let logs = s.logs();
        assert_eq!(logs.len(), LOG_CAPACITY);
        assert_eq!(logs[0].text, format!("line {}", 50));
    }

    #[test]
    fn log_tail_returns_the_most_recent_lines() {
        let s = Supervisor::new();
        for i in 0..10 {
            s.push_log(LogStream::Stderr, format!("l{i}"));
        }
        assert_eq!(s.log_tail(3), "l7\nl8\nl9");
    }

    /// Attaching somewhere nothing is listening must fail rather than report a
    /// connection the UI would then hang against.
    #[test]
    fn attach_to_a_dead_port_fails() {
        let s = Supervisor::new();
        // Port 1 is reserved and never a dlm server.
        assert!(s.attach_remote("127.0.0.1:1", None).is_err());
        assert!(matches!(s.status(), Status::Disconnected));
    }

    #[test]
    fn attach_rejects_an_unparseable_address() {
        let s = Supervisor::new();
        assert!(s.attach_remote("https://secure:8000", None).is_err());
    }
}
