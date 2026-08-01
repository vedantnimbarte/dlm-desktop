import { useEffect, useState } from "react";
import { api } from "../api";
import { useStore } from "../store";
import { formatBytes } from "../format";
import type { FlowSnapshot, LayerState, StageStat } from "../types";

/** Poll rate for the flow view. The engine batches at 20 Hz; polling faster
 *  would just re-render the same numbers. */
const POLL_MS = 250;

/** Stages of the data path, in the order bytes travel through them.
 *
 * `key` names the FlowState field; `edgeFrom`/`edgeTo` name the nodes it moves
 * bytes between. A stage with no `edge` is a node's own work rather than a
 * transfer. */
const NODES = [
  { id: "storage", label: "NVMe", sub: "checkpoint", color: "var(--stage-storage)" },
  { id: "ram", label: "Host RAM", sub: "layer cache", color: "var(--stage-ram)" },
  { id: "pinned", label: "Pinned", sub: "staging", color: "var(--stage-pinned)" },
  { id: "vram", label: "VRAM", sub: "resident window", color: "var(--stage-vram)" },
  { id: "compute", label: "Compute", sub: "decode block", color: "var(--stage-compute)" },
] as const;

export default function FlowView() {
  const status = useStore((s) => s.status);
  const caps = useStore((s) => s.caps);
  const [snap, setSnap] = useState<FlowSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  const ready = status.kind === "ready";

  // Subscribing is what makes the engine start collecting, so this effect owns
  // the engine's telemetry state: mount subscribes, unmount unsubscribes.
  useEffect(() => {
    if (!ready) {
      setSnap(null);
      return;
    }
    let stopped = false;
    api
      .flowSubscribe()
      .then(() => setError(null))
      .catch((e) => setError(errText(e)));

    const tick = async () => {
      try {
        const next = await api.flowState();
        if (!stopped) setSnap(next);
      } catch {
        /* transient; the next tick retries */
      }
    };
    void tick();
    const id = setInterval(tick, POLL_MS);
    return () => {
      stopped = true;
      clearInterval(id);
      void api.flowUnsubscribe();
    };
  }, [ready]);

  if (!ready) {
    return (
      <div className="flow-empty">
        Connect to a model to watch layers move through the memory hierarchy.
      </div>
    );
  }

  if (caps && !caps.telemetry) {
    return (
      <div className="flow-empty">
        This dlm build has no <code>--telemetry</code> flag, so it emits no
        per-layer events. Nothing is shown here rather than an animation built
        on numbers the engine never reported.
      </div>
    );
  }

  if (error) return <div className="flow-empty error-text">{error}</div>;
  if (!snap) return <div className="flow-empty dim">Connecting to telemetry…</div>;

  const { flow, hw, findings } = snap;

  if (!flow.active) {
    return (
      <div className="flow-empty dim">
        Waiting for activity — send a message and the layer traffic appears here.
      </div>
    );
  }

  const gpu = hw.gpus[0];
  const isGpu = flow.snapshot?.device === "gpu";

  return (
    <div className="flow">
      <div className="flow-main">
        <Topology flow={flow} isGpu={isGpu} />
        <LayerStrip layers={flow.layers} residentLayers={flow.snapshot?.residentLayers ?? 0} />
      </div>

      <div className="flow-side">
        <Verdict flow={flow} />
        <Findings findings={findings} />
        {gpu && <GpuPanel gpu={gpu} />}
        {!gpu && hw.gpuUnavailableReason && (
          <div className="flow-note dim">{hw.gpuUnavailableReason}</div>
        )}
      </div>
    </div>
  );
}

/** The data path, with edge weight and label driven by measured bandwidth. */
function Topology({ flow, isGpu }: { flow: FlowSnapshot["flow"]; isGpu: boolean }) {
  // On unified memory (and on a CPU run) there is no PCIe hop and no separate
  // VRAM. Drawing one would be a fabrication, so the topology is data-driven.
  const nodes = isGpu
    ? NODES
    : NODES.filter((n) => n.id !== "pinned" && n.id !== "vram");

  const edges = isGpu
    ? [
        { from: "storage", to: "ram", stat: flow.ramMiss, label: "read" },
        { from: "ram", to: "pinned", stat: flow.pinStage, label: "stage" },
        { from: "pinned", to: "vram", stat: flow.h2d, label: "PCIe" },
        { from: "vram", to: "compute", stat: flow.compute, label: "decode" },
      ]
    : [
        { from: "storage", to: "ram", stat: flow.ramMiss, label: "read" },
        { from: "ram", to: "compute", stat: flow.compute, label: "decode" },
      ];

  return (
    <div className="topology">
      {nodes.map((node, i) => (
        <div key={node.id} className="topo-group">
          <div className="topo-node" style={{ borderColor: node.color }}>
            <div className="topo-label">{node.label}</div>
            <div className="topo-sub dim">{node.sub}</div>
          </div>
          {i < edges.length && <Edge edge={edges[i]} />}
        </div>
      ))}
    </div>
  );
}

function Edge({
  edge,
}: {
  edge: { from: string; to: string; stat: StageStat; label: string };
}) {
  const { stat, label } = edge;
  const bw = stat.bytesPerSec;
  // Thickness is log-scaled: real bandwidths across these stages span three
  // orders of magnitude, and a linear scale would render the disk edge as a
  // hairline next to the bus.
  const thickness = bw ? Math.max(2, Math.min(14, 2 + Math.log10(bw) * 1.4)) : 2;
  const idle = !bw;

  return (
    <div className="topo-edge">
      <div
        className={`edge-line${idle ? " idle" : ""}${stat.estimated ? " estimated" : ""}`}
        style={{ height: thickness }}
      />
      <div className="edge-label mono">
        <span>{label}</span>
        {bw ? (
          <span className="edge-rate">
            {formatBytes(bw)}/s
            {stat.estimated && <span className="edge-est"> est</span>}
          </span>
        ) : (
          <span className="dim">idle</span>
        )}
      </div>
    </div>
  );
}

/** One cell per transformer layer, coloured by what it is doing. */
function LayerStrip({
  layers,
  residentLayers,
}: {
  layers: LayerState[];
  residentLayers: number;
}) {
  if (layers.length === 0) return null;
  return (
    <div className="layer-strip-wrap">
      <div className="layer-strip-head dim">
        <span>layers</span>
        <span className="mono">
          {residentLayers > 0 ? `${residentLayers} resident` : ""}
        </span>
      </div>
      <div className="layer-strip">
        {layers.map((l) => (
          <div
            key={l.layer}
            className={`layer-cell ${l.residency}`}
            title={`layer ${l.layer} — ${l.residency}${
              l.evictions ? `, ${l.evictions} evictions` : ""
            }${l.bytes ? `, ${formatBytes(l.bytes)}` : ""}`}
          />
        ))}
      </div>
      <div className="layer-legend dim">
        <span><i className="sw resident" /> resident</span>
        <span><i className="sw prefetched" /> prefetched</span>
        <span><i className="sw evicted" /> evicted</span>
        <span><i className="sw absent" /> absent</span>
      </div>
    </div>
  );
}

/** The one-line answer: where is the time actually going? */
function Verdict({ flow }: { flow: FlowSnapshot["flow"] }) {
  const transferUs =
    flow.ramMiss.totalUs + flow.pinStage.totalUs + flow.h2d.totalUs;
  const computeUs = flow.compute.totalUs;
  const total = transferUs + computeUs;
  if (total === 0) return null;
  const transferPct = (transferUs / total) * 100;

  return (
    <div className="verdict">
      <div className="verdict-bar">
        <div
          className="verdict-transfer"
          style={{ width: `${transferPct}%` }}
          title={`moving bytes: ${(transferUs / 1000).toFixed(0)} ms`}
        />
        <div
          className="verdict-compute"
          style={{ width: `${100 - transferPct}%` }}
          title={`computing: ${(computeUs / 1000).toFixed(0)} ms`}
        />
      </div>
      <div className="verdict-text">
        <strong>{transferPct.toFixed(0)}%</strong> moving bytes ·{" "}
        <strong>{(100 - transferPct).toFixed(0)}%</strong> computing
      </div>
      <div className="verdict-sub dim mono">
        {flow.windowSecs.toFixed(1)}s window · token {flow.token}
        {flow.prefetchDepth != null && ` · prefetch ${flow.prefetchDepth}`}
        {flow.sampledFraction < 0.999 &&
          ` · sampled ${(flow.sampledFraction * 100).toFixed(0)}%`}
      </div>
    </div>
  );
}

function Findings({ findings }: { findings: FlowSnapshot["findings"] }) {
  if (findings.length === 0) {
    return (
      <div className="flow-note ok-text">
        No issues detected in this window.
      </div>
    );
  }
  return (
    <div className="findings">
      {findings.map((f) => (
        <div key={f.id} className={`finding ${f.severity}`}>
          <div className="finding-title">{f.title}</div>
          <div className="finding-detail">{f.detail}</div>
          {f.fix && <div className="finding-fix">{f.fix}</div>}
        </div>
      ))}
    </div>
  );
}

function GpuPanel({ gpu }: { gpu: NonNullable<FlowSnapshot["hw"]["gpus"][0]> }) {
  const link =
    gpu.pcieLinkGen != null && gpu.pcieLinkWidth != null
      ? `Gen${gpu.pcieLinkGen} x${gpu.pcieLinkWidth}`
      : null;
  return (
    <div className="gpu-panel">
      <div className="gpu-name">{gpu.name}</div>
      <div className="kv">
        <span>VRAM</span>
        <span>
          {gpu.vramUsed != null && gpu.vramTotal != null
            ? `${formatBytes(gpu.vramUsed)} / ${formatBytes(gpu.vramTotal)}`
            : "—"}
        </span>
      </div>
      <div className="kv">
        <span>Utilization</span>
        <span>{gpu.utilizationPct != null ? `${gpu.utilizationPct}%` : "—"}</span>
      </div>
      <div className="kv">
        <span>Temperature</span>
        <span>{gpu.temperatureC != null ? `${gpu.temperatureC}°C` : "—"}</span>
      </div>
      <div className="kv">
        <span>PCIe link</span>
        <span>{link ?? "—"}</span>
      </div>
      {gpu.throttleReasons.length > 0 && (
        <div className="kv">
          <span>Throttled</span>
          <span className="warn-text">{gpu.throttleReasons.join(", ")}</span>
        </div>
      )}
    </div>
  );
}

function errText(e: unknown): string {
  if (e && typeof e === "object" && "message" in e)
    return String((e as { message: unknown }).message);
  return String(e);
}
