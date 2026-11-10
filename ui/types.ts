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
export type KvQuant = "f16" | "f32" | "int8" | "int4";
export type Device = "auto" | "cpu" | "gpu";
export type Speculative = "auto" | "off" | "ngram" | "draft";

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
  /** `--gpu-layers`: first N layers on the GPU, the rest on the CPU. */
  gpuLayers: number | null;
  /** "auto" (dlm detects it) or a template name. */
  chatTemplate: string;
  eosToken: number | null;
  /** null keeps dlm's default (64 on GPU, off on CPU). */
  prefixCacheSize: number | null;
  speculative: Speculative;
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
  frequency_penalty?: number;
  presence_penalty?: number;
  dry_multiplier?: number;
  response_format?: { type: "json_object" };
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
    /** Wall-clock spent inside a reasoning block, when the model emitted one. */
    thinkingMs?: number | null;
    /** Counts came from stream deltas, not dlm's `usage`. */
    estimated?: boolean;
    speculative?: SpeculativeUsage | null;
  };
}

export interface SpeculativeUsage {
  draftProposed: number;
  draftAccepted: number;
  acceptanceRate: number;
}

export interface Conversation {
  id: string;
  title: string;
  createdAt: number;
  /** Loaded lazily from the store when the conversation is opened. */
  messages: UiMessage[];
  /** True once the messages have been fetched, so an empty chat is
   *  distinguishable from one that simply has not loaded yet. */
  loaded: boolean;
  messageCount?: number;
}

/** `store::Conversation` as it crosses the boundary. */
export interface StoredConversation {
  id: string;
  title: string;
  modelId: string | null;
  created: number;
  updated: number;
  messageCount: number;
}

export interface StoredMessage {
  id: string;
  conversationId: string;
  seq: number;
  role: "system" | "user" | "assistant";
  content: string;
  ts: number;
  meta: string | null;
}

export interface SearchHit {
  conversationId: string;
  conversationTitle: string;
  messageId: string;
  role: string;
  /** FTS5 excerpt; the match is wrapped in [ ]. */
  snippet: string;
  ts: number;
}

export interface ApiLogEntry {
  id: number;
  ts: number;
  method: string;
  path: string;
  status: number | null;
  latencyMs: number | null;
  ttftMs: number | null;
  promptTokens: number | null;
  completionTokens: number | null;
  requestPreview: string | null;
  responsePreview: string | null;
  error: string | null;
}

export interface LocalModel {
  path: string;
  name: string;
  checkpointBytes: number;
  numLayers: number;
  hiddenSize: number;
  isMoe: boolean;
  estimatedParams: number;
  hasTokenizer: boolean;
  /** Published quantization of a `.gguf` file ("Q4_K_M"); null for safetensors. */
  ggufType: string | null;
  /** Set when dlm cannot decode the architecture; the model is still listed. */
  error: string | null;
}

export type VramSource =
  | { kind: "device"; free: number; total: number; via: string }
  | { kind: "manual"; bytes: number }
  | { kind: "unavailable"; reason: string };

export interface FitRow {
  quant: "checkpoint" | "int8" | "int4";
  available: boolean;
  unavailableReason: string | null;
  layersToLoad: number;
  numLayers: number;
  residentFraction: number;
  streamPasses: number;
  perLayerWeightBytes: number;
  kvTotalBytes: number;
  pinnedBytes: number;
  usableBytes: number;
  fitsEntirely: boolean;
}

export interface FitPreview {
  model: LocalModel;
  vram: VramSource;
  nativeQuant: string;
  contextLength: number;
  maxBatch: number;
  rows: FitRow[];
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
  estimated: boolean;
  speculative: SpeculativeUsage | null;
}

export interface ErrorEvent {
  requestId: number;
  message: string;
}

// --- flow telemetry --------------------------------------------------------

export type Residency = "absent" | "resident" | "prefetched" | "evicted";

export interface StageStat {
  events: number;
  bytes: number;
  totalUs: number;
  /** Bytes/sec while the stage was running. Null when it moves no bytes. */
  bytesPerSec: number | null;
  /** Duration is wall-clock, not device-measured — render as an estimate. */
  estimated: boolean;
}

export interface LayerState {
  layer: number;
  residency: Residency;
  lastSeenUs: number;
  evictions: number;
  bytes: number;
}

export interface TelemetrySnapshot {
  numLayers: number;
  residentLayers: number;
  layerBytes: number[];
  pinnedBytes: number;
  streaming: boolean;
  device: string;
  estimatedH2d: boolean;
}

export interface FlowState {
  snapshot: TelemetrySnapshot | null;
  mmapRead: StageStat;
  ramHit: StageStat;
  ramMiss: StageStat;
  dequant: StageStat;
  pinStage: StageStat;
  h2d: StageStat;
  compute: StageStat;
  evict: StageStat;
  prefetch: StageStat;
  layers: LayerState[];
  prefetchDepth: number | null;
  /** 1.0 means no events were lost. */
  sampledFraction: number;
  dropped: number;
  token: number;
  windowSecs: number;
  active: boolean;
}

export interface GpuInfo {
  name: string;
  vramTotal: number | null;
  vramUsed: number | null;
  utilizationPct: number | null;
  temperatureC: number | null;
  clockMhz: number | null;
  maxClockMhz: number | null;
  throttleReasons: string[];
  pcieLinkWidth: number | null;
  pcieMaxLinkWidth: number | null;
  pcieLinkGen: number | null;
  pcieMaxLinkGen: number | null;
  pcieTxBytesPerSec: number | null;
  pcieRxBytesPerSec: number | null;
}

export interface HostInfo {
  cpuPct: number | null;
  ramTotal: number | null;
  ramUsed: number | null;
  engineReadBytesPerSec: number | null;
}

export interface HwSnapshot {
  gpus: GpuInfo[];
  host: HostInfo;
  gpuUnavailableReason: string | null;
}

export type Severity = "info" | "warning" | "critical";

export interface Finding {
  id: string;
  severity: Severity;
  title: string;
  detail: string;
  fix: string | null;
}

export interface FlowSnapshot {
  flow: FlowState;
  hw: HwSnapshot;
  findings: Finding[];
  subscribed: boolean;
}

/** A dlm server found already listening on localhost. */
export interface DiscoveredServer {
  address: string;
  modelId: string;
}
