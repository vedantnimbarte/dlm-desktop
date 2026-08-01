import { useStore } from "../store";
import { splitThinking } from "../thinking";
import type { UiMessage } from "../types";
import ThinkingBlock from "./ThinkingBlock";

/** Split on ``` fences so code arrives as real blocks.
 *
 * This is not a markdown renderer and does not pretend to be one — fenced code
 * is the formatting that actually matters when reading model output, and doing
 * only that keeps a dependency out of the tree. An unterminated fence (common
 * mid-stream) renders as an open code block rather than swallowing the text. */
function segments(content: string): Array<{ code: boolean; text: string }> {
  const parts = content.split("```");
  return parts.map((text, i) => ({ code: i % 2 === 1, text }));
}

function stripLanguageTag(code: string): string {
  const nl = code.indexOf("\n");
  if (nl === -1) return code;
  const first = code.slice(0, nl).trim();
  // A lone word on the opening line is a language tag, not content.
  return /^[a-zA-Z0-9_+-]{1,20}$/.test(first) ? code.slice(nl + 1) : code;
}

function Prose({ text }: { text: string }) {
  return (
    <>
      {segments(text).map((seg, i) =>
        seg.code ? (
          <pre key={i}>
            <code>{stripLanguageTag(seg.text)}</code>
          </pre>
        ) : (
          <span key={i}>{seg.text}</span>
        ),
      )}
    </>
  );
}

export default function Message({ message }: { message: UiMessage }) {
  const { role, content, streaming, error, stats } = message;
  const showThinking = useStore((s) => s.showThinking);
  const expandThinking = useStore((s) => s.expandThinking);

  // A user turn is never a reasoning stream; parsing it would only risk
  // mangling a message that legitimately mentions <think>.
  const parts =
    role === "assistant"
      ? splitThinking(content, streaming)
      : [{ kind: "answer" as const, text: content }];

  const visible = showThinking
    ? parts
    : parts.filter((p) => p.kind !== "thinking");

  // While a hidden reasoning block is the only thing produced so far, the
  // message would otherwise render as an empty bubble — which is exactly how
  // this model looked before reasoning was handled at all.
  const nothingVisibleYet =
    streaming && visible.every((p) => p.text.trim().length === 0);

  return (
    <div className={`msg ${role}`}>
      <div className="msg-role">{role === "user" ? "You" : "AI"}</div>
      <div className="msg-body">
        <div className="msg-content">
          {visible.map((part, i) =>
            part.kind === "thinking" ? (
              <ThinkingBlock
                key={i}
                segment={part}
                thinkingMs={stats?.thinkingMs}
                defaultOpen={expandThinking}
              />
            ) : (
              <Prose key={i} text={part.text} />
            ),
          )}
          {nothingVisibleYet && (
            <span className="dim">
              {parts.some((p) => p.kind === "thinking")
                ? "Thinking… (reasoning hidden)"
                : ""}
            </span>
          )}
          {streaming && <span className="caret" />}
        </div>

        {error && <div className="msg-error">{error}</div>}

        {stats && !streaming && (
          <div className="msg-stats">
            {stats.tokensPerSec != null && (
              <span>{stats.tokensPerSec.toFixed(1)} tok/s</span>
            )}
            {stats.ttftMs != null && <span>{stats.ttftMs} ms to first token</span>}
            {stats.completionTokens != null && (
              <span>{stats.completionTokens} tokens</span>
            )}
            {stats.thinkingMs != null && (
              <span>{(stats.thinkingMs / 1000).toFixed(1)}s thinking</span>
            )}
            {stats.reason === "length" && <span>hit max_tokens</span>}
            {stats.reason === "cancelled" && <span>stopped</span>}
          </div>
        )}
      </div>
    </div>
  );
}
