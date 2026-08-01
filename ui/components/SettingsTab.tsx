import { useEffect, useState } from "react";
import { api, errorMessage } from "../api";
import { useStore } from "../store";
import { pickExecutable } from "../pick";
import type { ServeConfig } from "../types";

/** Optional numeric input: empty means "don't pass the flag", which is not the
 * same as zero. dlm treats an absent flag as "use my own default", so the null
 * has to survive the round trip. */
function OptNumber({
  label,
  hint,
  value,
  step,
  min,
  placeholder,
  onChange,
}: {
  label: string;
  hint?: string;
  value: number | null;
  step?: number;
  min?: number;
  placeholder?: string;
  onChange: (v: number | null) => void;
}) {
  return (
    <div className="field">
      <label>{label}</label>
      <input
        type="number"
        step={step}
        min={min}
        value={value ?? ""}
        placeholder={placeholder ?? "engine default"}
        onChange={(e) =>
          onChange(e.target.value === "" ? null : Number(e.target.value))
        }
      />
      {hint && <div className="hint">{hint}</div>}
    </div>
  );
}

function Check({
  label,
  hint,
  checked,
  disabled,
  onChange,
}: {
  label: string;
  hint?: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <>
      <div className="checkbox-field">
        <input
          type="checkbox"
          checked={checked}
          disabled={disabled}
          onChange={(e) => onChange(e.target.checked)}
        />
        <label>{label}</label>
      </div>
      {hint && (
        <div className="hint" style={{ marginTop: -6, marginBottom: 10 }}>
          {hint}
        </div>
      )}
    </>
  );
}

export default function SettingsTab() {
  const config = useStore((s) => s.config);
  const patchConfig = useStore((s) => s.patchConfig);
  const caps = useStore((s) => s.caps);
  const setCaps = useStore((s) => s.setCaps);
  const setProbeError = useStore((s) => s.setProbeError);
  const status = useStore((s) => s.status);

  const [command, setCommand] = useState("");
  const [binaryPath, setBinaryPath] = useState("");
  const [copied, setCopied] = useState(false);

  // Keep the preview in step with the form; this is the exact argv that runs.
  useEffect(() => {
    if (!config) return;
    let stale = false;
    api
      .previewCommand(config)
      .then((c) => {
        if (!stale) setCommand(c);
      })
      .catch(() => setCommand(""));
    return () => {
      stale = true;
    };
  }, [config]);

  if (!config) return <div className="empty">Loading defaults…</div>;

  const set = (patch: Partial<ServeConfig>) => patchConfig(patch);
  const running = status.kind === "ready" || status.kind === "starting";

  /** Pick the executable and apply it in one step — a chosen path the user
   *  then has to confirm is a needless second click. */
  const browseBinary = async () => {
    const picked = await pickExecutable(
      "Select the dlm executable",
      binaryPath || caps?.binary,
    );
    if (!picked) return;
    setBinaryPath(picked);
    await applyBinaryPath(picked);
  };

  const applyBinary = () => applyBinaryPath(binaryPath || null);

  const applyBinaryPath = async (path: string | null) => {
    await api.setBinaryOverride(path);
    try {
      const probed = await api.probeEngine();
      setCaps(probed);
      setProbeError(null);
    } catch (e) {
      setCaps(null);
      setProbeError(errorMessage(e));
    }
  };

  return (
    <>
      {running && (
        <div className="field">
          <div className="hint">
            Changes here apply on the next load — dlm reads every one of these
            flags once at startup, so switching any of them restarts the server.
          </div>
        </div>
      )}

      <div className="section">
        <div className="section-title">Precision</div>

        <div className="field">
          <label>Weight quantization (--quant)</label>
          <select
            value={config.quant}
            onChange={(e) => set({ quant: e.target.value as ServeConfig["quant"] })}
          >
            <option value="checkpoint">checkpoint dtype (no --quant)</option>
            <option value="int8">int8 — 2x smaller</option>
            <option value="int4">int4 — 4x smaller</option>
          </select>
          <div className="hint">
            The highest-leverage setting on a small card: quantizing at load
            keeps more layers resident, so streaming shrinks or stops entirely.
          </div>
        </div>

        <div className="field">
          <label>KV cache precision (--kv-quant)</label>
          <select
            value={config.kvQuant}
            onChange={(e) =>
              set({ kvQuant: e.target.value as ServeConfig["kvQuant"] })
            }
          >
            <option value="none">none — exact f32</option>
            <option value="int8">int8 — about half</option>
            <option value="int4">int4 — about a quarter</option>
          </select>
          <div className="hint">
            The KV cache can exceed the weights at long context.
          </div>
        </div>
      </div>

      <div className="section">
        <div className="section-title">Memory</div>

        <OptNumber
          label="VRAM budget (GiB)"
          hint="Overrides the live device query."
          value={config.vramBudgetGb}
          step={0.5}
          min={0}
          onChange={(v) => set({ vramBudgetGb: v })}
        />
        <OptNumber
          label="Safety margin (GiB)"
          hint="Cushion for activation spikes; default 1.5. Lower it on a 4 GB card to fit more layers."
          value={config.safetyMarginGb}
          step={0.1}
          min={0}
          onChange={(v) => set({ safetyMarginGb: v })}
        />
        <OptNumber
          label="Host RAM cache (GiB)"
          hint="Keeps materialized layers across token steps so an evicted layer is not re-read from disk. Off by default."
          value={config.ramCacheGb}
          step={0.5}
          min={0}
          onChange={(v) => set({ ramCacheGb: v })}
        />
        <OptNumber
          label="Expert cache (GiB)"
          hint="MoE models on the streaming GPU path only; ignored for dense models."
          value={config.expertCacheGb}
          step={0.5}
          min={0}
          onChange={(v) => set({ expertCacheGb: v })}
        />

        <div className="field">
          <label>Context length</label>
          <input
            type="number"
            min={1}
            value={config.contextLength}
            onChange={(e) => set({ contextLength: Number(e.target.value) })}
          />
        </div>
      </div>

      <div className="section">
        <div className="section-title">Layer streaming</div>

        <Check
          label="Stream layers (--stream)"
          hint="Hold only a bounded window of layers, streaming the rest — lets a model exceed the resident budget."
          checked={config.stream}
          onChange={(v) => set({ stream: v })}
        />
        <OptNumber
          label="Resident layers"
          hint="Window size. Defaults to the VRAM plan's layers_to_load."
          value={config.residentLayers}
          min={1}
          onChange={(v) => set({ residentLayers: v })}
        />
        <div className="field">
          <label>Prefetch depth</label>
          <input
            type="number"
            min={0}
            disabled={config.autoPrefetch}
            value={config.prefetchDepth}
            onChange={(e) => set({ prefetchDepth: Number(e.target.value) })}
          />
          <div className="hint">
            Layers loaded ahead of compute. 0 disables prefetching.
          </div>
        </div>
        <Check
          label="Auto-tune prefetch depth (--auto-prefetch)"
          hint="Adapts the depth from measured load-vs-compute time instead of a fixed value."
          checked={config.autoPrefetch}
          onChange={(v) => set({ autoPrefetch: v })}
        />
      </div>

      <div className="section">
        <div className="section-title">Compute</div>

        <div className="field">
          <label>Device</label>
          <select
            value={config.device}
            onChange={(e) =>
              set({ device: e.target.value as ServeConfig["device"] })
            }
          >
            <option value="auto">auto (engine decides)</option>
            <option value="gpu">gpu</option>
            <option value="cpu">cpu</option>
          </select>
        </div>

        <div className="field">
          <label>Max batch</label>
          <input
            type="number"
            min={1}
            value={config.maxBatch}
            onChange={(e) => set({ maxBatch: Number(e.target.value) })}
          />
          <div className="hint">
            Each in-flight sequence holds its own KV cache, so on GPU this
            scales KV VRAM.
          </div>
        </div>

        <div className="field">
          <label>Chat template</label>
          <select
            value={config.chatTemplate}
            onChange={(e) =>
              set({ chatTemplate: e.target.value as ServeConfig["chatTemplate"] })
            }
          >
            <option value="plain">plain</option>
            <option value="chatml">chatml</option>
            <option value="llama3">llama3</option>
          </select>
          <div className="hint">
            Match the model's trained format; the wrong one degrades output.
          </div>
        </div>
      </div>

      <div className="section">
        <div className="section-title">Server</div>

        <div className="field-row">
          <div className="field">
            <label>Port</label>
            <input
              type="number"
              min={1}
              max={65535}
              value={config.port}
              onChange={(e) => set({ port: Number(e.target.value) })}
            />
          </div>
          <div className="field">
            <label>Host</label>
            <input
              value={config.host}
              spellCheck={false}
              onChange={(e) => set({ host: e.target.value })}
            />
          </div>
        </div>

        <div className="field">
          <label>API key</label>
          <input
            type="password"
            value={config.apiKey ?? ""}
            placeholder="none (open on localhost)"
            onChange={(e) => set({ apiKey: e.target.value || null })}
          />
        </div>

        <Check
          label="Emit telemetry (--telemetry)"
          hint={
            caps?.telemetry
              ? "Streams per-layer flow events for the flow view."
              : "Unavailable: the installed dlm has no --telemetry flag. The flag is dropped rather than passed, so the server still starts."
          }
          checked={config.telemetry}
          disabled={!caps?.telemetry}
          onChange={(v) => set({ telemetry: v })}
        />
      </div>

      <div className="section">
        <div className="section-title">Engine binary</div>
        <div className="field">
          <div className="field-row">
            <input
              value={binaryPath}
              spellCheck={false}
              placeholder={caps?.binary ?? "path to the dlm executable"}
              onChange={(e) => setBinaryPath(e.target.value)}
            />
            <button
              style={{ flex: "0 0 auto" }}
              title="Choose the dlm executable"
              onClick={() => void browseBinary()}
            >
              Browse…
            </button>
          </div>
          <div className="hint">
            {caps
              ? `Found ${caps.version} at ${caps.binary}`
              : "Not found on PATH or in the installer's default location."}
          </div>
        </div>
        <button onClick={() => void applyBinary()}>Use this binary</button>
      </div>

      <div className="section">
        <div className="section-title">Command</div>
        <div className="cmd-preview">{command || "…"}</div>
        <div className="field-row" style={{ marginTop: 8 }}>
          <button
            onClick={() => {
              void navigator.clipboard.writeText(command);
              setCopied(true);
              setTimeout(() => setCopied(false), 1200);
            }}
          >
            {copied ? "Copied" : "Copy"}
          </button>
        </div>
        <div className="hint" style={{ marginTop: 6 }}>
          Exactly what runs, with the API key redacted. Nothing is passed that
          you cannot see here.
        </div>
      </div>
    </>
  );
}
