/** Thin typed wrappers over the Tauri command surface.
 *
 * Every backend call goes through here so the rest of the UI never touches
 * `invoke` directly and the command names live in exactly one place. */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  Capabilities,
  ChatMessage,
  DeltaEvent,
  DoneEvent,
  ErrorEvent,
  LogLine,
  Sampling,
  ServeConfig,
  ServerMetrics,
  Status,
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
