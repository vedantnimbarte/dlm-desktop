/** Drives one streaming chat turn.
 *
 * Event listeners are registered **once** for the app, not once per turn, and
 * deltas are routed by request id through `inFlight`. Registering per turn is
 * the obvious approach and the wrong one: React strict-mode double-invocation
 * and any overlapping turn both end up double-appending every token.
 *
 * Persistence follows the same rule as the store: the message row is written
 * when the turn starts and rewritten once when it ends, never per token. */

import { useCallback, useEffect, useRef } from "react";
import { api, errorMessage, listenChat } from "./api";
import { useStore, nextId } from "./store";
import { answerOnly, isThinking } from "./thinking";
import type { ChatMessage, UiMessage } from "./types";

/** request id -> where its tokens belong, plus reasoning timing.
 *
 * `startedAt` is set when the turn begins and `thinkingMs` when the reasoning
 * block closes, so "Thought for 4.2s" is measured rather than inferred from the
 * total. */
const inFlight = new Map<
  number,
  {
    conversationId: string;
    messageId: string;
    startedAt: number;
    thinkingMs?: number;
    wasThinking: boolean;
  }
>();

/** Persist a finished assistant turn, body and measurements together. */
function persistFinished(conversationId: string, messageId: string) {
  const conversation = useStore
    .getState()
    .conversations.find((c) => c.id === conversationId);
  const message = conversation?.messages.find((m) => m.id === messageId);
  if (!message) return;
  void api
    .finalizeMessage(
      messageId,
      message.content,
      message.stats ? JSON.stringify(message.stats) : null,
    )
    .catch(() => {
      /* history is best-effort; the turn is already on screen */
    });
}

export function useChat() {
  const unlistenRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    let disposed = false;

    listenChat({
      onDelta: ({ requestId, text }) => {
        const target = inFlight.get(requestId);
        if (!target) return;
        const store = useStore.getState();
        store.appendToMessage(target.conversationId, target.messageId, text);

        // Time the reasoning block by watching for the moment it closes.
        // Reading the accumulated content back is what makes this correct for a
        // tag that arrives split across several tokens.
        if (target.thinkingMs === undefined) {
          const message = useStore
            .getState()
            .conversations.find((c) => c.id === target.conversationId)
            ?.messages.find((m) => m.id === target.messageId);
          if (message) {
            const thinkingNow = isThinking(message.content);
            if (target.wasThinking && !thinkingNow) {
              target.thinkingMs = Date.now() - target.startedAt;
            }
            target.wasThinking = target.wasThinking || thinkingNow;
          }
        }
      },
      onDone: (e) => {
        const target = inFlight.get(e.requestId);
        if (!target) return;
        inFlight.delete(e.requestId);
        const store = useStore.getState();
        store.patchMessage(target.conversationId, target.messageId, {
          streaming: false,
          stats: {
            reason: e.reason,
            promptTokens: e.promptTokens,
            completionTokens: e.completionTokens,
            ttftMs: e.ttftMs,
            tokensPerSec: e.tokensPerSec,
            thinkingMs: target.thinkingMs ?? null,
          },
        });
        persistFinished(target.conversationId, target.messageId);
        if (store.activeRequestId === e.requestId)
          store.setActiveRequestId(null);
      },
      onError: (e) => {
        const target = inFlight.get(e.requestId);
        if (!target) return;
        inFlight.delete(e.requestId);
        const store = useStore.getState();
        store.patchMessage(target.conversationId, target.messageId, {
          streaming: false,
          error: e.message,
        });
        // Persist whatever text arrived before the failure rather than leaving
        // an empty row that reads as "the model said nothing".
        persistFinished(target.conversationId, target.messageId);
        if (store.activeRequestId === e.requestId)
          store.setActiveRequestId(null);
      },
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unlistenRef.current = unlisten;
    });

    return () => {
      disposed = true;
      unlistenRef.current?.();
      unlistenRef.current = null;
    };
  }, []);

  /** Send `text` as a new user turn. */
  const send = useCallback(async (text: string) => {
    const store = useStore.getState();
    const conversationId = store.activeId ?? (await store.newConversation());
    const conversation = useStore
      .getState()
      .conversations.find((c) => c.id === conversationId);
    if (!conversation) return;

    const userMessage: UiMessage = {
      id: nextId(),
      role: "user",
      content: text,
    };
    store.appendMessage(conversationId, userMessage);
    void api
      .appendMessage({
        conversationId,
        id: userMessage.id,
        role: "user",
        content: text,
        meta: null,
      })
      .catch(() => {});

    // The server is stateless, so every turn replays the whole conversation.
    // Messages that errored out carry no usable assistant text and are dropped
    // rather than sent as empty turns.
    // Reasoning is stripped from replayed history. A model's chain of thought
    // is scratchpad, not conversation: feeding it back wastes context and,
    // on reasoning models, degrades the next answer. The raw text stays in the
    // store — only what goes over the wire is trimmed.
    const history: ChatMessage[] = [
      ...conversation.messages
        .filter((m) => !m.error)
        .map((m) => ({
          role: m.role,
          content: m.role === "assistant" ? answerOnly(m.content) : m.content,
        }))
        .filter((m) => m.content.length > 0),
      { role: "user" as const, content: text },
    ];

    const assistantId = nextId();
    store.appendMessage(conversationId, {
      id: assistantId,
      role: "assistant",
      content: "",
      streaming: true,
    });
    void api
      .appendMessage({
        conversationId,
        id: assistantId,
        role: "assistant",
        content: "",
        meta: null,
      })
      .catch(() => {});

    try {
      const requestId = await api.chatSend(
        history,
        useStore.getState().sampling,
      );
      inFlight.set(requestId, {
        conversationId,
        messageId: assistantId,
        startedAt: Date.now(),
        wasThinking: false,
      });
      useStore.getState().setActiveRequestId(requestId);
    } catch (e) {
      useStore.getState().patchMessage(conversationId, assistantId, {
        streaming: false,
        error: errorMessage(e),
      });
    }
  }, []);

  /** Re-run the last user turn, discarding the assistant answer that followed. */
  const regenerate = useCallback(async () => {
    const store = useStore.getState();
    const conversation = store.conversations.find(
      (c) => c.id === store.activeId,
    );
    if (!conversation) return;

    // Walk back to the last user message; everything from the assistant reply
    // after it is what gets replaced.
    const lastUserIndex = [...conversation.messages]
      .reverse()
      .findIndex((m) => m.role === "user");
    if (lastUserIndex === -1) return;
    const index = conversation.messages.length - 1 - lastUserIndex;
    const prompt = conversation.messages[index].content;

    // Drop the user turn and everything after it locally and in the store, then
    // resend — `send` re-appends the user message, so the history stays exact.
    const kept = conversation.messages.slice(0, index);
    useStore.setState((s) => ({
      conversations: s.conversations.map((c) =>
        c.id === conversation.id ? { ...c, messages: kept } : c,
      ),
    }));
    await api.truncateConversation(conversation.id, index).catch(() => {});
    await send(prompt);
  }, [send]);

  const stop = useCallback(async () => {
    await api.chatStop();
  }, []);

  return { send, stop, regenerate };
}
