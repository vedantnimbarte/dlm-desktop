/** Application state, backed by the SQLite store in the Rust process.
 *
 * Conversations are persisted, but the UI does not wait on the database to
 * render: actions update local state first and write behind it. A dropped write
 * costs a message on next launch; a synchronous write on every token would cost
 * the streaming feel that makes the app usable. */

import { create } from "zustand";
import { api } from "./api";
import type {
  ApiLogEntry,
  Capabilities,
  Conversation,
  FitPreview,
  LocalModel,
  LogLine,
  Sampling,
  SearchHit,
  ServeConfig,
  ServerMetrics,
  Status,
  UiMessage,
} from "./types";

/** How many engine log lines the UI keeps. Matches the backend ring buffer. */
const LOG_LIMIT = 2000;

let idCounter = 0;
const nextId = () => `${Date.now().toString(36)}-${(idCounter++).toString(36)}`;

export type RightPanel = "model" | "settings" | "logs" | "api";

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
  activeRequestId: number | null;
  historyAvailable: boolean;

  loadConversations: () => Promise<void>;
  newConversation: () => Promise<string>;
  selectConversation: (id: string) => Promise<void>;
  deleteConversation: (id: string) => Promise<void>;
  renameConversation: (id: string, title: string) => Promise<void>;
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

  // --- search -------------------------------------------------------------
  searchQuery: string;
  searchHits: SearchHit[];
  setSearchQuery: (q: string) => void;
  runSearch: (q: string) => Promise<void>;

  // --- models -------------------------------------------------------------
  modelsRoot: string | null;
  localModels: LocalModel[];
  modelsScanning: boolean;
  modelsError: string | null;
  selectedModelPath: string | null;
  fit: FitPreview | null;
  fitLoading: boolean;
  fitError: string | null;

  setModelsRoot: (root: string | null) => void;
  scanModels: (root: string) => Promise<void>;
  selectModel: (path: string | null) => void;
  loadFit: (path: string) => Promise<void>;

  // --- api log ------------------------------------------------------------
  apiLog: ApiLogEntry[];
  setApiLog: (entries: ApiLogEntry[]) => void;
  pushApiLog: (entry: ApiLogEntry) => void;

  // --- reasoning display ---------------------------------------------------
  /** Show reasoning blocks at all. */
  showThinking: boolean;
  /** Keep them expanded instead of collapsing once the answer starts. */
  expandThinking: boolean;
  setShowThinking: (v: boolean) => void;
  setExpandThinking: (v: boolean) => void;

  // --- ui -----------------------------------------------------------------
  rightPanel: RightPanel;
  setRightPanel: (p: RightPanel) => void;
  /** Whether the flow panel is open. Closing it also unsubscribes from
   *  telemetry, which is what makes the engine stop collecting. */
  flowOpen: boolean;
  setFlowOpen: (open: boolean) => void;
}

export const useStore = create<AppStore>((set, get) => ({
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
      logs:
        s.logs.length >= LOG_LIMIT
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
  historyAvailable: true,

  loadConversations: async () => {
    try {
      const stored = await api.listConversations();
      set({
        historyAvailable: true,
        conversations: stored.map((c) => ({
          id: c.id,
          title: c.title,
          createdAt: c.created,
          messages: [],
          loaded: false,
          messageCount: c.messageCount,
        })),
      });
    } catch {
      // The database could not be opened. Chat still works; history does not,
      // and the sidebar says so rather than looking mysteriously empty.
      set({ historyAvailable: false });
    }
  },

  newConversation: async () => {
    const id = nextId();
    const conversation: Conversation = {
      id,
      title: "New chat",
      createdAt: Date.now(),
      messages: [],
      loaded: true,
      messageCount: 0,
    };
    set((s) => ({
      conversations: [conversation, ...s.conversations],
      activeId: id,
    }));
    try {
      await api.createConversation(id, conversation.title);
    } catch {
      set({ historyAvailable: false });
    }
    return id;
  },

  selectConversation: async (id) => {
    set({ activeId: id });
    const existing = get().conversations.find((c) => c.id === id);
    if (!existing || existing.loaded) return;
    try {
      const rows = await api.conversationMessages(id);
      set((s) => ({
        conversations: s.conversations.map((c) =>
          c.id !== id
            ? c
            : {
                ...c,
                loaded: true,
                messages: rows.map((r) => ({
                  id: r.id,
                  role: r.role,
                  content: r.content,
                  stats: r.meta ? safeParse(r.meta) : undefined,
                })),
              },
        ),
      }));
    } catch {
      set({ historyAvailable: false });
    }
  },

  deleteConversation: async (id) => {
    set((s) => {
      const conversations = s.conversations.filter((c) => c.id !== id);
      return {
        conversations,
        activeId:
          s.activeId === id ? (conversations[0]?.id ?? null) : s.activeId,
      };
    });
    try {
      await api.deleteConversation(id);
    } catch {
      /* already gone from the UI; nothing useful to do */
    }
  },

  renameConversation: async (id, title) => {
    set((s) => ({
      conversations: s.conversations.map((c) =>
        c.id === id ? { ...c, title } : c,
      ),
    }));
    try {
      await api.renameConversation(id, title);
    } catch {
      /* best effort */
    }
  },

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
        if (title !== c.title) void api.renameConversation(c.id, title);
        return { ...c, messages, title, messageCount: messages.length };
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

  setSampling: (patch) =>
    set((s) => ({ sampling: { ...s.sampling, ...patch } })),
  setActiveRequestId: (activeRequestId) => set({ activeRequestId }),

  searchQuery: "",
  searchHits: [],
  setSearchQuery: (searchQuery) => set({ searchQuery }),
  runSearch: async (q) => {
    if (!q.trim()) {
      set({ searchHits: [] });
      return;
    }
    try {
      set({ searchHits: await api.searchMessages(q) });
    } catch {
      set({ searchHits: [] });
    }
  },

  modelsRoot: null,
  localModels: [],
  modelsScanning: false,
  modelsError: null,
  selectedModelPath: null,
  fit: null,
  fitLoading: false,
  fitError: null,

  setModelsRoot: (modelsRoot) => set({ modelsRoot }),

  scanModels: async (root) => {
    set({ modelsScanning: true, modelsError: null });
    try {
      const localModels = await api.scanModels(root);
      set({ localModels, modelsRoot: root, modelsScanning: false });
    } catch (e) {
      set({
        modelsScanning: false,
        localModels: [],
        modelsError: e instanceof Object && "message" in e ? String(e.message) : String(e),
      });
    }
  },

  selectModel: (selectedModelPath) => {
    set({ selectedModelPath, fit: null, fitError: null });
    if (selectedModelPath) void get().loadFit(selectedModelPath);
  },

  loadFit: async (path) => {
    const config = get().config;
    set({ fitLoading: true, fitError: null });
    try {
      const fit = await api.modelFit({
        path,
        // Plan for the settings actually in the form, so the preview answers
        // "will it fit with *my* settings" rather than with defaults.
        contextLength: config?.contextLength ?? 8192,
        maxBatch: config?.maxBatch ?? 8,
        safetyMarginGb: config?.safetyMarginGb ?? null,
        vramBudgetGb: config?.vramBudgetGb ?? null,
      });
      set({ fit, fitLoading: false });
    } catch (e) {
      set({
        fitLoading: false,
        fit: null,
        fitError:
          e instanceof Object && "message" in e ? String(e.message) : String(e),
      });
    }
  },

  apiLog: [],
  setApiLog: (apiLog) => set({ apiLog }),
  pushApiLog: (entry) => set((s) => ({ apiLog: [entry, ...s.apiLog] })),

  // Reasoning is shown but collapsed by default: visible enough to notice the
  // model is working, quiet enough not to bury the answer.
  showThinking: loadFlag("dlm.showThinking", true),
  expandThinking: loadFlag("dlm.expandThinking", false),
  setShowThinking: (showThinking) => {
    saveFlag("dlm.showThinking", showThinking);
    set({ showThinking });
  },
  setExpandThinking: (expandThinking) => {
    saveFlag("dlm.expandThinking", expandThinking);
    set({ expandThinking });
  },

  rightPanel: "model",
  setRightPanel: (rightPanel) => set({ rightPanel }),
  flowOpen: false,
  setFlowOpen: (flowOpen) => set({ flowOpen }),
}));

/** Selector for the open conversation. */
export function useActiveConversation(): Conversation | null {
  return useStore(
    (s) => s.conversations.find((c) => c.id === s.activeId) ?? null,
  );
}

/** Display preferences live in localStorage rather than the SQLite store: they
 *  are per-machine UI state, not conversation data, and must be readable
 *  synchronously during the first render. */
function loadFlag(key: string, fallback: boolean): boolean {
  try {
    const raw = localStorage.getItem(key);
    return raw === null ? fallback : raw === "true";
  } catch {
    return fallback;
  }
}

function saveFlag(key: string, value: boolean) {
  try {
    localStorage.setItem(key, String(value));
  } catch {
    /* private mode or a locked-down webview; the setting just will not stick */
  }
}

function summarize(text: string): string {
  const line = text.trim().split("\n")[0] ?? "";
  return line.length > 42 ? `${line.slice(0, 42)}…` : line || "New chat";
}

function safeParse(json: string): UiMessage["stats"] | undefined {
  try {
    return JSON.parse(json);
  } catch {
    return undefined;
  }
}

export { nextId };
