/**
 * Phase 8 — Tier-3 free-text → registry action.
 *
 * Bridges the CommandBar to the Rust `command_interpret` Tauri command
 * (which spawns the local `claude` CLI as a one-shot subprocess; see
 * `src-tauri/src/commands/command_interpreter.rs` for the architectural
 * rationale and CLI flag set).
 *
 * The hot path:
 *
 *   1. {@link interpretCommand} is called with the operator's free
 *      text after Tier-1/2 misses.
 *   2. The L1 cache (LRU ~200, persisted to `instanceStorage`) is
 *      consulted with the normalized text as the key. Hits return
 *      instantly without subprocess cost.
 *   3. Misses build a compact tools catalog from the current registry
 *      snapshot and invoke the Rust command.
 *   4. The model's structured `{tool, args, confidence}` is resolved
 *      against the live registry. Unknown tool ids OR confidence below
 *      the gate return `null` so the CommandBar can fall back to fuzzy.
 *   5. Successful resolutions are written back to the cache.
 *
 * Cancellation: the caller passes an `AbortSignal` so a follow-up
 * keystroke (or pressing `Escape`) cleanly drops the in-flight call.
 * The Tauri layer doesn't surface its own abort hook, so the signal is
 * checked at JS boundaries — the subprocess on the Rust side runs to
 * completion in the background, but its result is discarded.
 *
 * Confidence gate: defaults to `0.4`. Below this the model is usually
 * guessing; surfacing it would offer suggestions that are worse than
 * the Tier-1 fuzzy fallback. The CommandBar's Tier-3 row also displays
 * the confidence so operators can sanity-check before pressing Enter.
 */

import { invoke } from "@tauri-apps/api/core";

import { instanceStorage } from "@/lib/instance-storage";

import { getById, getAll } from "./registry";
import type { CommandAction } from "./types";

const CACHE_STORAGE_KEY = "terminal-command-interpret-cache";
const CACHE_CAP = 200;
const DEFAULT_CONFIDENCE_GATE = 0.4;

/**
 * Mirror of the Rust `InterpretResult` struct
 * (`command_interpreter.rs`'s `InterpretResult`). Any field name change
 * on the Rust side has to mirror here — keep them in lockstep.
 */
interface RawInterpretResult {
  tool: string;
  args: Record<string, unknown>;
  confidence: number;
}

export interface InterpretMatch {
  action: CommandAction;
  args: Record<string, unknown>;
  /** Model's self-reported confidence ∈ [0, 1]. */
  confidence: number;
}

interface CachedEntry {
  tool: string;
  args: Record<string, unknown>;
  confidence: number;
  /** Last access epoch ms — drives LRU eviction. */
  lastUsed: number;
}

type Cache = Record<string, CachedEntry>;

/** Lowercase + trim + collapse internal whitespace. Strips a single
 *  leading `/` so slash and non-slash forms cache-share. */
export function normalizeInterpretKey(text: string): string {
  return text
    .replace(/^\//, "")
    .trim()
    .toLowerCase()
    .replace(/\s+/g, " ");
}

function readCache(): Cache {
  return instanceStorage.getJSON<Cache>(CACHE_STORAGE_KEY, {});
}

function writeCache(cache: Cache): void {
  instanceStorage.setJSON(CACHE_STORAGE_KEY, cache);
}

function getCached(key: string): CachedEntry | null {
  const cache = readCache();
  const entry = cache[key];
  if (!entry) return null;
  // Refresh LRU position. Cheap because writeCache is a single JSON
  // round-trip; we don't try to batch since hits are bounded by the
  // CommandBar's debounce.
  entry.lastUsed = Date.now();
  cache[key] = entry;
  writeCache(cache);
  return entry;
}

function setCached(key: string, entry: Omit<CachedEntry, "lastUsed">): void {
  const cache = readCache();
  cache[key] = { ...entry, lastUsed: Date.now() };
  // Evict least-recently-used entries down to the cap.
  const entries = Object.entries(cache);
  if (entries.length > CACHE_CAP) {
    entries.sort((a, b) => a[1].lastUsed - b[1].lastUsed);
    const toDrop = entries.length - CACHE_CAP;
    for (let i = 0; i < toDrop; i++) {
      delete cache[entries[i][0]];
    }
  }
  writeCache(cache);
}

/**
 * Build the JSON catalog that the model sees. Kept compact so the
 * prompt-token cost stays small — slash + label + description +
 * paramSchema is enough for the model to route; `aliases` and
 * `patterns` add no signal and inflate the prompt.
 */
function buildToolsJson(actions: readonly CommandAction[]): string {
  return JSON.stringify(
    actions.map((a) => ({
      id: a.id,
      slash: a.slash,
      label: a.label,
      description: a.description,
      paramSchema: a.paramSchema ?? {},
    })),
  );
}

export interface InterpretOptions {
  /** Aborted to drop an in-flight call. The subprocess on the Rust
   *  side runs to completion regardless; only the JS-side result is
   *  discarded. */
  signal?: AbortSignal;
  /** Confidence floor below which we return null. Defaults to
   *  {@link DEFAULT_CONFIDENCE_GATE}. */
  minConfidence?: number;
}

/**
 * Project a Rust-side {@link RawInterpretResult} onto the live registry,
 * returning `null` when the model's `tool` is unknown or empty.
 *
 * The registry can change between cache-write and cache-read (a tab
 * close that drops a per-tab action, for example), so the action
 * lookup happens at consumer time, not at cache time.
 */
function projectToMatch(raw: RawInterpretResult): InterpretMatch | null {
  if (!raw.tool) return null;
  const action = getById(raw.tool);
  if (!action) return null;
  return {
    action,
    args: raw.args ?? {},
    confidence: raw.confidence,
  };
}

/**
 * Try to resolve free-text to a registry action via the Tier-3
 * subprocess. Returns `null` when:
 *
 *   - text normalises to empty
 *   - the model returns confidence below {@link InterpretOptions.minConfidence}
 *   - the model returns an unknown / empty `tool`
 *   - the subprocess errors (any reason)
 *   - the signal aborts before / during the call
 *
 * Never throws — Tier-3 is fallback chrome, so a failure here just
 * lets Tier-1 fuzzy carry the response.
 */
export async function interpretCommand(
  text: string,
  options: InterpretOptions = {},
): Promise<InterpretMatch | null> {
  const { signal, minConfidence = DEFAULT_CONFIDENCE_GATE } = options;

  const key = normalizeInterpretKey(text);
  if (key.length === 0) return null;

  const cached = getCached(key);
  if (cached) {
    if (cached.confidence < minConfidence) return null;
    return projectToMatch(cached);
  }

  if (signal?.aborted) return null;

  const toolsJson = buildToolsJson(getAll());

  let raw: RawInterpretResult;
  try {
    raw = await invoke<RawInterpretResult>("command_interpret", {
      text,
      toolsJson,
    });
  } catch (err) {
    // Log to console; Tier-3 failure is silent at the UX level so the
    // CommandBar's fuzzy fallback isn't paved over by an error toast.
    // `console.warn` is allowed by the no-console rule (only `log` is
    // disallowed), so no eslint-disable is needed here.
    console.warn("[Tier-3] command_interpret failed:", err);
    return null;
  }

  if (signal?.aborted) return null;

  // Cache regardless of confidence so a low-confidence repeat doesn't
  // re-pay the subprocess cost. The gate is applied below at read time
  // too — getCached's gate check rejects sub-threshold entries.
  setCached(key, { tool: raw.tool, args: raw.args, confidence: raw.confidence });

  if (raw.confidence < minConfidence) return null;
  return projectToMatch(raw);
}
