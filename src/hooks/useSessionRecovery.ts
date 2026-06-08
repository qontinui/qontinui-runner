/**
 * useSessionRecovery Hook
 *
 * Subscribes to the one-shot `session-recovery-summary` Tauri event emitted by
 * the Rust backend at boot, AFTER `resume_ai_sessions` runs (Phase 4 of
 * `2026-06-06-runner-dev-loop-and-restart-resilience`).
 *
 * The backend classifies each boot as crash-recovery (the previous shutdown
 * was NOT clean — see `session::shutdown_marker`) vs a planned restart, and
 * threads per-session honesty out of the resume loop: whether each session
 * resumed losslessly (`--resume`) or from a lossy summary, whether its
 * worktree claim was re-acquired / degraded / peer-conflicted, and whether its
 * drained WIP was restored or kept for manual recovery.
 *
 * The {@link SessionRecoveryBanner} renders this. Mirrors the
 * `useSessionState` module-singleton pattern so a late-mounting banner still
 * catches the event (the summary is cached at module scope and replayed to
 * any subscriber that mounts after the event fired).
 */

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

/** Resume fidelity — mirrors the Rust `ResumeFidelity` (kebab-case). */
export type ResumeFidelity = "lossless" | "summary";

/** Worktree-claim re-acquire outcome — mirrors Rust `ClaimStatus`. */
export type ClaimStatus = "none" | "reacquired" | "peer-conflict" | "degraded";

/** Drained-WIP restore outcome — mirrors Rust `WipStatus`. */
export type WipStatus = "none" | "restored" | "kept-for-manual";

/**
 * Per-session recovery outcome — mirrors the Rust `SessionRecoveryOutcome`
 * struct (`#[serde(rename_all = "camelCase")]`).
 */
export interface SessionRecoveryOutcome {
  taskRunId: string;
  title: string;
  fidelity: ResumeFidelity;
  /** On a `summary` fidelity, the number of conversation turns covered. */
  turnsCovered: number;
  claimStatus: ClaimStatus;
  wipStatus: WipStatus;
}

/**
 * Startup-recovery summary payload — mirrors the Rust `SessionRecoverySummary`
 * struct emitted on `session-recovery-summary`.
 */
export interface SessionRecoverySummary {
  /** True when the previous shutdown was NOT clean (crash / hard kill). */
  crashRecovery: boolean;
  /** Number of sessions successfully resumed. */
  resumedCount: number;
  /** Per-session honest outcomes. */
  sessions: SessionRecoveryOutcome[];
}

/** The Tauri event name (mirrors `SESSION_RECOVERY_SUMMARY_EVENT` in Rust). */
export const SESSION_RECOVERY_SUMMARY_EVENT = "session-recovery-summary";

// Module-singleton cache so a banner that mounts AFTER the boot event still
// gets the summary (the event fires ~3s into startup; the terminal page may
// mount later).
let cachedSummary: SessionRecoverySummary | null = null;
const subscribers = new Set<(s: SessionRecoverySummary) => void>();
let listenerInitialized = false;

function ensureListener() {
  if (listenerInitialized) return;
  listenerInitialized = true;

  void listen<SessionRecoverySummary>(SESSION_RECOVERY_SUMMARY_EVENT, (event) => {
    const payload = event.payload;
    if (!payload || typeof payload !== "object") return;
    cachedSummary = payload;
    for (const fn of subscribers) {
      fn(payload);
    }
  });
}

/**
 * Subscribe to the startup recovery summary. Returns the latest summary (or
 * `null` before the boot event fires). The summary is replayed to a late
 * subscriber from the module cache.
 */
export function useSessionRecovery(): SessionRecoverySummary | null {
  const [summary, setSummary] = useState<SessionRecoverySummary | null>(cachedSummary);

  useEffect(() => {
    ensureListener();

    const handler = (s: SessionRecoverySummary) => setSummary(s);
    subscribers.add(handler);

    // Replay the cached summary if the event already fired before mount.
    if (cachedSummary) {
      setSummary(cachedSummary);
    }

    return () => {
      subscribers.delete(handler);
    };
  }, []);

  return summary;
}
