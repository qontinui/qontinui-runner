import { useEffect } from "react";

import { getApiBase } from "../lib/runner-api";

/**
 * useVisionCacheInvalidation — keep the runner's vision read-cache fresh on
 * event/poll-driven re-renders.
 *
 * The runner's `vision/extract` (OCR) read-cache keys on `vision_mutation_id`
 * and is only bumped by control actions (click/type) — see
 * `src-tauri/src/mcp/ui_bridge/vision_routes.rs`. A UI that re-renders from a
 * Tauri event or a data poll (rather than a control action) keeps serving the
 * FIRST capture's OCR blocks indefinitely, so verification reads go stale and
 * produce false verdicts (the StatusStrip teardown "still showing" false
 * positive — see plan 2026-06-05-ui-bridge-verification-read-freshness).
 *
 * This mounts a single document-wide `MutationObserver` that POSTs
 * `POST /ui-bridge/vision/mutation-occurred` (the existing
 * `vision_mutation_occurred_handler` → `bump_mutation_id`) whenever the DOM
 * changes from ANY source. The cache key then rotates and the next read
 * re-renders. The invariant becomes "the read-cache key changes iff the DOM
 * changed", regardless of what caused the change.
 *
 * The POST is debounced so a burst of mutations (animations, streaming text,
 * list re-renders) coalesces into a single bump after the DOM settles, and
 * never overlaps a previous in-flight bump. It is best-effort: a failed bump
 * only means a vision read may serve a slightly stale frame until the next
 * mutation, which `{"force":true}` can still override — it is not
 * correctness-critical for the app itself.
 */
export function useVisionCacheInvalidation(opts?: { debounceMs?: number }): void {
  const debounceMs = opts?.debounceMs ?? 250;

  useEffect(() => {
    if (typeof window === "undefined" || typeof MutationObserver === "undefined") {
      return;
    }

    let timer: ReturnType<typeof setTimeout> | null = null;
    let inFlight = false;
    let pending = false;

    const bump = async () => {
      // Coalesce: if a bump is already in flight, mark that another is owed
      // and let the in-flight one re-schedule on completion.
      if (inFlight) {
        pending = true;
        return;
      }
      inFlight = true;
      try {
        await fetch(`${getApiBase()}/ui-bridge/vision/mutation-occurred`, {
          method: "POST",
        });
      } catch {
        /* best-effort — cache freshness is an optimization, never block on it */
      } finally {
        inFlight = false;
        if (pending) {
          pending = false;
          schedule();
        }
      }
    };

    const schedule = () => {
      // THROTTLE, not debounce-reset. The previous implementation did
      // `clearTimeout(timer); timer = setTimeout(...)` on EVERY mutation
      // batch, so a high-frequency mutation source churned one setTimeout
      // per batch. Under sustained mutation (e.g. the UI Bridge
      // auto-register scanner stamping `data-ui-bridge-id` attributes, a
      // streaming text node, a re-render loop) that reached ~1,700
      // setTimeout/sec — each allocating a closure — which pegged the
      // WebView2 renderer's CPU and grew its heap ~150 MB/min until it
      // hard-OOM-crashed ("Error code: out of memory" → reload → sign-in).
      //
      // Throttling arms at most one timer per `debounceMs` window
      // regardless of mutation rate (so a 1,700/sec mutation storm yields
      // ~4 setTimeout/sec), while still bumping the vision cache once per
      // window during sustained mutation — which is actually a better
      // freshness guarantee than the old trailing debounce that never
      // fired at all while mutations kept arriving.
      if (timer) return;
      timer = setTimeout(() => {
        timer = null;
        void bump();
      }, debounceMs);
    };

    const observer = new MutationObserver(schedule);
    // Observe structural (childList) and text (characterData) changes —
    // what OCR actually re-reads. Deliberately NOT `attributes: true`:
    // arbitrary attribute churn (the bridge's own `data-ui-bridge-id`
    // stamps, `aria-*`/`data-*` live regions, class/style toggles) is a
    // high-frequency, largely-OCR-irrelevant trigger, and watching the
    // bridge's own attribute writes made this observer self-triggering.
    // Control actions still bump the cache directly, and a `{force:true}`
    // read overrides — cache freshness here is best-effort, not
    // correctness-critical (see the hook doc above).
    observer.observe(document.body, {
      childList: true,
      subtree: true,
      characterData: true,
    });

    return () => {
      if (timer) clearTimeout(timer);
      observer.disconnect();
    };
  }, [debounceMs]);
}
