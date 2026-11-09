import { useStore } from "../store";

/** Bottom bar: the one place that always says what the engine is doing. */
export default function StatusBar() {
  const status = useStore((s) => s.status);
  const caps = useStore((s) => s.caps);
  const metrics = useStore((s) => s.metrics);
  const flowOpen = useStore((s) => s.flowOpen);
  const setFlowOpen = useStore((s) => s.setFlowOpen);

  let badge = { cls: "", dot: "", label: "" };
  switch (status.kind) {
    case "disconnected":
      badge = { cls: "", dot: "", label: "Disconnected" };
      break;
    case "starting":
      badge = {
        cls: "busy",
        dot: "",
        // A cold load can run for minutes; showing the clock is the difference
        // between "working" and "hung".
        label: `Loading ${shortPath(status.model)} · ${status.elapsedSecs}s`,
      };
      break;
    case "ready":
      badge = { cls: "ok", dot: "", label: `Ready · ${status.modelId}` };
      break;
    case "failed":
      badge = { cls: "error", dot: "", label: "Engine failed" };
      break;
  }

  const stream = metrics?.stream ?? null;
  const hitRate =
    stream && stream.hits + stream.misses > 0
      ? (stream.hits / (stream.hits + stream.misses)) * 100
      : null;

  return (
    <footer className="statusbar">
      <span className={`badge ${badge.cls}`}>
        <span className="dot" />
        {badge.label}
      </span>

      {caps ? (
        <span className="dim mono">dlm {caps.version}</span>
      ) : (
        <span className="dim">dlm binary not found</span>
      )}

      {caps && !caps.telemetry && (
        <span
          className="dim"
          title="This dlm build has no --telemetry flag, so per-layer flow events are unavailable. The flow view falls back to /metrics polling."
        >
          telemetry unavailable
        </span>
      )}

      <span className="spacer" />

      <button
        title="Watch layers move from storage through to compute"
        onClick={() => setFlowOpen(!flowOpen)}
      >
        {flowOpen ? "Hide flow" : "Show flow"}
      </button>


      {stream ? (
        <>
          <span className="mono" title="Layer fetches served from the resident window">
            window hit {hitRate === null ? "—" : `${hitRate.toFixed(0)}%`}
          </span>
          <span className="mono" title="Live prefetch depth">
            prefetch {stream.depth}
          </span>
          <span className="mono" title="Layers evicted from the resident window">
            evictions {stream.evictions}
          </span>
        </>
      ) : (
        status.kind === "ready" && (
          <span className="dim" title="All layers are resident; nothing is streaming.">
            resident (not streaming)
          </span>
        )
      )}

      {metrics && (
        <span className="dim mono">
          {metrics.completion_tokens} tokens generated
        </span>
      )}
    </footer>
  );
}

function shortPath(p: string): string {
  const parts = p.replace(/\\/g, "/").split("/").filter(Boolean);
  return parts[parts.length - 1] ?? p;
}
