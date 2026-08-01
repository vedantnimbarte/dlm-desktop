import type { FitPreview } from "../types";
import { formatBytes } from "../format";

/** "Will it fit, and how much of it will stream?"
 *
 * The most useful thing this app can tell someone with a small card, and the
 * reason the model manager exists at all. Every number here comes from dlm's
 * own `VramProfiler` against the real checkpoint — not an estimate this UI
 * invented — so it agrees with what `dlm profile` would print. */
export default function FitTable({ fit }: { fit: FitPreview }) {
  const { vram, rows, model } = fit;
  /** Rows that actually produced a plan. */
  const planned = rows.filter((r) => r.available);

  return (
    <div className="fit">
      <div className="fit-basis">
        {vram.kind === "device" && (
          <>
            Planning against <strong>{formatBytes(vram.free)}</strong> free of{" "}
            {formatBytes(vram.total)}, reported by the {vram.via}.
          </>
        )}
        {vram.kind === "manual" && (
          <>
            Planning against a manual budget of{" "}
            <strong>{formatBytes(vram.bytes)}</strong>.
          </>
        )}
        {vram.kind === "unavailable" && (
          <span className="error-text">
            No VRAM figure available ({vram.reason}). Set a VRAM budget in
            Settings to see a plan.
          </span>
        )}
      </div>

      <table className="fit-table">
        <thead>
          <tr>
            <th>quant</th>
            <th>resident</th>
            <th>per layer</th>
            <th>passes</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.quant} className={row.available ? "" : "unavailable"}>
              <td>
                {row.quant}
                {row.quant === "checkpoint" && (
                  <span className="dim"> ({fit.nativeQuant})</span>
                )}
              </td>
              {row.available ? (
                <>
                  <td>
                    <div className="fit-bar" title={`${row.layersToLoad} of ${row.numLayers} layers`}>
                      <div
                        className={`fit-bar-fill${row.fitsEntirely ? " full" : ""}`}
                        style={{ width: `${row.residentFraction * 100}%` }}
                      />
                    </div>
                    <span className="mono">
                      {row.layersToLoad}/{row.numLayers}
                    </span>
                  </td>
                  <td className="mono">
                    {formatBytes(row.perLayerWeightBytes)}
                  </td>
                  <td className="mono">
                    {row.fitsEntirely ? (
                      <span className="ok-text">none</span>
                    ) : (
                      `${row.streamPasses}×`
                    )}
                  </td>
                </>
              ) : (
                <td colSpan={3} className="dim">
                  {row.unavailableReason}
                </td>
              )}
            </tr>
          ))}
        </tbody>
      </table>

      <div className="fit-notes">
        {/* Say nothing when nothing was planned. Claiming "every precision
            streams at this budget" with no budget and no rows would be exactly
            the kind of confident fabrication this panel exists to avoid. */}
        {planned.length > 0 &&
          (planned.some((r) => r.fitsEntirely) ? (
            <div className="note ok-text">
              At least one precision holds the whole model — no streaming, which
              is several times faster.
            </div>
          ) : (
            <div className="note warn-text">
              Every precision streams at this budget. Streaming cost is
              bandwidth: the fewer layers stay resident, the more of the model
              crosses the bus each token.
            </div>
          ))}

        {model.isMoe && (
          <div className="note">
            Mixture-of-experts: these figures cover the resident layer core
            only. Routed experts stream into their own budget
            (<code>--expert-cache-gb</code>), so real VRAM use is higher.
          </div>
        )}

        {!model.hasTokenizer && (
          <div className="note warn-text">
            No tokenizer found in this directory. dlm falls back to a raw byte
            tokenizer, which produces gibberish on an instruct model.
          </div>
        )}

        {model.error && <div className="note error-text">{model.error}</div>}

        <div className="note dim">
          Planned for {fit.contextLength} tokens of context at batch{" "}
          {fit.maxBatch}. KV cache scales with both.
        </div>
      </div>
    </div>
  );
}
