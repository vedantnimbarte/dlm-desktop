# dlm-desktop

A desktop UI for [dlm](https://github.com/vedantnimbarte/dlm), the dynamic
layer-streaming inference engine.

It does what you would expect from a local-LLM app — conversations, model
management, a settings panel — and one thing those apps cannot do: it shows you
**where a transformer layer actually spends its time** on the way from storage to
the GPU, measured rather than guessed, and tells you which stage is the
bottleneck.

```
NVMe ─▶ page cache ─▶ host RAM cache ─▶ pinned staging ─▶ PCIe ─▶ VRAM ─▶ SM
```

That matters because dlm's whole proposition is running models that do not fit,
and the cost of doing so is bandwidth. A UI that makes the cost visible turns
`--quant`, `--resident-layers` and `--ram-cache-gb` from documentation into
something you can see the effect of.

## Status

| | |
|---|---|
| ✅ | Streaming chat against a local or remote `dlm serve`, with per-turn tok/s and TTFT |
| ✅ | Connection supervision — spawn and own a server, or attach to one |
| ✅ | Conversation history in SQLite, with full-text search across every chat |
| ✅ | Model manager: scan checkpoints, and a VRAM fit preview per `--quant` |
| ✅ | Full `dlm serve` flag editor with a verbatim command preview |
| ✅ | API log — every request with latency, TTFT and token counts |
| ✅ | Live flow view: measured per-stage bandwidth, layer residency, issue detection |
| ✅ | Engine telemetry (A1–A5) landed in dlm |
| ✅ | CI and release bundles for Windows, Linux and macOS |

## Architecture

Two ways of talking to dlm, on purpose:

- **As a library** (`dlm` crate) for work that must happen *before* a server
  exists — reading the local model catalog, planning VRAM, tokenizing.
- **As a process** (`dlm serve`) for inference, over HTTP + SSE. An engine OOM
  or CUDA fault then kills a process, not the UI.

Local and remote connections use the same client, so nothing above
`src-tauri/src/engine` cares who owns the process.

There is deliberately **no HTTP client crate**. dlm's server is itself
dependency-free HTTP/1.1 with no TLS, so `src-tauri/src/client/http.rs` is a few
hundred lines over `std::net::TcpStream` — which also gives exact control over
SSE framing, the thing the telemetry stream depends on.

## Requirements

- Rust 1.85+
- Node 18+
- A `dlm` binary on `PATH` (or point at one in Settings)
- The `dlm` repo checked out **next to this one** — `src-tauri/Cargo.toml`
  currently uses `dlm = { path = "../../dlm" }` so the engine telemetry work can
  be developed in lockstep. Release builds should pin a git tag instead.

## Development

```sh
npm install
npm run tauri dev
```

Tests:

```sh
cd src-tauri
cargo test                       # unit tests, hermetic

# integration tests against a real server
dlm serve --model-path models/Qwen3-0.6B --device cpu --stream \
          --resident-layers 2 --port 8123 &
DLM_LIVE_ADDR=127.0.0.1:8123 cargo test --test live_server -- --nocapture
```

The live tests are skipped unless `DLM_LIVE_ADDR` (or `DLM_MODELS_DIR`) is set,
so `cargo test` stays hermetic. They exist because unit tests cannot prove
interoperability with a real engine, and both bugs that mattered most were only
visible end to end:

- an SSE frame cap that aborted the telemetry stream precisely when there was
  activity to report, and
- a subscriber gate that let a departing connection switch collection off
  underneath an arriving one.

Both looked identical to "nothing happened" from the UI.

Run the whole set against a real engine:

```sh
dlm serve --model-path models/Qwen3-0.6B --device cpu --stream \
          --resident-layers 3 --ram-cache-gb 2 --chat-template plain \
          --port 8130 --telemetry &

DLM_LIVE_ADDR=127.0.0.1:8130 \
DLM_MODELS_DIR=../../dlm/models \
  cargo test --all-targets -- --test-threads=1
```

## CI and releases

`.github/workflows/ci.yml` runs fmt, clippy (`-D warnings`), the tests and a
real Tauri link step on Windows, Linux and macOS. It checks the engine out as a
sibling directory so CI reproduces the `../../dlm` path dependency rather than
special-casing the manifest — what builds there is what builds locally.

`.github/workflows/release.yml` builds signed bundles (msi/nsis, AppImage/deb,
dmg for both Mac architectures) on a `v*` tag and opens a draft release. Set the
`DLM_ENGINE_REF` repository variable to the engine tag a release was tested
against; signing secrets are optional, and their absence produces an unsigned
bundle rather than a failed build.

## Engine changes this depends on

The flow view needs telemetry `/metrics` cannot provide: aggregate counters
answer "is the window working?" but not "where did the time go?". These landed
in the dlm repo on `feat/telemetry-events`:

| | |
|---|---|
| A1 | A telemetry event bus with a bounded ring buffer, gated so it costs nothing when off |
| A2 | Emission at the existing seams: `streaming.rs`'s load/compute timers, `CachedLayerSource`'s hit/miss, `upload_async`'s stage/copy split, the prefetch queue, LRU eviction |
| A3 | `cudaEventElapsedTime` / `hipEventElapsedTime` FFI plus a `CopyTimer`, so an H2D copy is timed by the device rather than by wall-clock around an enqueue |
| A4 | `GET /v1/telemetry` (SSE) behind `--telemetry`, inside the `--api-key` guard |
| A5 | `hub::pull_with_progress`, so downloads can show real progress |

Against an engine without them, the app probes `dlm serve --help`, drops the
flag rather than failing to start, and says "telemetry unavailable" in the
status bar. **No event is ever fabricated to fill the gap** — a measurement the
engine did not make is not shown as one. The same rule runs through the whole
feature: a CPU run draws no PCIe hop rather than inventing one, an estimated
duration renders as a dashed edge, dropped events surface as "sampled at N%",
and a diagnostic rule with a missing counter declines to fire rather than
assuming a default.

Telemetry events carry timings, byte counts and layer indices only. No prompt or
completion text ever enters them, which is what makes the endpoint safe to expose
to a remote UI.

## License

Apache-2.0, matching dlm.
