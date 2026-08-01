import { useState } from "react";
import { api } from "../api";
import { useStore } from "../store";
import { formatMs, formatTime } from "../format";

/** Every API call the app made, with its timings.
 *
 * Doubles as a debugging tool for people driving dlm from their own code: the
 * request body shown here is the exact JSON that went over the wire, so a
 * behaviour seen in the chat pane can be reproduced with curl. */
export default function ApiTab() {
  const entries = useStore((s) => s.apiLog);
  const setApiLog = useStore((s) => s.setApiLog);
  const [expanded, setExpanded] = useState<number | null>(null);

  const clear = async () => {
    await api.clearApiLog().catch(() => {});
    setApiLog([]);
  };

  return (
    <>
      <div className="section">
        <div className="section-title">
          API calls
          <button
            className="ghost"
            style={{ float: "right", padding: "0 6px" }}
            disabled={entries.length === 0}
            onClick={() => void clear()}
          >
            Clear
          </button>
        </div>

        {entries.length === 0 ? (
          <div className="empty">
            No requests yet. Every chat completion is recorded here with its
            latency, time-to-first-token and token counts.
          </div>
        ) : (
          entries.map((e) => {
            const failed = e.error != null || (e.status != null && e.status >= 400);
            return (
              <div
                key={e.id || e.ts}
                className={`api-row${failed ? " failed" : ""}`}
                onClick={() => setExpanded(expanded === e.id ? null : e.id)}
              >
                <div className="api-head">
                  <span className="mono">{e.method}</span>
                  <span className="api-path mono">{e.path}</span>
                  <span
                    className={`api-status mono ${failed ? "error-text" : "ok-text"}`}
                  >
                    {e.error ? "err" : (e.status ?? "—")}
                  </span>
                  <span className="dim mono">{formatTime(e.ts)}</span>
                </div>

                <div className="api-metrics mono dim">
                  <span title="Total wall-clock for the request">
                    {formatMs(e.latencyMs)}
                  </span>
                  {e.ttftMs != null && (
                    <span title="Time to first visible token">
                      ttft {formatMs(e.ttftMs)}
                    </span>
                  )}
                  {e.promptTokens != null && <span>in {e.promptTokens}</span>}
                  {e.completionTokens != null && (
                    <span>out {e.completionTokens}</span>
                  )}
                </div>

                {e.error && <div className="api-error">{e.error}</div>}

                {expanded === e.id && (
                  <div className="api-detail">
                    {e.requestPreview && (
                      <>
                        <div className="api-detail-label">request</div>
                        <pre>{e.requestPreview}</pre>
                      </>
                    )}
                    {e.responsePreview && (
                      <>
                        <div className="api-detail-label">response</div>
                        <pre>{e.responsePreview}</pre>
                      </>
                    )}
                  </div>
                )}
              </div>
            );
          })
        )}
      </div>
    </>
  );
}
