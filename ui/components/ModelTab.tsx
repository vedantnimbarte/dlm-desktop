import { useState } from "react";
import { api, errorMessage } from "../api";
import { useStore } from "../store";

/** Load a model (spawn a local server) or attach to one already running.
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
  const sampling = useStore((s) => s.sampling);
  const setSampling = useStore((s) => s.setSampling);
  const setRightPanel = useStore((s) => s.setRightPanel);

  const [mode, setMode] = useState<"local" | "remote">("local");
  const [remoteAddress, setRemoteAddress] = useState("127.0.0.1:8000");
  const [remoteKey, setRemoteKey] = useState("");
  const [error, setError] = useState<string | null>(null);

  const busy = connecting || status.kind === "starting";

  const startLocal = async () => {
    if (!config) return;
    setError(null);
    setConnecting(true);
    try {
      const next = await api.startLocal(config);
      setStatus(next);
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
      const next = await api.attachRemote(remoteAddress, remoteKey || null);
      setStatus(next);
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

        {mode === "local" ? (
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
              <label>Model directory</label>
              <input
                value={config?.modelPath ?? ""}
                placeholder="models/Qwen3-0.6B"
                spellCheck={false}
                onChange={(e) => patchConfig({ modelPath: e.target.value })}
              />
              <div className="hint">
                A directory with <code>config.json</code> and{" "}
                <code>*.safetensors</code>.
              </div>
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
          </>
        ) : (
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
              <div className="hint">
                Required if that server was started with <code>--api-key</code>.
              </div>
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
            onChange={(e) =>
              setSampling({ temperature: Number(e.target.value) })
            }
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
                  top_p: e.target.value === "" ? undefined : Number(e.target.value),
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
                  top_k: e.target.value === "" ? undefined : Number(e.target.value),
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
    </>
  );
}
