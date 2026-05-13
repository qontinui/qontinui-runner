/**
 * useGitSupervision — Phase 1 of the D5 Git Supervision Channel
 *
 * Listens for `git-supervision` events emitted by the Rust trigger system
 * (see plan: `plans/2026-05-13-d5-git-supervision-channel-phase-1.md`),
 * maintains a small bounded in-memory ring (last 256 events), and exposes
 * a derived `proposalsByStateId` map for the state-machine compiler.
 *
 * The compiler reads `proposalsByStateId` as its 4th argument; states
 * named in the map are tagged `provenance: "git-supervised"` when they
 * would otherwise have fallen back to `ai-generated` (see
 * `compile-state-machine.ts` `choosePromotion` priority 2.5).
 *
 * Phase 1 scope notes:
 * - In-memory only; no persistence, no replay on restart.
 * - State-ID inference is best-effort: we use `payload.specId` if the
 *   Rust side surfaces it, otherwise we try to match the event's `path`
 *   against the caller-provided `knownStateIds` set. Events with no
 *   inferred state ID still live in `recentEvents` for the debug surface.
 * - Forward-compat: unknown event kinds are still buffered so the debug
 *   panel can surface them; only known kinds populate proposals.
 */

import { useEffect, useState, useMemo } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { GitProposalState } from "@/lib/compile-state-machine";

// ---------------------------------------------------------------------------
// Event shape (contract with Rust side)
// ---------------------------------------------------------------------------

/**
 * Wire shape of a single git-supervision event.
 *
 * The Rust struct uses `#[serde(rename_all = "camelCase")]`, so all
 * field names are already camelCase on the JS side. This contract is
 * mirrored on the Rust side in `src-tauri/src/git_supervision/` (or the
 * equivalent trigger-system fan-out per §3.1 of the plan).
 */
export interface GitSupervisionEvent {
  /** Unique event ID (Rust-generated, e.g. UUID). */
  id: string;
  /** Discriminator. Unknown values are still buffered for the debug surface. */
  kind:
    | "commit"
    | "branch_switch"
    | "tag"
    | "spec_changed"
    | "working_tree_drift"
    | string;
  /**
   * Event-kind-specific payload. Per the plan §3.2 the shape varies; this
   * hook treats payloads opaquely except for state-ID inference, which
   * looks at `specId` (preferred) and `path` (fallback).
   */
  payload: Record<string, unknown>;
  /** Unix millis timestamp (Rust side emits `chrono::Utc::now().timestamp_millis()`). */
  timestamp: number;
  /**
   * Always "git-supervised" for Phase 1; reserved for future channels
   * (e.g. "ci-supervised") that may share the bus.
   */
  provenance: string;
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/** Maximum number of events retained in the in-memory ring. */
export const MAX_EVENTS = 256;

export interface UseGitSupervisionOptions {
  /**
   * Optional set of state IDs known to the caller (typically derived from
   * the loaded specs). When provided, the path-based inference fallback
   * matches event paths against this set to recover a state ID. Without
   * it, only events that carry an explicit `payload.specId` are tied to
   * a state.
   */
  knownStateIds?: ReadonlySet<string>;
}

export interface UseGitSupervisionReturn {
  /** Most recent events first. Bounded to `MAX_EVENTS`. */
  recentEvents: GitSupervisionEvent[];
  /** Map of compiled-state-id → proposal metadata for the compiler. */
  proposalsByStateId: GitProposalState;
}

/**
 * Best-effort state-ID inference for an event.
 *
 * Order of preference (Phase 1 MVP):
 *   1. `payload.specId` (string)        — explicit Rust-side mapping
 *   2. `payload.stateId` (string)       — alternative explicit field
 *   3. `payload.path` (string)          — match terminal segment against
 *                                         `knownStateIds`
 *
 * Returns `undefined` if no inference succeeds; the event is still kept
 * in the ring but doesn't contribute to `proposalsByStateId`.
 */
function inferStateId(
  event: GitSupervisionEvent,
  knownStateIds: ReadonlySet<string> | undefined,
): string | undefined {
  const p = event.payload;
  const specId = p.specId;
  if (typeof specId === "string" && specId.length > 0) return specId;

  const stateId = p.stateId;
  if (typeof stateId === "string" && stateId.length > 0) return stateId;

  if (!knownStateIds || knownStateIds.size === 0) return undefined;

  // Candidate paths to try, in order. `file_watcher.rs` emits
  // `payload.files: string[]` (plural); `git_watcher.rs` puts touched
  // paths under `payload.changed_files[].path`. Singular `payload.path`
  // is the simplest forward-compat shape for future emitters.
  const candidates: string[] = [];
  if (typeof p.path === "string") candidates.push(p.path);
  if (Array.isArray(p.files)) {
    for (const f of p.files) if (typeof f === "string") candidates.push(f);
  }
  if (Array.isArray(p.changedFiles)) {
    for (const cf of p.changedFiles) {
      if (cf && typeof cf === "object" && "path" in (cf as object)) {
        const v = (cf as { path?: unknown }).path;
        if (typeof v === "string") candidates.push(v);
      }
    }
  }

  for (const path of candidates) {
    if (path.length === 0) continue;
    // Try the full path first (an event path might literally equal a
    // known state ID).
    if (knownStateIds.has(path)) return path;

    // Match the basename without extension. Spec paths look like
    // `specs/pages/<slug>/spec.uibridge.json`, so try the parent-dir
    // name first (typical convention) and the basename second.
    const parts = path.split(/[\\/]/).filter((s) => s.length > 0);
    if (parts.length === 0) continue;
    const basename = parts[parts.length - 1]?.replace(/\.[^.]+$/, "");
    if (basename && knownStateIds.has(basename)) return basename;
    if (parts.length >= 2) {
      const parent = parts[parts.length - 2];
      if (parent && knownStateIds.has(parent)) return parent;
    }
  }
  return undefined;
}

/**
 * Subscribe to git-supervision events and expose the bounded ring +
 * derived proposal map.
 *
 * Implementation notes:
 * - The Tauri listener is bound exactly once (empty-dep `useEffect`).
 *   `knownStateIds` flows into `proposalsByStateId` via the memo, so
 *   spec-loaded-set changes recompute the map without churning the
 *   underlying subscription.
 * - The state-update path is intentionally a full ring replacement on
 *   every event — at 256 entries this is cheap and avoids the foot-guns
 *   of in-place mutation with React.
 * - `proposalsByStateId` is a `useMemo` over `recentEvents` and
 *   `knownStateIds`, so the compiler sees an updated map whenever a new
 *   event lands or the loaded spec set changes.
 */
export function useGitSupervision(
  options: UseGitSupervisionOptions = {},
): UseGitSupervisionReturn {
  const [recentEvents, setRecentEvents] = useState<GitSupervisionEvent[]>([]);
  const { knownStateIds } = options;

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    let cancelled = false;

    const setup = async () => {
      try {
        unlisten = await listen<GitSupervisionEvent>("git-supervision", (event) => {
          const ev = event.payload;
          if (!ev || typeof ev !== "object") return;
          setRecentEvents((prev) => {
            const next = [ev, ...prev];
            if (next.length > MAX_EVENTS) next.length = MAX_EVENTS;
            return next;
          });
        });
        if (cancelled && unlisten) {
          unlisten();
          unlisten = null;
        }
      } catch (err) {
        // Tauri may be unavailable in browser dev mode — silent degrade.
        // Logged at warn rather than error so it shows in dev tools but
        // doesn't surface as a runtime error.
        console.warn("[useGitSupervision] failed to subscribe to git-supervision events:", err);
      }
    };

    void setup();

    return () => {
      cancelled = true;
      if (unlisten) {
        unlisten();
        unlisten = null;
      }
    };
  }, []);

  const proposalsByStateId = useMemo<GitProposalState>(() => {
    const out: GitProposalState = {};
    // Iterate from oldest → newest so the newest proposal wins on
    // duplicate state IDs (later writes overwrite earlier ones in the
    // map). `recentEvents` is newest-first, hence the reverse-walk.
    for (let i = recentEvents.length - 1; i >= 0; i--) {
      const ev = recentEvents[i];
      const stateId = inferStateId(ev, knownStateIds);
      if (!stateId) continue;
      out[stateId] = {
        proposedAt: new Date(ev.timestamp).toISOString(),
        kind: ev.kind,
      };
    }
    return out;
  }, [recentEvents, knownStateIds]);

  return { recentEvents, proposalsByStateId };
}

// ---------------------------------------------------------------------------
// Test helper — exported for unit tests that want to exercise the inference
// logic without a real Tauri event bus. Not part of the public hook API.
// ---------------------------------------------------------------------------

export const __testables = {
  inferStateId,
};
