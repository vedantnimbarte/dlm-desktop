import { describe, expect, it } from "vitest";
import {
  answerOnly,
  hasThinking,
  isThinking,
  splitThinking,
} from "./thinking";

describe("splitThinking", () => {
  it("returns plain output untouched", () => {
    expect(splitThinking("The capital of France is Paris.")).toEqual([
      { kind: "answer", text: "The capital of France is Paris." },
    ]);
  });

  it("separates a completed thinking block from the answer", () => {
    const out = splitThinking("<think>Let me recall.</think>It is Paris.");
    expect(out).toEqual([
      { kind: "thinking", text: "Let me recall.", open: false },
      { kind: "answer", text: "It is Paris." },
    ]);
  });

  it("keeps text that precedes a thinking block", () => {
    const out = splitThinking("Sure. <think>hmm</think>Paris.");
    expect(out.map((s) => s.kind)).toEqual(["answer", "thinking", "answer"]);
    expect(out[0]).toEqual({ kind: "answer", text: "Sure. " });
  });

  it("handles several blocks in one message", () => {
    const out = splitThinking("<think>a</think>one<think>b</think>two");
    expect(out).toEqual([
      { kind: "thinking", text: "a", open: false },
      { kind: "answer", text: "one" },
      { kind: "thinking", text: "b", open: false },
      { kind: "answer", text: "two" },
    ]);
  });

  it("accepts <thinking> as well as <think>", () => {
    const out = splitThinking("<thinking>reasoning</thinking>answer");
    expect(out[0]).toEqual({ kind: "thinking", text: "reasoning", open: false });
  });

  it("is case-insensitive and tolerates whitespace in tags", () => {
    const out = splitThinking("<THINK >x</Think >y");
    expect(out[0].kind).toBe("thinking");
    expect(out[1]).toEqual({ kind: "answer", text: "y" });
  });

  // --- streaming -----------------------------------------------------------

  it("marks an unterminated block as still open", () => {
    const out = splitThinking("<think>I am partway through");
    expect(out).toEqual([
      { kind: "thinking", text: "I am partway through", open: true },
    ]);
  });

  /** A chat template that pre-fills `<think>` means the model only ever emits
   *  the closing tag, so its output begins inside the block. */
  it("treats a leading close tag as output that began inside a block", () => {
    const out = splitThinking("reasoned about it</think>The answer.");
    expect(out).toEqual([
      { kind: "thinking", text: "reasoned about it", open: false },
      { kind: "answer", text: "The answer." },
    ]);
  });

  it("withholds a partial tag while streaming", () => {
    // "<thi" could still become "<think>"; showing it would flash on screen.
    expect(splitThinking("Paris.<thi", true)).toEqual([
      { kind: "answer", text: "Paris." },
    ]);
    expect(splitThinking("Paris.</thin", true)).toEqual([
      { kind: "answer", text: "Paris." },
    ]);
  });

  it("keeps a less-than that cannot become a tag", () => {
    expect(splitThinking("if a < b then", true)).toEqual([
      { kind: "answer", text: "if a < b then" },
    ]);
  });

  /** Nothing may be silently dropped from a finished message. */
  it("does not withhold a partial tag once the message is complete", () => {
    expect(splitThinking("Paris.<thi", false)).toEqual([
      { kind: "answer", text: "Paris.<thi" },
    ]);
  });

  it("drops whitespace-only fragments between tags", () => {
    const out = splitThinking("<think>x</think>\n\n");
    expect(out).toEqual([{ kind: "thinking", text: "x", open: false }]);
  });

  it("survives an empty message", () => {
    expect(splitThinking("")).toEqual([]);
  });
});

describe("isThinking", () => {
  it("is true only while a block is open", () => {
    expect(isThinking("<think>working")).toBe(true);
    expect(isThinking("<think>done</think>answer")).toBe(false);
    expect(isThinking("plain answer")).toBe(false);
  });
});

describe("hasThinking", () => {
  it("detects reasoning in any position", () => {
    expect(hasThinking("<think>x</think>y")).toBe(true);
    expect(hasThinking("y</think>z")).toBe(true);
    expect(hasThinking("no reasoning here")).toBe(false);
  });
});

describe("answerOnly", () => {
  it("strips reasoning so it is never fed back as history", () => {
    expect(answerOnly("<think>long private reasoning</think>Paris.")).toBe(
      "Paris.",
    );
  });

  it("returns empty while the model is still thinking", () => {
    expect(answerOnly("<think>still going")).toBe("");
  });

  it("joins answer fragments around multiple blocks", () => {
    expect(answerOnly("<think>a</think>one <think>b</think>two")).toBe(
      "one two",
    );
  });

  it("leaves a plain answer alone", () => {
    expect(answerOnly("just an answer")).toBe("just an answer");
  });
});
