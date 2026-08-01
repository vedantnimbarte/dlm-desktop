/** Splitting reasoning-model output into thinking and answer.
 *
 * Models like Qwen3 and DeepSeek-R1 emit their chain of thought inline, wrapped
 * in `<think>` tags. Rendered raw it swamps the answer; hidden entirely it
 * throws away the most interesting thing the model produced. Both need to be
 * separable, which means parsing the stream.
 *
 * Three cases make this less trivial than a regex:
 *
 * 1. **The block is still open.** Mid-stream there is no closing tag yet, and
 *    everything after the opener is thinking-so-far — not an unterminated tag
 *    to render literally.
 * 2. **A closing tag with no opener.** Several chat templates pre-fill
 *    `<think>` into the prompt, so the model only ever emits `</think>`. Its
 *    output then *starts* inside a thinking block.
 * 3. **A tag split across tokens.** `<th` arriving alone must not flash on
 *    screen as text before `ink>` completes it.
 *
 * The message stored in SQLite is always the raw text the model produced; this
 * runs at render time, so toggling visibility never rewrites history. */

export type Segment =
  | { kind: "answer"; text: string }
  /** `open` means the closing tag has not arrived — the model is still thinking. */
  | { kind: "thinking"; text: string; open: boolean };

/** Tag names treated as reasoning blocks, longest first so `<thinking>` is not
 *  mistaken for `<think>` followed by stray text. */
const TAGS = ["thinking", "think"] as const;

const OPEN_RE = new RegExp(`<(${TAGS.join("|")})\\s*>`, "i");
const CLOSE_RE = new RegExp(`</(${TAGS.join("|")})\\s*>`, "i");

/**
 * Length of a trailing run that could still become a tag.
 *
 * While streaming, a partial `<thi` must be withheld rather than rendered: it
 * would appear as literal text for one frame and then vanish, which reads as a
 * glitch.
 */
function danglingPrefixLength(text: string): number {
  const start = text.lastIndexOf("<");
  if (start === -1) return 0;
  const tail = text.slice(start);
  // A complete tag is not dangling; it was already consumed by the matchers.
  if (tail.includes(">")) return 0;
  const lower = tail.toLowerCase();
  const couldBecomeTag = TAGS.some(
    (t) =>
      `<${t}>`.startsWith(lower) ||
      `</${t}>`.startsWith(lower) ||
      lower === "<" ||
      lower === "</",
  );
  return couldBecomeTag ? tail.length : 0;
}

function push(out: Segment[], seg: Segment) {
  // Whitespace-only fragments between tags are framing, not content.
  if (seg.text.trim().length === 0) return;
  out.push(seg);
}

/**
 * Split `content` into ordered thinking and answer segments.
 *
 * `streaming` withholds a trailing partial tag; pass false for a finished
 * message so nothing is ever silently dropped from stored text.
 */
export function splitThinking(content: string, streaming = false): Segment[] {
  const out: Segment[] = [];
  let rest = content;

  // A closing tag before any opener means the output began inside a thinking
  // block — the template supplied the opener, so the model never emitted one.
  const firstClose = rest.search(CLOSE_RE);
  const firstOpen = rest.search(OPEN_RE);
  if (firstClose !== -1 && (firstOpen === -1 || firstClose < firstOpen)) {
    const close = rest.match(CLOSE_RE)!;
    push(out, {
      kind: "thinking",
      text: rest.slice(0, firstClose),
      open: false,
    });
    rest = rest.slice(firstClose + close[0].length);
  }

  for (;;) {
    const openAt = rest.search(OPEN_RE);
    if (openAt === -1) break;
    const open = rest.match(OPEN_RE)!;

    push(out, { kind: "answer", text: rest.slice(0, openAt) });
    rest = rest.slice(openAt + open[0].length);

    const closeAt = rest.search(CLOSE_RE);
    if (closeAt === -1) {
      // Still thinking: everything from here is the block so far.
      push(out, { kind: "thinking", text: rest, open: true });
      return out;
    }
    const close = rest.match(CLOSE_RE)!;
    push(out, { kind: "thinking", text: rest.slice(0, closeAt), open: false });
    rest = rest.slice(closeAt + close[0].length);
  }

  if (streaming) {
    const dangling = danglingPrefixLength(rest);
    if (dangling > 0) rest = rest.slice(0, rest.length - dangling);
  }
  push(out, { kind: "answer", text: rest });
  return out;
}

/** Is the model currently inside a thinking block? */
export function isThinking(content: string): boolean {
  const segments = splitThinking(content, true);
  const last = segments[segments.length - 1];
  return last?.kind === "thinking" && last.open;
}

/** True if `content` contains any reasoning block at all. */
export function hasThinking(content: string): boolean {
  return splitThinking(content).some((s) => s.kind === "thinking");
}

/** The answer with all reasoning removed — what gets copied, and what is
 *  replayed as conversation history so the model is not fed its own
 *  scratchpad back. */
export function answerOnly(content: string): string {
  return splitThinking(content)
    .filter((s) => s.kind === "answer")
    .map((s) => s.text)
    .join("")
    .trim();
}
