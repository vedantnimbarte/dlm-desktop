import { useEffect, useState } from "react";
import type { Segment } from "../thinking";

/** A model's chain of thought, shown separately from its answer.
 *
 * Expanded while the model is still thinking — watching it work is the point,
 * and on a streamed 4 GB-card run it is often the only thing happening for
 * several seconds — then collapsed once the answer begins, so the reasoning
 * does not bury it. */
export default function ThinkingBlock({
  segment,
  thinkingMs,
  defaultOpen,
}: {
  segment: Extract<Segment, { kind: "thinking" }>;
  /** How long the block took, once closed. */
  thinkingMs?: number | null;
  defaultOpen: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen || segment.open);

  // Collapse automatically when the model stops thinking, unless the user has
  // asked for reasoning to stay open. Their explicit toggle is not overridden:
  // this only fires on the streaming-to-done transition.
  useEffect(() => {
    if (!segment.open && !defaultOpen) setOpen(false);
    if (segment.open) setOpen(true);
  }, [segment.open, defaultOpen]);

  const label = segment.open
    ? "Thinking…"
    : thinkingMs != null
      ? `Thought for ${(thinkingMs / 1000).toFixed(1)}s`
      : "Thought about it";

  return (
    <div className={`thinking${segment.open ? " active" : ""}`}>
      <button
        className="thinking-head"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
      >
        <span className={`thinking-caret${open ? " open" : ""}`}>▸</span>
        <span className="thinking-label">{label}</span>
        {segment.open && <span className="thinking-pulse" />}
      </button>
      {open && <div className="thinking-body">{segment.text.trim()}</div>}
    </div>
  );
}
