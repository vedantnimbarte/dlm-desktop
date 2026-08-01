/** Mirrors of the Rust types crossing the Tauri boundary.
 *
 * These are hand-written rather than generated: the surface is small, and
 * keeping them here makes the serde `rename_all = "camelCase"` contract
 * explicit on the side that consumes it. */

export type Status =
  | { kind: "disconnected" }
  | { kind: "starting"; model: string; elapsedSecs: number }
  | { kind: "ready"; modelId: string; endpoint: string; local: boolean }
  | { kind: "failed"; detail: string };

export interface Capabilities {
  binary: string;
  version: string;
  /** Whether this dlm build has `--telemetry` (engine change A4). */
  telemetry: boolean;
}

export type LogStreamKind = "stdout" | "stderr" | "supervisor";

export interface LogLine {
  tMs: number;
  stream: LogStreamKind;
  text: string;
}

export interface UiError {
  kind: string;
  message: string;
}

export type Quant = "checkpoint" | "int8" | "int4";
export type KvQuant = "none" | "int8" | "int4";
export type Device = "auto" | "cpu" | "gpu";
export type ChatTemplate = "plain" | "chatml" | "llama3";

/** Mirrors `engine::ServeConfig`; field names match dlm's flags. */
export interface ServeConfig {
  modelPath: string;
  port: number;
  host: string;
  quant: Quant;
  kvQuant: KvQuant;
  vramBudgetGb: number | null;
  safetyMarginGb: number | null;
  ramCacheGb: number | null;
  expertCacheGb: number | null;
  contextLength: number;
  stream: boolean;
  residentLayers: number | null;
  prefetchDepth: number;
  autoPrefetch: boolean;
  device: Device;
  multiGpuIds: number[];
  maxBatch: number;
  chatTemplate: ChatTemplate;
  eosToken: number | null;
  prefixCacheSize: number;
  draftModelPath: string | null;
  draftGamma: number;
  apiKey: string | null;
  telemetry: boolean;
}

export interface Sampling {
  temperature?: number;
  top_p?: number;
  top_k?: number;
  min_p?: number;
  repetition_penalty?: number;
  seed?: number;
  max_tokens?: number;
  stop?: string[];
}

export interface StreamGauges {
  hits: number;
  misses: number;
  evictions: number;
  prefetched: number;
  depth: number;
  expert_hits: number;
  expert_misses: number;
  expert_evictions: number;
}

export interface ServerMetrics {
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  /** Absent (null) on a fully-resident run — not the same as all-zero. */
  stream: StreamGauges | null;
}

export interface ChatMessage {
  role: "system" | "user" | "assistant";
  content: string;
}

/** A message as the UI holds it: the wire shape plus per-turn measurements. */
export interface UiMessage extends ChatMessage {
  id: string;
  /** True while tokens are still arriving. */
  streaming?: boolean;
  error?: string;
  stats?: {
    reason?: string;
    promptTokens?: number | null;
    completionTokens?: number | null;
    ttftMs?: number | null;
    tokensPerSec?: number | null;
  };
}

export interface Conversation {
  id: string;
  title: string;
  createdAt: number;
  messages: UiMessage[];
}

export interface DeltaEvent {
  requestId: number;
  text: string;
}

export interface DoneEvent {
  requestId: number;
  reason: string;
  promptTokens: number | null;
  completionTokens: number | null;
  ttftMs: number | null;
  tokensPerSec: number | null;
}

export interface ErrorEvent {
  requestId: number;
  message: string;
}
