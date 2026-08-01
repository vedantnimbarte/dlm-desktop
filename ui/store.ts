/** Application state.
 *
 * Conversations are held here in memory for now. The action surface
 * (`newConversation`, `appendMessage`, `patchMessage`, …) is deliberately the
 * shape a persistent store would have, so moving to SQLite in the backend
 * replaces the bodies of these actions and nothing in the components. */

import { create } from "zustand";
import type {
  Capabilities,
  Conversation,
  LogLine,
  Sampling,
  ServeConfig,
  ServerMetrics,
  Status,
  UiMessage,
} from "./types";

/** How many engine log lines the UI keeps. Matches the backend ring buffer. */
const LOG_LIMIT = 2000;

let idCounter = 0;
const nextId = () => `${Date.now().toString(36)}-${(idCounter++).toString(36)}`;

export type RightPanel = "model" | "settings" | "logs";

interface AppStore {
  // --- connection ---------------------------------------------------------
  status: Status;
  caps: Capabilities | null;
  probeError: string | null;
  logs: LogLine[];
  metrics: ServerMetrics | null;
  config: ServeConfig | null;
  connecting: boolean;

  setStatus: (s: Status) => void;
  setCaps: (c: Capabilities | null) => void;
  setProbeError: (e: string | null) => void;
  pushLog: (l: LogLine) => void;
  setLogs: (l: LogLine[]) => void;
  setMetrics: (m: ServerMetrics | null) => void;
  setConfig: (c: ServeConfig) => void;
  patchConfig: (p: Partial<ServeConfig>) => void;
  setConnecting: (b: boolean) => void;

  // --- conversations ------------------------------------------------------
  conversations: Conversation[];
  activeId: string | null;
  sampling: Sampling;
  /** Request id of the turn currently streaming, if any. */
  activeRequestId: number | null;

  newConversation: () => string;
  selectConversation: (id: string) => void;
  deleteConversation: (id: string) => void;
  renameConversation: (id: string, title: string) => void;
  appendMessage: (conversationId: string, message: UiMessage) => void;
  patchMessage: (
    conversationId: string,
    messageId: string,
    patch: Partial<UiMessage>,
  ) => void;
  appendToMessage: (
    conversationId: string,
    messageId: string,
    text: string,
  ) => void;
  setSampling: (s: Partial<Sampling>) => void;
  setActiveRequestId: (id: number | null) => void;

  // --- ui -----------------------------------------------------------------
  rightPanel: RightPanel;
  setRightPanel: (p: RightPanel) => void;
}

export const useStore = create<AppStore>((set) => ({
  status: { kind: "disconnected" },
  caps: null,
  probeError: null,
  logs: [],
  metrics: null,
  config: null,
  connecting: false,

  setStatus: (status) => set({ status }),
  setCaps: (caps) => set({ caps }),
  setProbeError: (probeError) => set({ probeError }),
  pushLog: (line) =>
    set((s) => ({
      logs: s.logs.length >= LOG_LIMIT
        ? [...s.logs.slice(s.logs.length - LOG_LIMIT + 1), line]
        : [...s.logs, line],
    })),
  setLogs: (logs) => set({ logs }),
  setMetrics: (metrics) => set({ metrics }),
  setConfig: (config) => set({ config }),
  patchConfig: (patch) =>
    set((s) => ({ config: s.config ? { ...s.config, ...patch } : s.config })),
  setConnecting: (connecting) => set({ connecting }),

  conversations: [],
  activeId: null,
  sampling: { temperature: 0.7, max_tokens: 512 },
  activeRequestId: null,

  newConversation: () => {
    const conversation: Conversation = {
      id: nextId(),
      title: "New chat",
      createdAt: Date.now(),
      messages: [],
    };
    set((s) => ({
      conversations: [conversation, ...s.conversations],
      activeId: conversation.id,
    }));
    return conversation.id;
  },

  selectConversation: (activeId) => set({ activeId }),

  deleteConversation: (id) =>
    set((s) => {
      const conversations = s.conversations.filter((c) => c.id !== id);
      return {
        conversations,
        // Deleting the open chat falls through to the next one rather than
        // leaving the pane pointed at nothing.
        activeId:
          s.activeId === id ? (conversations[0]?.id ?? null) : s.activeId,
      };
    }),

  renameConversation: (id, title) =>
    set((s) => ({
      conversations: s.conversations.map((c) =>
        c.id === id ? { ...c, title } : c,
      ),
    })),

  appendMessage: (conversationId, message) =>
    set((s) => ({
      conversations: s.conversations.map((c) => {
        if (c.id !== conversationId) return c;
        const messages = [...c.messages, message];
        // First user line names the chat; a sidebar full of "New chat" is
        // useless for finding anything later.
        const title =
          c.title === "New chat" && message.role === "user"
            ? summarize(message.content)
            : c.title;
        return { ...c, messages, title };
      }),
    })),

  patchMessage: (conversationId, messageId, patch) =>
    set((s) => ({
      conversations: s.conversations.map((c) =>
        c.id !== conversationId
          ? c
          : {
              ...c,
              messages: c.messages.map((m) =>
                m.id === messageId ? { ...m, ...patch } : m,
              ),
            },
      ),
    })),

  appendToMessage: (conversationId, messageId, text) =>
    set((s) => ({
      conversations: s.conversations.map((c) =>
        c.id !== conversationId
          ? c
          : {
              ...c,
              messages: c.messages.map((m) =>
                m.id === messageId ? { ...m, content: m.content + text } : m,
              ),
            },
      ),
    })),

  setSampling: (patch) => set((s) => ({ sampling: { ...s.sampling, ...patch } })),
  setActiveRequestId: (activeRequestId) => set({ activeRequestId }),

  rightPanel: "model",
  setRightPanel: (rightPanel) => set({ rightPanel }),
}));

/** Selector for the open conversation. */
export function useActiveConversation(): Conversation | null {
  return useStore(
    (s) => s.conversations.find((c) => c.id === s.activeId) ?? null,
  );
}

/** Helper for callers outside React. */
export const storeApi = {
  get: () => useStore.getState(),
};

function summarize(text: string): string {
  const line = text.trim().split("\n")[0] ?? "";
  return line.length > 42 ? `${line.slice(0, 42)}…` : line || "New chat";
}

export { nextId };
