/**
 * Post-spawn polling hook that fills in `tab.claudeSessionId` for
 * PTY-launched AI tabs (the `onLaunchAiSession` path in TerminalPage).
 *
 * FALLBACK ONLY since #548 Phase 1: runner launches pre-pin the id via
 * `--session-id` and never start a capture; this poll covers only un-pinned
 * tabs (custom launch commands, hand-typed `claude`), binds origin "guessed".
 *
 * Today, `useShellIntegration.ts:135` only sets `claudeSessionId` on
 * the resume path (`handleResumeSession`). For initial spawns, the
 * field is never set — which means `useCommitState`'s per-tab probe has
 * nothing to look up and the commit traffic light sits gray forever.
 *
 * This hook resolves §1.5 of the terminal-traffic-light plan: after a
 * tab is created and `writeWhenReady(tabId, "...claude\r")` has been
 * called, the consumer fires `startCapture(tabId, workingDir,
 * spawnTimestamp)`. The hook polls the existing `transcript_get_latest`
 * Tauri command (in `commands/transcript.rs:178`) every ~500 ms for up
 * to ~15 s. Freshness gating (last_modified > spawnTimestamp) is now
 * enforced backend-side via the `sinceMs` argument; if the backend
 * returns a non-null payload it's already known fresher than the
 * spawn. The frontend just checks the session id isn't already claimed
 * by another tab and then calls
 * `updateTab(tabId, { claudeSessionId, claudeConfigDir })`.
 *
 * Concurrency defense: a `claimedSessionIds` ref tracks every session id
 * we've already bound to a tab; the polling loop refuses to claim an
 * already-claimed id. This is the intra-tab dedup line of defense for
 * the case where two concurrent spawns into the same working directory
 * (and same config dir) race on a single JSONL — backend-side `sinceMs`
 * cannot disambiguate them.
 *
 * Diagnostics: set `localStorage["debug:tabSidCapture"] = "1"` to emit
 * `logger.debug("[tabSidCapture] <tag>", payload)` at every rejection
 * branch (`tab-not-found`, `already-bound`, `timed-out`, `probe-null`,
 * `already-claimed`, `probe-error`). Off by default; same affordance
 * pattern as `UI_BRIDGE_DEBUG_FIND`.
 *
 * On timeout the tab simply stays without a session id — the user can
 * still use the terminal, just no commit indicator.
 */

import { useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "./types";
import type { TerminalTab } from "./useTerminalManager";
import { createLogger } from "@/lib/logger";
import { rememberSessionId } from "./lastKnownSessionIds";

const logger = createLogger("TabSessionIdCapture");

interface TranscriptSessionPayload {
  session_id: string;
  config_dir: string;
  project_path: string;
  last_modified: string; // ISO 8601
}

const POLL_INTERVAL_MS = 500;
// Widened from 15s → 45s. Claude CLI can take a while to write its first
// transcript line under load (cold model spin-up, slow disk, busy runner), and
// a miss here makes the tab unresumable at the next save. The poll is cheap
// (one `transcript_get_latest` IPC per tick) and self-cancels the moment the id
// is bound, so a longer ceiling costs nothing in the common (fast) case.
const POLL_TIMEOUT_MS = 45_000;

interface CaptureParams {
  updateTab: (
    id: string,
    updates: Partial<
      Pick<
        TerminalTab,
        "isAlive" | "exitCode" | "workingDir" | "claudeSessionId" | "claudeConfigDir"
      >
    >,
  ) => void;
  /** Used to skip capture for tabs that already have a session id
   *  (e.g. resume path beat us to it) and for diagnostics. */
  tabs: TerminalTab[];
  /**
   * Fired the instant a `claudeSessionId` is bound to a tab — the durable
   * session-registry OPEN hook. The hook itself stays zone-agnostic; the
   * consumer (TerminalPage) resolves the tab's zone and records the OPEN
   * synchronously (NOT via the debounced localStorage snapshot). Optional
   * `configDir` is the Claude config dir captured from the transcript.
   */
  onSessionIdBound?: (tabId: string, claudeSessionId: string, configDir?: string) => void;
}

interface UseTabSessionIdCaptureResult {
  /**
   * Begin polling `transcript_get_latest` for `tabId`. Idempotent —
   * a second call for the same tabId is a no-op while a poll is
   * already in flight.
   *
   * `configDir` (optional) — when the tab was launched into a
   * specific Claude config dir (e.g. via `onLaunchAiSession`'s
   * per-account config), pass it through so the backend filters
   * to that account. Without it, a concurrent Claude session in a
   * *different* account writing to the same `projectPath` can win
   * the freshest-mtime race and the wrong session_id gets bound.
   * P0 silent-fail if omitted; multi-account users are the common case.
   */
  startCapture: (
    tabId: string,
    workingDir: string,
    spawnTimestamp: number,
    configDir?: string,
  ) => void;
}

export function useTabSessionIdCapture({
  updateTab,
  tabs,
  onSessionIdBound,
}: CaptureParams): UseTabSessionIdCaptureResult {
  // Keep the latest callback in a ref so `startCapture`'s identity stays
  // stable (it only depends on `updateTab`); the bind site reads the live
  // callback without re-creating the polling closure on every render.
  const onSessionIdBoundRef = useRef(onSessionIdBound);
  useEffect(() => {
    onSessionIdBoundRef.current = onSessionIdBound;
  }, [onSessionIdBound]);
  // Tabs reference: kept fresh so `startCapture` can read the latest
  // `claudeSessionId` and skip already-bound tabs.
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);

  // Session ids already bound to *some* tab — used to refuse a duplicate
  // claim if two tabs spawn into the same workdir before the first
  // JSONL hits disk.
  const claimedSessionIds = useRef<Set<string>>(new Set());

  // Hydrate the claimed set on mount and on every `tabs` change so we
  // never re-claim a session id that's already bound to a tab (resume
  // path, prior session, etc.).
  useEffect(() => {
    for (const tab of tabs) {
      if (tab.claudeSessionId) {
        claimedSessionIds.current.add(tab.claudeSessionId);
      }
    }
  }, [tabs]);

  // Active poll bookkeeping — keyed by tabId so a tab close cancels its
  // own loop without affecting siblings, and a duplicate startCapture
  // for the same tabId is a no-op.
  const inFlight = useRef<Map<string, { cancelled: boolean }>>(new Map());

  // Cancel all in-flight polls on unmount.
  useEffect(() => {
    const map = inFlight.current;
    return () => {
      for (const handle of map.values()) {
        handle.cancelled = true;
      }
      map.clear();
    };
  }, []);

  const startCapture = useCallback<UseTabSessionIdCaptureResult["startCapture"]>(
    // workingDir is best-effort — the per-terminal cwd is set by the
    // shell-integration `cwd` event, which may not have fired yet at
    // capture-start time. Empty string is treated as "let the backend
    // fall back to its workspace default".
    (tabId, workingDir, spawnTimestamp, configDir) => {
      // Re-read on every emission so tests that stub localStorage at
      // arbitrary points still get the latest value. Production cost
      // is a single getItem per debug call site.
      const debug = (): boolean =>
        typeof localStorage !== "undefined" && localStorage.getItem("debug:tabSidCapture") === "1";

      if (debug()) {
        logger.debug("[tabSidCapture] start", { tabId, workingDir, configDir, spawnTimestamp });
      }

      // Already polling this tab? Don't double-up.
      if (inFlight.current.has(tabId)) return;
      const handle = { cancelled: false };
      inFlight.current.set(tabId, handle);

      const startedAt = Date.now();

      const tick = async (): Promise<void> => {
        if (handle.cancelled) return;

        // Already-bound? Stop. (Resume path may have set it before us.)
        const tab = tabsRef.current.find((t) => t.id === tabId);
        if (!tab) {
          // Tab was closed — stop.
          if (debug()) logger.debug("[tabSidCapture] tab-not-found", { tabId });
          handle.cancelled = true;
          inFlight.current.delete(tabId);
          return;
        }
        if (tab.claudeSessionId) {
          if (debug()) {
            logger.debug("[tabSidCapture] already-bound", {
              tabId,
              sessionId: tab.claudeSessionId,
            });
          }
          handle.cancelled = true;
          inFlight.current.delete(tabId);
          return;
        }

        // Timed out?
        if (Date.now() - startedAt > POLL_TIMEOUT_MS) {
          if (debug()) {
            logger.debug("[tabSidCapture] timed-out", {
              tabId,
              elapsedMs: Date.now() - startedAt,
            });
          }
          handle.cancelled = true;
          inFlight.current.delete(tabId);
          return;
        }

        try {
          // Pass workingDir + configDir when known, plus sinceMs so the
          // backend can drop pre-spawn JSONLs before answering. Both
          // workingDir/configDir fall back server-side: empty workingDir
          // → workspace project path; missing configDir → scan all known
          // config dirs (the bug path that motivated adding configDir —
          // see hook docstring).
          const args: Record<string, string | number> = { sinceMs: spawnTimestamp };
          if (workingDir) args.projectPath = workingDir;
          if (configDir) args.configDir = configDir;
          const resp = await invoke<CommandResponse>("transcript_get_latest", args);
          if (handle.cancelled) return;

          const data = resp.success ? (resp.data as TranscriptSessionPayload | null) : null;
          if (!data) {
            if (debug()) logger.debug("[tabSidCapture] probe-null", { tabId });
          } else if (claimedSessionIds.current.has(data.session_id)) {
            if (debug()) {
              logger.debug("[tabSidCapture] already-claimed", {
                tabId,
                sessionId: data.session_id,
              });
            }
          } else {
            claimedSessionIds.current.add(data.session_id);
            updateTab(tabId, {
              claudeSessionId: data.session_id,
              claudeConfigDir: data.config_dir,
            });
            // Persist durably so a later state churn / remount that drops the
            // live id off the tab object can still be recovered at save time.
            rememberSessionId(tabId, data.session_id, data.config_dir);
            // Record the OPEN in the durable backend session registry the
            // instant the id is known. Zone-agnostic here — the consumer
            // resolves the zone and fires the synchronous record.
            onSessionIdBoundRef.current?.(tabId, data.session_id, data.config_dir);
            handle.cancelled = true;
            inFlight.current.delete(tabId);
            return;
          }
        } catch (err) {
          // Probe error: keep trying; runner may be transiently busy.
          if (debug()) logger.debug("[tabSidCapture] probe-error", { tabId, err });
        }

        // Schedule next tick.
        setTimeout(() => {
          void tick();
        }, POLL_INTERVAL_MS);
      };

      void tick();
    },
    [updateTab],
  );

  return { startCapture };
}
