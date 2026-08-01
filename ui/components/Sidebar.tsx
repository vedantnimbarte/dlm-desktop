import { useStore } from "../store";

export default function Sidebar() {
  const conversations = useStore((s) => s.conversations);
  const activeId = useStore((s) => s.activeId);
  const newConversation = useStore((s) => s.newConversation);
  const selectConversation = useStore((s) => s.selectConversation);
  const deleteConversation = useStore((s) => s.deleteConversation);

  return (
    <aside className="sidebar">
      <div className="sidebar-head">
        <span className="brand">dlm</span>
        <button
          className="ghost"
          title="New chat"
          onClick={() => newConversation()}
        >
          + New
        </button>
      </div>

      <div className="convo-list">
        {conversations.length === 0 ? (
          <div className="empty">No conversations yet.</div>
        ) : (
          conversations.map((c) => (
            <div
              key={c.id}
              className={`convo${c.id === activeId ? " active" : ""}`}
              onClick={() => selectConversation(c.id)}
            >
              <span className="convo-title">{c.title}</span>
              <button
                className="del"
                title="Delete conversation"
                onClick={(e) => {
                  // Without this the click also selects the row being removed.
                  e.stopPropagation();
                  deleteConversation(c.id);
                }}
              >
                ×
              </button>
            </div>
          ))
        )}
      </div>
    </aside>
  );
}
