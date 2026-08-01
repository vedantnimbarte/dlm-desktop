/** Drives one streaming chat turn.
 *
 * Event listeners are registered **once** for the app, not once per turn, and
 * deltas are routed by request id through `inFlight`. Registering per turn is
 * the obvious approach and the wrong one: React strict-mode double-invocation
 * and any overlapping turn both end up double-appending every token. */

import { useCallback, useEffect, useRef } from "react";
import { api, errorMessage, listenChat } from "./api";
import { useStore, nextId } from "./store";
import type { ChatMessage } from "./types";

/** request id -> where its tokens belong. */
const inFlight = new Map<
  number,
  { conversationId: string; messageId: string }
>();

export function useChat() {
  const unlistenRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    let disposed = false;

    listenChat({
      onDelta: ({ requestId, text }) => {
        const target = inFlight.get(requestId);
        if (!target) return;
        useStore
          .getState()
          .appendToMessage(target.conversationId, target.messageId, text);
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
          },
        });
        if (store.activeRequestId === e.requestId) store.setActiveRequestId(null);
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
        if (store.activeRequestId === e.requestId) store.setActiveRequestId(null);
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

  const send = useCallback(async (text: string) => {
    const store = useStore.getState();
    const conversationId = store.activeId ?? store.newConversation();
    const conversation = useStore
      .getState()
      .conversations.find((c) => c.id === conversationId);
    if (!conversation) return;

    const userMessage = {
      id: nextId(),
      role: "user" as const,
      content: text,
    };
    store.appendMessage(conversationId, userMessage);

    // The server is stateless, so every turn replays the whole conversation.
    // Messages that errored out carry no usable assistant text and are dropped
    // rather than sent as empty turns.
    const history: ChatMessage[] = [
      ...conversation.messages
        .filter((m) => !m.error && m.content.length > 0)
        .map((m) => ({ role: m.role, content: m.content })),
      { role: "user" as const, content: text },
    ];

    const assistantId = nextId();
    store.appendMessage(conversationId, {
      id: assistantId,
      role: "assistant",
      content: "",
      streaming: true,
    });

    try {
      const requestId = await api.chatSend(history, useStore.getState().sampling);
      inFlight.set(requestId, { conversationId, messageId: assistantId });
      useStore.getState().setActiveRequestId(requestId);
    } catch (e) {
      useStore.getState().patchMessage(conversationId, assistantId, {
        streaming: false,
        error: errorMessage(e),
      });
    }
  }, []);

  const stop = useCallback(async () => {
    await api.chatStop();
  }, []);

  return { send, stop };
}
