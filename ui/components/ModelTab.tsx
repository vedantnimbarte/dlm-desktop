import { useState } from "react";
import { api, errorMessage } from "../api";
import { useStore } from "../store";
import { formatBytes, formatParams } from "../format";
import FitTable from "./FitTable";

/** Browse local checkpoints, see whether they fit, and load one.
 *
 * dlm binds one model per process, so "switch model" is "restart the server".
 * That is stated in the UI rather than hidden behind a spinner — the restart is
 * the reason a switch costs a full model load. */
export default function ModelTab() {
  const status = useStore((s) => s.status);
  const config = useStore((s) => s.config);
  const patchConfig = useStore((s) => s.patchConfig);
  const connecting = useStore((s) => s.connecting);
  const setConnecting = useStore((s) => s.setConnecting);
  const setStatus = useStore((s) => s.setStatus);
  const caps = useStore((s) => s.caps);
  const probeError = useStore((s) => s.probeError);
  const setRightPanel = useStore((s) => s.setRightPanel);

  const modelsRoot = useStore((s) => s.modelsRoot);
  const localModels = useStore((s) => s.localModels);
  const modelsScanning = useStore((s) => s.modelsScanning);
  const modelsError = useStore((s) => s.modelsError);
  const selectedModelPath = useStore((s) => s.selectedModelPath);
  const selectModel = useStore((s) => s.selectModel);
  const scanModels = useStore((s) => s.scanModels);
  const fit = useStore((s) => s.fit);
  const fitLoading = useStore((s) => s.fitLoading);
  const fitError = useStore((s) => s.fitError);

  const [mode, setMode] = useState<"local" | "remote">("local");
  const [rootDraft, setRootDraft] = useState("");
  const [remoteAddress, setRemoteAddress] = useState("127.0.0.1:8000");
  const [remoteKey, setRemoteKey] = useState("");
  const [error, setError] = useState<string | null>(null);

  const busy = connecting || status.kind === "starting";

  const startLocal = async () => {
    if (!config?.modelPath) return;
    setError(null);
    setConnecting(true);
    try {
      setStatus(await api.startLocal(config));
    } catch (e) {
      setError(errorMessage(e));
      // The engine's own output explains a failed load far better than any
      // message this app can compose, so send the user straight to it.
      setRightPanel("logs");
    } finally {
      setConnecting(false);
    }
  };

  const attach = async () => {
    setError(null);
    setConnecting(true);
    try {
      setStatus(await api.attachRemote(remoteAddress, remoteKey || null));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setConnecting(false);
    }
  };

  const disconnect = async () => {
    await api.disconnect();
    setStatus({ kind: "disconnected" });
  };

  return (
    <>
      <div className="section">
        <div className="section-title">Connection</div>
        <div className="field-row" style={{ marginBottom: 12 }}>
          <button
            className={mode === "local" ? "primary" : ""}
            onClick={() => setMode("local")}
          >
            Local
          </button>
          <button
            className={mode === "remote" ? "primary" : ""}
            onClick={() => setMode("remote")}
          >
            Remote
          </button>
        </div>

        {mode === "remote" ? (
          <>
            <div className="field">
              <label>Server address</label>
              <input
                value={remoteAddress}
                spellCheck={false}
                onChange={(e) => setRemoteAddress(e.target.value)}
              />
              <div className="hint">
                A running <code>dlm serve</code>, or a distributed coordinator.
              </div>
            </div>
            <div className="field">
              <label>API key (optional)</label>
              <input
                type="password"
                value={remoteKey}
                onChange={(e) => setRemoteKey(e.target.value)}
              />
            </div>
            <div className="field-row">
              <button
                className="primary"
                disabled={busy}
                onClick={() => void attach()}
              >
                {busy ? "Connecting…" : "Attach"}
              </button>
              <button
                disabled={status.kind === "disconnected"}
                onClick={() => void disconnect()}
              >
                Detach
              </button>
            </div>
          </>
        ) : (
          <>
            {!caps && (
              <div className="field">
                <div className="hint error-text">
                  {probeError ?? "dlm binary not found."} Set its path in
                  Settings, or use Remote to attach to a server elsewhere.
                </div>
              </div>
            )}

            <div className="field">
              <label>Models directory</label>
              <div className="field-row">
                <input
                  value={rootDraft || modelsRoot || ""}
                  placeholder="path to a folder of checkpoints"
                  spellCheck={false}
                  onChange={(e) => setRootDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && rootDraft)
                      void scanModels(rootDraft);
                  }}
                />
                <button
                  style={{ flex: "0 0 auto" }}
                  disabled={modelsScanning}
                  onClick={() =>
                    void scanModels(rootDraft || modelsRoot || "")
                  }
                >
                  {modelsScanning ? "…" : "Scan"}
                </button>
              </div>
              {modelsError && (
                <div className="hint error-text">{modelsError}</div>
              )}
            </div>

            <div className="model-list">
              {localModels.length === 0 ? (
                <div className="empty">
                  {modelsScanning ? "Scanning…" : "No checkpoints found here."}
                </div>
              ) : (
                localModels.map((m) => (
                  <div
                    key={m.path}
                    className={`model-row${
                      m.path === selectedModelPath ? " active" : ""
                    }`}
                    onClick={() => {
                      selectModel(m.path);
                      patchConfig({ modelPath: m.path });
                    }}
                  >
                    <div className="model-name">
                      {m.name}
                      {m.isMoe && <span className="tag">MoE</span>}
                      {m.error && (
                        <span className="tag tag-error" title={m.error}>
                          unsupported
                        </span>
                      )}
                      {!m.hasTokenizer && (
                        <span
                          className="tag tag-warn"
                          title="No tokenizer.json or vocab.json+merges.txt"
                        >
                          no tokenizer
                        </span>
                      )}
                    </div>
                    <div className="model-meta dim mono">
                      {m.numLayers} layers · {formatParams(m.estimatedParams)} ·{" "}
                      {formatBytes(m.checkpointBytes)}
                    </div>
                  </div>
                ))
              )}
            </div>

            {fitLoading && <div className="hint dim">Planning…</div>}
            {fitError && <div className="hint error-text">{fitError}</div>}
            {fit && !fitLoading && <FitTable fit={fit} />}

            <div className="field" style={{ marginTop: 12 }}>
              <label>Model directory</label>
              <input
                value={config?.modelPath ?? ""}
                placeholder="select one above, or type a path"
                spellCheck={false}
                onChange={(e) => patchConfig({ modelPath: e.target.value })}
              />
            </div>

            <div className="field-row">
              <button
                className="primary"
                disabled={busy || !caps || !config?.modelPath}
                onClick={() => void startLocal()}
              >
                {busy ? "Loading…" : "Load model"}
              </button>
              <button
                disabled={status.kind === "disconnected"}
                onClick={() => void disconnect()}
              >
                Stop
              </button>
            </div>

            {status.kind === "ready" && status.local && (
              <div className="hint dim">
                Loading a different model restarts the server — dlm serves one
                model per process.
              </div>
            )}
          </>
        )}

        {error && (
          <div className="field">
            <div className="hint error-text" style={{ whiteSpace: "pre-wrap" }}>
              {error}
            </div>
          </div>
        )}
        {status.kind === "failed" && !error && (
          <div className="field">
            <div className="hint error-text" style={{ whiteSpace: "pre-wrap" }}>
              {status.detail}
            </div>
          </div>
        )}
      </div>

      <SamplingSection />
    </>
  );
}

function SamplingSection() {
  const sampling = useStore((s) => s.sampling);
  const setSampling = useStore((s) => s.setSampling);

  return (
    <div className="section">
      <div className="section-title">Sampling</div>

      <div className="field">
        <label>Temperature — {sampling.temperature ?? 0}</label>
        <input
          type="range"
          min={0}
          max={2}
          step={0.05}
          value={sampling.temperature ?? 0.7}
          onChange={(e) => setSampling({ temperature: Number(e.target.value) })}
        />
        <div className="hint">0 is deterministic greedy decoding.</div>
      </div>

      <div className="field">
        <label>Max tokens</label>
        <input
          type="number"
          min={1}
          value={sampling.max_tokens ?? 512}
          onChange={(e) => setSampling({ max_tokens: Number(e.target.value) })}
        />
      </div>

      <div className="field-row">
        <div className="field">
          <label>top_p</label>
          <input
            type="number"
            min={0}
            max={1}
            step={0.05}
            value={sampling.top_p ?? ""}
            placeholder="default"
            onChange={(e) =>
              setSampling({
                top_p:
                  e.target.value === "" ? undefined : Number(e.target.value),
              })
            }
          />
        </div>
        <div className="field">
          <label>top_k</label>
          <input
            type="number"
            min={0}
            value={sampling.top_k ?? ""}
            placeholder="default"
            onChange={(e) =>
              setSampling({
                top_k:
                  e.target.value === "" ? undefined : Number(e.target.value),
              })
            }
          />
        </div>
      </div>
      <div className="hint">
        Blank fields are omitted from the request, leaving the server's own
        defaults in place.
      </div>
    </div>
  );
}
