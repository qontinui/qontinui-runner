// Shared types for settings components

// Re-export Monitor from shared geometry types
export type { Monitor } from "../../types/geometry";

export interface DebugSettings {
  enable_image_debug: boolean;
  top_matches_count: number;
}

export interface AppSettings {
  auto_load_last_config: boolean;
}

export interface ConnectionInfo {
  version: string;
  url: string;
  token: string;
  userId: string;
  /** Project ID as UUID string (e.g., "fb93478d-98bd-4e40-99f4-0f2c08c1fd5a") */
  projectId: string | null;
  createdAt: string;
  backendUrl: string;
}

export interface WebSocketSettings {
  // Connection
  enabled: boolean;
  url: string;
  token: string;
  projectId: string;
  connected: boolean;
  backendUrl: string;
  /** Custom user-defined name for this runner (e.g., "My Laptop") */
  runnerName: string;

  // Cloud permission status (read-only, from API)
  cloudPermissionEnabled: boolean;
  sessionsLimit: number | null;
  sessionsUsed: number;
  sessionsResetAt: string | null;

  // User controls
  sendToCloud: boolean;
  sendLogs: boolean;
  sendScreenshots: boolean;
  sendVideos: boolean;
}

export type ScreenSelectionType =
  | { type: "all" }
  | { type: "primary" }
  | { type: "specific"; indices: number[] };

export interface ScreenshotCaptureSettings {
  enabled: boolean;
  manualClicksEnabled: boolean;
  outputFolder: string;
  baseImageName: string;
  screens: ScreenSelectionType;
  captureTimings: number[];
}

export interface StorageUsage {
  screenshots: number;
  videos: number;
  screenshotCount: number;
  videoCount: number;
}

export interface StoragePaths {
  screenshot_path: string;
  video_path: string;
  max_screenshot_mb: number;
  max_video_mb: number;
  auto_cleanup: boolean;
}

export type LogFunction = (
  level: "info" | "warning" | "error" | "debug" | "success",
  message: string,
) => void;

export interface UpdateInfo {
  available: boolean;
  version?: string;
  current_version?: string;
  notes?: string;
  development?: boolean;
}

export type UpdateStatus = "idle" | "checking" | "downloading" | "installing" | "error";

// AI Settings Types
export type AiProvider = "claude_cli" | "claude_api" | "gemini_cli" | "gemini_api";
export type CliExecutionMode = "auto" | "windows_native" | "wsl" | "native";
export type GeminiAuthMethod = "oauth" | "api_key";
export type AccountSelectionMode = "manual" | "least_usage";

export interface ClaudeCliSettings {
  execution_mode: CliExecutionMode;
  custom_path?: string;
  timeout_seconds: number;
  /** Custom CLAUDE_CONFIG_DIR for multi-account support */
  config_dir?: string;
  /** How to select which account to use when multiple config dirs exist */
  account_selection_mode?: AccountSelectionMode;
  /** Auto-migrate a token-exhausted terminal session to a fresh account
   * (transcript copy + `claude --resume` respawn). Runner default: true. */
  auto_migrate_on_token_exhaustion?: boolean;
  /** After a migration respawn, nudge the resumed session to continue its
   * task once the CLI is idle. Runner default: true. */
  auto_continue_after_migration?: boolean;
}

/** One per-model scoped weekly limit (e.g. the Fable-only cap). */
export interface ModelLimitInfo {
  model: string;
  /** Fraction used, 0.0–1.0 */
  utilization: number;
  resets_at: number | null;
}

export interface AccountUsageInfo {
  config_dir: string;
  label: string;
  utilization: number;
  rate_limit_type: string | null;
  resets_at: number | null;
  status: string | null;
  error: string | null;
  /** Expected utilization at this point in the billing period (0.0–1.0) */
  expected_utilization: number | null;
  /** Actual minus expected utilization. Negative = under budget */
  usage_delta: number | null;
  /** Fraction of the billing period elapsed (0.0–1.0) */
  period_elapsed_fraction: number | null;
  /** Days remaining until reset */
  period_remaining_days: number | null;
  /** Session-window (5-hour) utilization 0.0–1.0 (OAuth usage source only) */
  session_utilization?: number | null;
  /** Unix seconds when the 5-hour session window resets */
  session_resets_at?: number | null;
  /** Per-model scoped weekly limits */
  model_limits?: ModelLimitInfo[];
  /** Stats source: "oauth_usage" (free endpoint) or "probe" (Haiku probe) */
  source?: string | null;
}

/** Weekly utilization at/above which an account is treated as "out of
 * tokens" for selection. Mirrors `EXHAUSTION_UTILIZATION` in the runner
 * (`src-tauri/src/commands/ai_settings.rs`). */
const EXHAUSTION_UTILIZATION = 0.99;

/**
 * Whether an account **won't serve a request right now** — at/over its weekly
 * cap, server-reported rejected/blocked/exceeded, or its usage probe errored
 * (the probe hits the same per-account quota the CLI uses, so this also catches
 * a spend-limited account whose weekly *token* utilization still reads low).
 * Mirrors the runner's `probe_result_exhausted`.
 */
export function isAccountExhausted(a: {
  utilization: number;
  status?: string | null;
  error?: string | null;
  session_utilization?: number | null;
}): boolean {
  if (a.error != null || a.utilization >= EXHAUSTION_UTILIZATION) return true;
  // A hit 5-hour session window blocks requests even while weekly reads low.
  if (a.session_utilization != null && a.session_utilization >= EXHAUSTION_UTILIZATION) {
    return true;
  }
  const s = (a.status ?? "").toLowerCase();
  return s.includes("reject") || s.includes("block") || s.includes("exceed");
}

/** The minimum shape `compareByUsageHeadroom` ranks on. Structural so both
 * `AccountUsageInfo` shapes (settings + the terminal subset in
 * `useSessionManager`) satisfy it. */
type RankableAccount = {
  utilization: number;
  expected_utilization?: number | null;
  usage_delta?: number | null;
  status?: string | null;
  error?: string | null;
};

/**
 * The pace tier an account falls in, carrying **that tier's own ranking key**.
 *
 * Each tier ranks on a different field in a different direction, so the key
 * travels with the tier rather than being flattened into one pre-negated
 * scalar — a flattened key hides "descending" inside a negation no reader can
 * see, and lets a later edit compare two different tiers' keys to each other,
 * which is meaningless.
 *
 * Mirrors the runner's `PaceRank` (`ai_provider/config.rs`).
 */
type PaceRank =
  | { tier: "under"; expected: number }
  | { tier: "unknown"; utilization: number }
  | { tier: "over"; ratio: number };

/** Tier precedence: measured spare capacity → no evidence → measured over. */
const PACE_TIER_INDEX: Record<PaceRank["tier"], number> = {
  under: 0,
  unknown: 1,
  over: 2,
};

/**
 * The over-pace ratio `utilization / expected`, through an explicit guard on
 * `expected === 0`.
 *
 * **Never divide here without the guard.** `0 / 0` is `NaN`, and
 * `Array.prototype.sort` treats a `NaN` comparator result as `0` — so an
 * unguarded division does not throw, it silently makes the resulting order
 * depend on input position instead of on the data.
 *
 * - `expected === 0 && utilization === 0` → `1` (exactly on pace, nothing
 *   spent; it lands in this tier only by the `delta >= 0` boundary).
 * - `expected === 0 && utilization > 0` → `Infinity` (the arithmetic limit:
 *   spend against a just-reset window, whose capacity is in no danger of
 *   expiring — the last account use-it-or-lose-it wants to burn).
 */
function overPaceRatio(utilization: number, expected: number): number {
  if (expected === 0) return utilization === 0 ? 1 : Infinity;
  return utilization / expected;
}

/** Classify one account into its pace tier and compute that tier's key. */
function paceRank(a: RankableAccount): PaceRank {
  const utilization = a.utilization ?? 0;
  const delta = a.usage_delta;
  const expected = a.expected_utilization;
  // No usable pace signal: it cannot be classified under- or over-pace, and
  // neither tier's key is computable. Fall back to raw utilization ascending.
  if (delta == null || expected == null) return { tier: "unknown", utilization };
  if (delta < 0) return { tier: "under", expected };
  return { tier: "over", ratio: overPaceRatio(utilization, expected) };
}

/** Ascending compare that is total over `Infinity` — `Infinity - Infinity`
 * would be `NaN`, which `Array.prototype.sort` silently reads as `0`. */
function ascending(x: number, y: number): number {
  if (x < y) return -1;
  if (x > y) return 1;
  return 0;
}

/**
 * Comparator for "best account" selection — the best account sorts first.
 *
 * **The rule is use-it-or-lose-it.** Unused weekly capacity expires at the
 * account's reset and does not roll over, so the account worth spending is the
 * one whose spare capacity is about to be lost — *not* the emptiest one, whose
 * runway is in no danger. (This reverses the earlier min-`usage_delta` rule,
 * which ranked "furthest under projected pace" first and therefore preferred
 * exactly the capacity that was in no danger of expiring.)
 *
 * Three levels, in order:
 *
 * 1. Exhausted accounts (out of tokens / rejected; see {@link
 *    isAccountExhausted}) always sort AFTER usable ones, no matter how
 *    favourable their pace key — a fully-used account whose window is nearly
 *    over still has nothing left to burn.
 * 2. The pace {@link PaceRank} **tier** ({@link PACE_TIER_INDEX}): under-pace
 *    (`usage_delta < 0`) before unknown (no usable pace signal) before
 *    over-pace (`usage_delta >= 0`).
 * 3. Only for two accounts in the *same* tier, that tier's own key in that
 *    tier's own direction:
 *    - **under-pace → `expected_utilization` DESCENDING.** The account
 *      furthest through its 7-day window wins, because its spare capacity is
 *      the capacity that expires first.
 *    - **unknown → raw `utilization` ascending** — least-used first, exactly
 *      how this population has always been ranked.
 *    - **over-pace → the RATIO `utilization / expected_utilization`,
 *      ASCENDING** — least-over *relative to its own pace*. A ratio, **not a
 *      difference**: a difference is not comparable across accounts at
 *      different points in their windows, since +5 points over at 10% expected
 *      is far more over-pace than +5 points over at 80% expected, yet a
 *      difference scores the two identically. Anything that reads only
 *      "least over first" is the comment a future edit would flatten back to
 *      `usage_delta`.
 *
 * Keys from different tiers are never compared — they measure different things.
 *
 * Mirrors the runner's `cmp_rank` (`ai_provider/config.rs`), consumed by
 * `pick_from` / `pick_target_from` (`ai_provider/account_usage.rs`); the two
 * implementations are documented mirrors and must change together.
 * Structurally typed so both `AccountUsageInfo` shapes (settings + the
 * terminal subset in `useSessionManager`) can use the one comparator.
 */
export function compareByUsageHeadroom(a: RankableAccount, b: RankableAccount): number {
  const ea = isAccountExhausted(a) ? 1 : 0;
  const eb = isAccountExhausted(b) ? 1 : 0;
  if (ea !== eb) return ea - eb;

  const ra = paceRank(a);
  const rb = paceRank(b);
  const ta = PACE_TIER_INDEX[ra.tier];
  const tb = PACE_TIER_INDEX[rb.tier];
  if (ta !== tb) return ta - tb;

  // Same tier only — each tier's own key, in that tier's own direction.
  if (ra.tier === "under" && rb.tier === "under") {
    // Highest expected wins: unused weekly capacity expires at the reset and
    // does not roll over, so burn the account whose window is furthest along.
    return ascending(rb.expected, ra.expected);
  }
  if (ra.tier === "unknown" && rb.tier === "unknown") {
    return ascending(ra.utilization, rb.utilization);
  }
  if (ra.tier === "over" && rb.tier === "over") {
    // Least-over RELATIVE to its own pace. A ratio, not a difference: +5
    // points over at 10% expected is far more over-pace than +5 points over
    // at 80% expected, yet a difference scores the two identically.
    return ascending(ra.ratio, rb.ratio);
  }
  // Unreachable: the tier indices are equal by the check above, so the three
  // arms are exhaustive. TypeScript cannot narrow `ra.tier === rb.tier` from
  // `ta === tb`, so the branch has to be spelled out.
  return 0;
}

export interface ClaudeApiSettings {
  model: string;
  max_tokens: number;
}

export interface GeminiCliSettings {
  execution_mode: CliExecutionMode;
  custom_path?: string;
  timeout_seconds: number;
  auth_method: GeminiAuthMethod;
  model: string;
}

export interface GeminiApiSettings {
  model: string;
  max_output_tokens: number;
  temperature: number;
}

export interface AiSettings {
  provider: AiProvider;
  claude_cli: ClaudeCliSettings;
  claude_api: ClaudeApiSettings;
  gemini_cli?: GeminiCliSettings;
  gemini_api?: GeminiApiSettings;
  /** Default iteration threshold for including video in auto-refine (0 = never) */
  auto_refine_video_after_iterations: number;
  /** Enable interactive bidirectional CLI sessions (stream-json protocol).
   * When true, sessions use multi-turn interactive mode with message queuing.
   * When false, sessions use one-shot inline mode. */
  interactive_sessions_enabled: boolean;
}

export interface AiConnectionTestResult {
  success: boolean;
  message: string;
  provider: string;
}

// Agentic Settings Types

export interface CompressionSettings {
  enabled: boolean;
  threshold_tokens: number;
  target_tokens: number;
  keep_recent_items: number;
  summarize_batch_size: number;
  tokens_per_char: number;
}

export interface RetrySettings {
  enabled: boolean;
  max_retries: number;
  base_delay_ms: number;
  max_delay_ms: number;
  exponential_base: number;
  jitter: boolean;
  feedback_injection: boolean;
  retryable_errors: string[];
}

export interface RoutingSettings {
  enabled: boolean;
  simple_model: string;
  medium_model: string;
  complex_model: string;
  file_count_thresholds: [number, number];
  prompt_length_thresholds: [number, number];
  complex_keywords: string[];
  simple_keywords: string[];
}

export interface AgenticSettings {
  compression: CompressionSettings;
  retry: RetrySettings;
  routing: RoutingSettings;
}

// Self-Healing Settings Types

/** LLM mode for self-healing operations */
export type SelfHealingLlmMode = "disabled" | "local_ollama" | "remote_api";

/** API provider for remote LLM in self-healing */
export type SelfHealingApiProvider = "open_ai" | "anthropic";

/** Settings for self-healing automation features */
export interface SelfHealingSettings {
  /** Enable action caching to avoid redundant operations */
  action_caching_enabled: boolean;
  /** Cache TTL in seconds (how long cached actions remain valid) */
  cache_ttl_seconds: number;
  /** Enable visual validation of actions */
  visual_validation_enabled: boolean;
  /** LLM mode for self-healing assistance */
  llm_mode: SelfHealingLlmMode;
  /** Ollama model name (used when llm_mode is LocalOllama) */
  ollama_model: string;
  /** API provider (used when llm_mode is RemoteApi) */
  api_provider: SelfHealingApiProvider;
}

// World State Verifier Settings Types

/** Tri-state mode selector for the VLM-based World State Verifier. */
export type WsvMode = "disabled" | "enabled" | "shadow";

/** Settings for the VLM-based World State Verifier (CUA-WSM / SEAgent judge). */
export interface WorldStateVerifierSettings {
  /** Tri-state mode: disabled, enabled (WSM wins), or shadow (log disagreements only). */
  mode: WsvMode;
  /** llama-swap (or compatible) endpoint base URL. */
  endpoint: string;
  /** Model alias or HuggingFace id to request. */
  model: string;
  /** When true, append pre/post screenshot thumbnails to agentic iteration canvas panels. */
  show_screenshot_evidence: boolean;
}

/** Result shape returned by the `test_wsv_connection` Tauri command. */
export interface WsvConnectionTestResult {
  ok: boolean;
  error: string | null;
  models_available: string[];
  latency_ms: number;
}

/** One row from the shadow-mode disagreements calibration log. */
export interface WsvDisagreementRow {
  id: number;
  task_run_id: string;
  iteration: number;
  text_status: string;
  wsm_status: string;
  text_confidence: number;
  wsm_confidence: number;
  intent: string;
  wsm_observations: string;
  created_at: string;
}
