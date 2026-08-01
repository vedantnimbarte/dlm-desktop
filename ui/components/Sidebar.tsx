import { useEffect, useState } from "react";
import { useStore } from "../store";

/** How long to wait after the last keystroke before running a search. FTS5 is
 *  fast, but a query per character is still pointless work and makes the list
 *  flicker between partial-word matches. */
const SEARCH_DEBOUNCE_MS = 180;

export default function Sidebar() {
  const conversations = useStore((s) => s.conversations);
  const activeId = useStore((s) => s.activeId);
  const historyAvailable = useStore((s) => s.historyAvailable);
  const newConversation = useStore((s) => s.newConversation);
  const selectConversation = useStore((s) => s.selectConversation);
  const deleteConversation = useStore((s) => s.deleteConversation);
  const renameConversation = useStore((s) => s.renameConversation);
  const searchHits = useStore((s) => s.searchHits);
  const runSearch = useStore((s) => s.runSearch);

  const [query, setQuery] = useState("");
  const [renaming, setRenaming] = useState<string | null>(null);
  const [draftTitle, setDraftTitle] = useState("");

  useEffect(() => {
    const id = setTimeout(() => void runSearch(query), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(id);
  }, [query, runSearch]);

  const searching = query.trim().length > 0;

  return (
    <aside className="sidebar">
      <div className="sidebar-head">
        <span className="brand">dlm</span>
        <button
          className="ghost"
          title="New chat"
          onClick={() => void newConversation()}
        >
          + New
        </button>
      </div>

      <div className="sidebar-search">
        <input
          value={query}
          placeholder="Search conversations…"
          spellCheck={false}
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>

      <div className="convo-list">
        {!historyAvailable && (
          <div className="empty error-text">
            History is unavailable — the database could not be opened. Chat
            still works, but nothing is being saved.
          </div>
        )}

        {searching ? (
          searchHits.length === 0 ? (
            <div className="empty">No matches.</div>
          ) : (
            searchHits.map((hit) => (
              <div
                key={hit.messageId}
                className="hit"
                onClick={() => {
                  void selectConversation(hit.conversationId);
                  setQuery("");
                }}
              >
                <div className="hit-title">{hit.conversationTitle}</div>
                <div className="hit-snippet">
                  <span className="hit-role">{hit.role}</span>
                  {/* FTS5 marks the match with [ ]; render those runs bold
                      rather than leaving the brackets as literal noise. */}
                  {renderSnippet(hit.snippet)}
                </div>
              </div>
            ))
          )
        ) : conversations.length === 0 ? (
          <div className="empty">No conversations yet.</div>
        ) : (
          conversations.map((c) => (
            <div
              key={c.id}
              className={`convo${c.id === activeId ? " active" : ""}`}
              onClick={() => void selectConversation(c.id)}
              onDoubleClick={() => {
                setRenaming(c.id);
                setDraftTitle(c.title);
              }}
            >
              {renaming === c.id ? (
                <input
                  autoFocus
                  value={draftTitle}
                  onClick={(e) => e.stopPropagation()}
                  onChange={(e) => setDraftTitle(e.target.value)}
                  onBlur={() => {
                    void renameConversation(c.id, draftTitle.trim() || c.title);
                    setRenaming(null);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") e.currentTarget.blur();
                    if (e.key === "Escape") setRenaming(null);
                  }}
                />
              ) : (
                <>
                  <span className="convo-title">{c.title}</span>
                  <button
                    className="del"
                    title="Delete conversation"
                    onClick={(e) => {
                      // Without this the click also selects the row being
                      // removed.
                      e.stopPropagation();
                      void deleteConversation(c.id);
                    }}
                  >
                    ×
                  </button>
                </>
              )}
            </div>
          ))
        )}
      </div>

      {!searching && conversations.length > 0 && (
        <div className="sidebar-foot dim">Double-click a chat to rename it</div>
      )}
    </aside>
  );
}

/** Split an FTS5 snippet on its `[`…`]` match markers. */
function renderSnippet(snippet: string) {
  const parts = snippet.split(/(\[[^\]]*\])/g);
  return parts.map((part, i) =>
    part.startsWith("[") && part.endsWith("]") ? (
      <mark key={i}>{part.slice(1, -1)}</mark>
    ) : (
      <span key={i}>{part}</span>
    ),
  );
}
