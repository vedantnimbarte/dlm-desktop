import { useEffect, useState } from "react";
import { api, errorMessage } from "../api";
import { useStore } from "../store";
import { formatBytes, formatParams } from "../format";
import { pickDirectory, pickExecutable, pickFile } from "../pick";
import FitTable from "./FitTable";
import type { DiscoveredServer } from "../types";

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
  const setCaps = useStore((s) => s.setCaps);
  const setProbeError = useStore((s) => s.setProbeError);
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
  const [discovered, setDiscovered] = useState<DiscoveredServer[]>([]);

  // Look for an already-running engine whenever we are not connected. This is
  // the answer to "no binary found" far more often than installing one is, and
  // it costs a single request to a well-known port.
  useEffect(() => {
    if (status.kind === "ready") {
      setDiscovered([]);
      return;
    }
    let stale = false;
    api
      .discoverServers()
      .then((found) => {
        if (!stale) setDiscovered(found);
      })
      .catch(() => setDiscovered([]));
    return () => {
      stale = true;
    };
  }, [status.kind]);

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

  const attachTo = async (address: string, key?: string | null) => {
    setError(null);
    setConnecting(true);
    try {
      setStatus(await api.attachRemote(address, key ?? null));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setConnecting(false);
    }
  };

  const attach = () => attachTo(remoteAddress, remoteKey || null);

  /** Locate the engine binary and probe it, from the Local pane. */
  const browseBinary = async () => {
    const picked = await pickExecutable("Select the dlm executable");
    if (!picked) return;
    await api.setBinaryOverride(picked);
    try {
      setCaps(await api.probeEngine());
      setProbeError(null);
    } catch (e) {
      setCaps(null);
      setProbeError(errorMessage(e));
    }
  };

  const disconnect = async () => {
    await api.disconnect();
    setStatus({ kind: "disconnected" });
  };

  /** Pick a folder of checkpoints and scan it immediately — a picked folder
   *  the user then has to press "Scan" on is a needless second step. */
  const browseRoot = async () => {
    const dir = await pickDirectory(
      "Select a folder containing models",
      rootDraft || modelsRoot,
    );
    if (!dir) return;
    setRootDraft(dir);
    await scanModels(dir);
  };

  /** Pick a single model folder directly, for a checkpoint that lives outside
   *  the scanned directory. */
  const browseModel = async () => {
    const dir = await pickDirectory(
      "Select a model folder",
      config?.modelPath || modelsRoot,
    );
    if (!dir) return;
    patchConfig({ modelPath: dir });
    // Show the fit preview for it too, so "will this run?" is answered without
    // the model having to be in the scanned list.
    selectModel(dir);
  };

  /** A GGUF model is one self-contained file, not a folder. */
  const browseGguf = async () => {
    const file = await pickFile("Select a .gguf model", "GGUF model", [
      "gguf",
    ]);
    if (!file) return;
    patchConfig({ modelPath: file });
    selectModel(file);
  };

  return (
    <>
      <div className="section">
        <div className="section-title">Connection</div>
        <div className="segmented">
          <button
            className={mode === "local" ? "active" : ""}
            onClick={() => setMode("local")}
          >
            Local
          </button>
          <button
            className={mode === "remote" ? "active" : ""}
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
                  {probeError ?? "dlm binary not found."}
                </div>
                <div className="field-row" style={{ marginTop: 6 }}>
                  <button onClick={() => void browseBinary()}>
                    Locate dlm…
                  </button>
                </div>
                <div className="hint">
                  Or set <code>$DLM_BIN</code>, or use Remote to attach to a
                  server running elsewhere.
                </div>
              </div>
            )}

            {/* A server the user already started is the answer to "no binary
                found" far more often than installing one is. */}
            {discovered.length > 0 && (
              <div className="field">
                {discovered.map((s) => (
                  <div key={s.address} className="discovered">
                    <div>
                      <div className="discovered-title">
                        dlm already running on {s.address}
                      </div>
                      <div className="hint dim">serving {s.modelId}</div>
                    </div>
                    <button
                      className="primary"
                      disabled={busy}
                      onClick={() => void attachTo(s.address)}
                    >
                      Attach
                    </button>
                  </div>
                ))}
              </div>
            )}

            <div className="field">
              <label>Models directory</label>
              <div className="field-row">
                <input
                  value={rootDraft || modelsRoot || ""}
                  placeholder="folder containing your checkpoints"
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
                  title="Choose a folder"
                  onClick={() => void browseRoot()}
                >
                  Browse…
                </button>
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
                      {m.ggufType && <span className="tag">{m.ggufType}</span>}
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
              <div className="field-row">
                <input
                  value={config?.modelPath ?? ""}
                  placeholder="pick one above, or browse"
                  spellCheck={false}
                  onChange={(e) => patchConfig({ modelPath: e.target.value })}
                />
                <button
                  style={{ flex: "0 0 auto" }}
                  title="Choose a model folder"
                  onClick={() => void browseModel()}
                >
                  Folder…
                </button>
                <button
                  style={{ flex: "0 0 auto" }}
                  title="Choose a .gguf file"
                  onClick={() => void browseGguf()}
                >
                  GGUF…
                </button>
              </div>
              <div className="hint">
                A folder with <code>config.json</code> and{" "}
                <code>*.safetensors</code>, or a single <code>.gguf</code> file.
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

/** Blank input means "omit the field", not zero. */
function optNum(raw: string): number | undefined {
  return raw === "" ? undefined : Number(raw);
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
          value={sampling.max_tokens ?? ""}
          placeholder="no limit"
          onChange={(e) => setSampling({ max_tokens: optNum(e.target.value) })}
        />
        <div className="hint">
          Blank runs to the end of the answer, capped by the context window.
        </div>
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
            onChange={(e) => setSampling({ top_p: optNum(e.target.value) })}
          />
        </div>
        <div className="field">
          <label>top_k</label>
          <input
            type="number"
            min={0}
            value={sampling.top_k ?? ""}
            placeholder="default"
            onChange={(e) => setSampling({ top_k: optNum(e.target.value) })}
          />
        </div>
      </div>

      <div className="field-row">
        <div className="field">
          <label>frequency_penalty</label>
          <input
            type="number"
            min={-2}
            max={2}
            step={0.1}
            value={sampling.frequency_penalty ?? ""}
            placeholder="0"
            onChange={(e) =>
              setSampling({ frequency_penalty: optNum(e.target.value) })
            }
          />
        </div>
        <div className="field">
          <label>presence_penalty</label>
          <input
            type="number"
            min={-2}
            max={2}
            step={0.1}
            value={sampling.presence_penalty ?? ""}
            placeholder="0"
            onChange={(e) =>
              setSampling({ presence_penalty: optNum(e.target.value) })
            }
          />
        </div>
      </div>

      <div className="field">
        <label>DRY multiplier</label>
        <input
          type="number"
          min={0}
          step={0.1}
          value={sampling.dry_multiplier ?? ""}
          placeholder="off"
          onChange={(e) =>
            setSampling({ dry_multiplier: optNum(e.target.value) })
          }
        />
        <div className="hint">
          Penalizes verbatim loops a small model falls into; 0.8 is a common
          starting point.
        </div>
      </div>

      <div className="checkbox-field">
        <input
          type="checkbox"
          checked={sampling.response_format?.type === "json_object"}
          onChange={(e) =>
            setSampling({
              response_format: e.target.checked
                ? { type: "json_object" }
                : undefined,
            })
          }
        />
        <label>JSON output (response_format: json_object)</label>
      </div>

      <div className="hint">
        Blank fields are omitted from the request, leaving the server's own
        defaults in place.
      </div>
    </div>
  );
}
