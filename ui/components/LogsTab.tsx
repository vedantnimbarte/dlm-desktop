import { useEffect, useRef } from "react";
import { useStore } from "../store";

/** Raw engine output.
 *
 * This is where dlm's own words land — the VRAM plan it printed, the device it
 * fell back to, the error it died with. It is deliberately unfiltered: the
 * moment this app starts summarizing the engine's output, it starts hiding the
 * thing the user needs when something goes wrong. */
export default function LogsTab() {
  const logs = useStore((s) => s.logs);
  const endRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);

  useEffect(() => {
    if (pinned.current) endRef.current?.scrollIntoView({ block: "end" });
  }, [logs]);

  return (
    <div
      ref={containerRef}
      onScroll={(e) => {
        const el = e.currentTarget;
        pinned.current =
          el.scrollHeight - el.scrollTop - el.clientHeight < 40;
      }}
    >
      {logs.length === 0 ? (
        <div className="empty">
          No engine output yet. Start a local server to see dlm's own logs here.
        </div>
      ) : (
        <div className="log-view">
          {logs.map((l, i) => (
            <div key={i} className={`log-line log-${l.stream}`}>
              <span className="log-t">{(l.tMs / 1000).toFixed(1)}s</span>
              <span className="log-text">{l.text}</span>
            </div>
          ))}
          <div ref={endRef} />
        </div>
      )}
    </div>
  );
}
