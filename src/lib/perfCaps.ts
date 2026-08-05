/**
 * Operator-tunable performance caps, mirrored from `settings.json` into a
 * module-level store (plan `2026-07-28-runner-many-sessions-performance`
 * Phase 8).
 *
 * The Rust struct is `settings::PerformanceSettings`, served by the
 * `get_performance_settings` / `save_performance_settings` Tauri commands.
 * This module is the frontend's single reader: consumers that need a cap
 * during render (the session-count advisory, the WebGL LRU) call
 * {@link getPerfCaps} synchronously and never await, because a cap must
 * never gate a first paint on IPC. Until the first load resolves, the
 * synchronous snapshot is {@link DEFAULT_PERF_CAPS} — which is byte-for-byte
 * the Rust defaults, so a slow or failed load behaves exactly like a stock
 * install rather than like "no caps".
 *
 * Store rather than context for the same reason `authOverlayStore` is one:
 * the consumers sit in unrelated trees (settings panel, terminal page,
 * per-pane backend construction) and a provider high enough to cover all
 * three would re-render the world on every save.
 */

import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createLogger } from "@/lib/logger";

const logger = createLogger("perfCaps");

/**
 * Mirror of Rust `settings::PerformanceSettings` (serde snake_case, no
 * rename attribute — the field names cross the IPC boundary as-is).
 */
export interface PerfCaps {
  /** Max panes holding a WebGL context at once, per window. */
  max_webgl_panes: number;
  /** Flush cadence (ms) for the `background` emission tier. */
  background_flush_interval_ms: number;
  /** Flush cadence (ms) for the `unwatched` tier; `0` = no webview emit. */
  unwatched_flush_interval_ms: number;
  /** Per-session scrollback ring capacity, bytes. */
  scrollback_capacity_bytes: number;
  /** Full-fleet grid-scan cadence, ms. */
  grid_scan_interval_ms: number;
  /** Default `Intent.share_output` for terminal sessions. */
  share_terminal_output: boolean;
  /** Explicit redaction flag; `null` follows `share_terminal_output`. */
  redact_terminal_secrets: boolean | null;
  /** Open-session count past which the advisory banner shows. */
  max_sessions_warn: number;
}

/**
 * The Rust defaults, duplicated here as the pre-load snapshot.
 *
 * Duplication is deliberate and is covered by a test that would have to be
 * updated in lockstep: the alternative is rendering with zeros (a WebGL cap
 * of 0 would disable acceleration entirely; a session threshold of 0 would
 * warn from the first tab) for the window before IPC resolves.
 */
export const DEFAULT_PERF_CAPS: Readonly<PerfCaps> = Object.freeze({
  max_webgl_panes: 8,
  background_flush_interval_ms: 250,
  unwatched_flush_interval_ms: 0,
  scrollback_capacity_bytes: 1_048_576,
  grid_scan_interval_ms: 1500,
  share_terminal_output: true,
  redact_terminal_secrets: null,
  max_sessions_warn: 30,
});

let caps: PerfCaps = { ...DEFAULT_PERF_CAPS };
const listeners = new Set<() => void>();
let loadPromise: Promise<PerfCaps> | null = null;

/**
 * Whether {@link getPerfCaps} is serving values the backend actually
 * confirmed. `"failed"` matters for the settings panel: the caps it shows are
 * then the compiled-in defaults, which may differ from what is on disk, and a
 * panel that presented those as the current configuration would be lying.
 */
export type PerfCapsLoadState = "unloaded" | "loaded" | "failed";
let loadState: PerfCapsLoadState = "unloaded";

function emit(): void {
  for (const l of [...listeners]) l();
}

/**
 * Normalize an IPC payload into a complete {@link PerfCaps}.
 *
 * Exported for tests, and defensive on purpose: a runner binary older than
 * the field it is asked for returns an object missing that key, and a
 * `NaN`/negative cap would be worse than the default. Anything unusable
 * falls back to the stock value FIELD BY FIELD, so one bad key never
 * discards the other seven.
 */
export function normalizePerfCaps(raw: unknown): PerfCaps {
  const src = (typeof raw === "object" && raw !== null ? raw : {}) as Record<string, unknown>;

  /*
   * `min` is 0 everywhere: the panel must display what is STORED, and the
   * runner applies its own floors at use (`effective_grid_scan_interval_ms`,
   * `effective_scrollback_capacity`). Re-deriving those floors here would make
   * the panel show a number the runner is not using — the exact dishonesty the
   * "floors apply at use" decision exists to avoid. Only values that are not
   * numbers at all, or are negative/NaN/Infinite, fall back.
   */
  const num = (key: keyof PerfCaps, fallback: number, min: number): number => {
    const v = src[key];
    if (typeof v !== "number" || !Number.isFinite(v) || v < min) return fallback;
    return v;
  };
  const bool = (key: keyof PerfCaps, fallback: boolean): boolean => {
    const v = src[key];
    return typeof v === "boolean" ? v : fallback;
  };

  return {
    max_webgl_panes: num("max_webgl_panes", DEFAULT_PERF_CAPS.max_webgl_panes, 0),
    background_flush_interval_ms: num(
      "background_flush_interval_ms",
      DEFAULT_PERF_CAPS.background_flush_interval_ms,
      0,
    ),
    unwatched_flush_interval_ms: num(
      "unwatched_flush_interval_ms",
      DEFAULT_PERF_CAPS.unwatched_flush_interval_ms,
      0,
    ),
    scrollback_capacity_bytes: num(
      "scrollback_capacity_bytes",
      DEFAULT_PERF_CAPS.scrollback_capacity_bytes,
      0,
    ),
    grid_scan_interval_ms: num("grid_scan_interval_ms", DEFAULT_PERF_CAPS.grid_scan_interval_ms, 0),
    share_terminal_output: bool("share_terminal_output", DEFAULT_PERF_CAPS.share_terminal_output),
    redact_terminal_secrets:
      typeof src.redact_terminal_secrets === "boolean" ? src.redact_terminal_secrets : null,
    max_sessions_warn: num("max_sessions_warn", DEFAULT_PERF_CAPS.max_sessions_warn, 0),
  };
}

/** The store half of the `useSyncExternalStore` contract. */
export function subscribePerfCaps(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/**
 * Current caps. Synchronous and always defined — the stock defaults until
 * {@link loadPerfCaps} resolves.
 */
export function getPerfCaps(): PerfCaps {
  return caps;
}

/**
 * Publish a new snapshot (the seam a successful save uses, and tests use).
 *
 * Publishing marks the store `"loaded"`: every caller has a backend-confirmed
 * value in hand — a save returns what the runner stored — so a panel that had
 * shown the "could not read current values" notice must stop showing it.
 */
export function setPerfCaps(next: PerfCaps): void {
  caps = next;
  loadState = "loaded";
  emit();
}

/**
 * Load the caps from the backend once per process. Repeat calls share the
 * first in-flight promise; a failure logs, keeps the defaults, and clears
 * the memo so a later call can retry (a cap the operator changed should not
 * be permanently unreadable because one early call raced app startup).
 */
export function loadPerfCaps(): Promise<PerfCaps> {
  if (loadPromise) return loadPromise;
  loadPromise = invoke<unknown>("get_performance_settings")
    .then((raw) => {
      const next = normalizePerfCaps(raw);
      loadState = "loaded";
      setPerfCaps(next);
      return next;
    })
    .catch((err) => {
      logger.warn("get_performance_settings failed; keeping default caps", err);
      loadPromise = null;
      loadState = "failed";
      // Republish with a fresh object identity. The VALUES are unchanged, so
      // `useSyncExternalStore`'s `Object.is` bail-out would otherwise swallow
      // this and a component rendering off the load state would never learn
      // the read failed.
      caps = { ...caps };
      emit();
      return caps;
    });
  return loadPromise;
}

/** Did the caps currently being served come from the backend? */
export function getPerfCapsLoadState(): PerfCapsLoadState {
  return loadState;
}

/** Test seam: forget the cached load and reset to stock defaults. */
export function __resetPerfCapsForTest(): void {
  caps = { ...DEFAULT_PERF_CAPS };
  loadPromise = null;
  loadState = "unloaded";
  listeners.clear();
}

/** React binding — re-renders the caller when the caps change. */
export function usePerfCaps(): PerfCaps {
  return useSyncExternalStore(subscribePerfCaps, getPerfCaps, getPerfCaps);
}
