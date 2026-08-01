import { useEffect, useRef, useState } from "react";
import { useActiveConversation, useStore } from "../store";
import { useChat } from "../useChat";
import Message from "./Message";
import { hasThinking } from "../thinking";

export default function ChatPane() {
  const conversation = useActiveConversation();
  const status = useStore((s) => s.status);
  const activeRequestId = useStore((s) => s.activeRequestId);
  const showThinking = useStore((s) => s.showThinking);
  const setShowThinking = useStore((s) => s.setShowThinking);
  const expandThinking = useStore((s) => s.expandThinking);
  const setExpandThinking = useStore((s) => s.setExpandThinking);
  const { send, stop, regenerate } = useChat();

  const [draft, setDraft] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedToBottom = useRef(true);

  const ready = status.kind === "ready";
  const streaming = activeRequestId !== null;

  // Follow the stream, but stop following the moment the user scrolls up to
  // read something — yanking them back to the bottom mid-read is the single
  // most irritating thing a chat UI can do.
  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinnedToBottom.current) el.scrollTop = el.scrollHeight;
  }, [conversation?.messages]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    pinnedToBottom.current = distance < 60;
  };

  const submit = async () => {
    const text = draft.trim();
    if (!text || !ready || streaming) return;
    setDraft("");
    pinnedToBottom.current = true;
    await send(text);
  };

  const messages = conversation?.messages ?? [];
  const conversationHasThinking = messages.some(
    (m) => m.role === "assistant" && hasThinking(m.content),
  );

  return (
    <main className="chat">
      <header className="chat-head">
        <span className="chat-model">
          {status.kind === "ready" ? status.modelId : "No model loaded"}
        </span>
        {status.kind === "ready" && (
          <span className="badge">
            {status.local ? "local" : "remote"} · {status.endpoint}
          </span>
        )}
        <span className="spacer" />

        {/* Only offered when this conversation actually contains reasoning —
            a toggle for something that never appears is just clutter. */}
        {conversationHasThinking && (
          <>
            <button
              className="ghost"
              title={
                showThinking
                  ? "Hide the model's reasoning"
                  : "Show the model's reasoning"
              }
              onClick={() => setShowThinking(!showThinking)}
            >
              {showThinking ? "Hide thinking" : "Show thinking"}
            </button>
            {showThinking && (
              <button
                className="ghost"
                title={
                  expandThinking
                    ? "Collapse reasoning once the answer starts"
                    : "Keep reasoning expanded"
                }
                onClick={() => setExpandThinking(!expandThinking)}
              >
                {expandThinking ? "Collapse" : "Expand"}
              </button>
            )}
          </>
        )}

        <button
          className="ghost"
          title="Discard the last answer and ask again"
          disabled={!ready || streaming || messages.length < 2}
          onClick={() => void regenerate()}
        >
          Regenerate
        </button>
      </header>

      <div className="messages" ref={scrollRef} onScroll={onScroll}>
        {messages.length === 0 ? (
          <div className="empty">
            {ready
              ? "Send a message to start."
              : "Load a model or attach to a running dlm server to begin."}
          </div>
        ) : (
          messages.map((m) => <Message key={m.id} message={m} />)
        )}
      </div>

      <div className="composer">
        <div className="composer-inner">
          <textarea
            value={draft}
            placeholder={ready ? "Message the model…" : "Not connected"}
            disabled={!ready}
            rows={1}
            onChange={(e) => {
              setDraft(e.target.value);
              // Grow with the content up to the CSS max-height.
              e.target.style.height = "auto";
              e.target.style.height = `${Math.min(e.target.scrollHeight, 200)}px`;
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void submit();
              }
            }}
          />
          {streaming ? (
            <button className="danger" onClick={() => void stop()}>
              Stop
            </button>
          ) : (
            <button
              className="primary"
              disabled={!ready || draft.trim().length === 0}
              onClick={() => void submit()}
            >
              Send
            </button>
          )}
        </div>
        <div className="composer-hint">
          Enter to send · Shift+Enter for a newline
        </div>
      </div>
    </main>
  );
}
