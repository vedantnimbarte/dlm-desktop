/** Thin typed wrappers over the Tauri command surface.
 *
 * Every backend call goes through here so the rest of the UI never touches
 * `invoke` directly and the command names live in exactly one place. */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ApiLogEntry,
  Capabilities,
  ChatMessage,
  DeltaEvent,
  DoneEvent,
  ErrorEvent,
  FitPreview,
  FlowSnapshot,
  LocalModel,
  LogLine,
  Sampling,
  SearchHit,
  ServeConfig,
  ServerMetrics,
  Status,
  StoredConversation,
  StoredMessage,
} from "./types";

export const api = {
  status: () => invoke<Status>("engine_status"),
  capabilities: () => invoke<Capabilities | null>("engine_capabilities"),
  logs: () => invoke<LogLine[]>("engine_logs"),
  probeEngine: () => invoke<Capabilities>("probe_engine"),
  setBinaryOverride: (path: string | null) =>
    invoke<void>("set_binary_override", { path }),

  startLocal: (config: ServeConfig) => invoke<Status>("start_local", { config }),
  attachRemote: (address: string, apiKey: string | null) =>
    invoke<Status>("attach_remote", { address, apiKey }),
  disconnect: () => invoke<void>("disconnect"),

  metrics: () => invoke<ServerMetrics>("server_metrics"),
  previewCommand: (config: ServeConfig) =>
    invoke<string>("preview_command", { config }),
  defaultServeConfig: () => invoke<ServeConfig>("default_serve_config"),

  chatSend: (messages: ChatMessage[], sampling: Sampling) =>
    invoke<number>("chat_send", { messages, sampling }),
  chatStop: () => invoke<void>("chat_stop"),

  // --- models -------------------------------------------------------------
  defaultModelsRoot: () => invoke<string | null>("default_models_root"),
  scanModels: (root: string) => invoke<LocalModel[]>("scan_models", { root }),
  modelFit: (args: {
    path: string;
    contextLength: number;
    maxBatch: number;
    safetyMarginGb: number | null;
    vramBudgetGb: number | null;
  }) => invoke<FitPreview>("model_fit", args),

  // --- history ------------------------------------------------------------
  listConversations: () => invoke<StoredConversation[]>("list_conversations"),
  createConversation: (id: string, title: string) =>
    invoke<StoredConversation>("create_conversation", { id, title }),
  renameConversation: (id: string, title: string) =>
    invoke<void>("rename_conversation", { id, title }),
  deleteConversation: (id: string) =>
    invoke<void>("delete_conversation", { id }),
  conversationMessages: (conversationId: string) =>
    invoke<StoredMessage[]>("conversation_messages", { conversationId }),
  appendMessage: (args: {
    conversationId: string;
    id: string;
    role: string;
    content: string;
    meta: string | null;
  }) => invoke<number>("append_message", args),
  finalizeMessage: (id: string, content: string, meta: string | null) =>
    invoke<void>("finalize_message", { id, content, meta }),
  truncateConversation: (conversationId: string, seq: number) =>
    invoke<void>("truncate_conversation", { conversationId, seq }),
  searchMessages: (query: string, limit?: number) =>
    invoke<SearchHit[]>("search_messages", { query, limit: limit ?? null }),

  apiLog: (limit?: number) =>
    invoke<ApiLogEntry[]>("api_log", { limit: limit ?? null }),
  clearApiLog: () => invoke<void>("clear_api_log"),

  // --- flow telemetry -----------------------------------------------------
  /** Subscribing is what makes the engine start collecting. */
  flowSubscribe: () => invoke<void>("flow_subscribe"),
  flowUnsubscribe: () => invoke<void>("flow_unsubscribe"),
  flowState: () => invoke<FlowSnapshot>("flow_state"),
};

/** Subscribe to the streaming chat events for one turn.
 *
 * Returns an unsubscribe function; callers must call it when the turn ends or
 * the component unmounts, otherwise listeners accumulate across turns and every
 * delta gets handled N times. */
export async function listenChat(handlers: {
  onDelta: (e: DeltaEvent) => void;
  onDone: (e: DoneEvent) => void;
  onError: (e: ErrorEvent) => void;
}): Promise<UnlistenFn> {
  const unlisteners = await Promise.all([
    listen<DeltaEvent>("chat:delta", (e) => handlers.onDelta(e.payload)),
    listen<DoneEvent>("chat:done", (e) => handlers.onDone(e.payload)),
    listen<ErrorEvent>("chat:error", (e) => handlers.onError(e.payload)),
  ]);
  return () => unlisteners.forEach((u) => u());
}

/** Live engine stdout/stderr. */
export function listenEngineLog(
  onLine: (line: LogLine) => void,
): Promise<UnlistenFn> {
  return listen<LogLine>("engine:log", (e) => onLine(e.payload));
}

/** One event per completed API call, so the log panel updates without polling. */
export function listenApiLog(
  onEntry: (entry: ApiLogEntry) => void,
): Promise<UnlistenFn> {
  return listen<ApiLogEntry>("api:logged", (e) => onEntry(e.payload));
}

/** Normalize a rejected `invoke` into a readable message.
 *
 * Tauri rejects with whatever the command's error type serialized to, which for
 * this backend is `UiError`; a plain string can still arrive from a panic or a
 * serialization failure, so both shapes are handled. */
export function errorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  return String(e);
}
