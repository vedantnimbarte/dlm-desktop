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

Early. Built in milestones; what is working today:

| | |
|---|---|
| ✅ | Streaming chat against a local or remote `dlm serve` |
| ✅ | Connection supervision — spawn and own a server, or attach to one |
| ✅ | Full `dlm serve` flag editor with a verbatim command preview |
| ✅ | Live engine log panel |
| ⏳ | Conversation persistence (SQLite), model manager with VRAM fit preview |
| ⏳ | Flow visualization (needs the engine telemetry work, below) |
| ⏳ | Hardware counters and issue detection |

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

The live tests are skipped unless `DLM_LIVE_ADDR` is set, so `cargo test` stays
hermetic. They exist because unit tests cannot prove interoperability with dlm's
hand-rolled HTTP server — its header framing, its `Connection: close` SSE bodies,
and its chunk shapes.

## Engine changes this depends on

The flow visualization needs telemetry that dlm does not emit yet. Today the
engine exposes only aggregate Prometheus counters at `/metrics`
(`dlm_stream_layer_hits_total` and friends) — no timestamped per-layer events and
no per-stage timings. The planned additions, tracked against the dlm repo:

| | |
|---|---|
| A1 | A telemetry event bus with a bounded ring buffer, gated so it costs nothing when off |
| A2 | Emission at the existing seams: `streaming.rs` load/compute timers, `LayerRamCache::get_or_load`, `upload_async`'s stage/copy split, the prefetch worker, LRU eviction |
| A3 | `cudaEventElapsedTime` FFI, so an H2D copy is timed by the GPU rather than by wall-clock around a `synchronize()` |
| A4 | `GET /v1/telemetry` (SSE) behind a `--telemetry` flag, inside the `--api-key` guard |
| A5 | A progress callback on `hub::pull`, so downloads can show a real progress bar |

Until A4 lands, the app detects the missing `--telemetry` flag by probing
`dlm serve --help`, drops the flag rather than failing to start, and says so in
the status bar. **No event is ever fabricated to fill the gap** — a measurement
the engine did not make is not shown as one.

Telemetry events carry timings, byte counts and layer indices only. No prompt or
completion text ever enters them, which is what makes the endpoint safe to expose
to a remote UI.

## License

Apache-2.0, matching dlm.
