import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

/**
 * Half a viewport of overscan in every direction: a cell up to half a screen
 * away from the scroll container is already "near", so it cold-mounts BEFORE
 * it scrolls into view and the operator never sees an empty tile.
 *
 * This is an `IntersectionObserver` rootMargin, so the live band is
 * viewport + 2×margin — 2× the viewport at 50%, down from 3× at the original
 * 100% (plan `2026-07-28-runner-many-sessions-performance` Phase 2 / A9).
 * Halving it trims the live-pane count by roughly a third; the hard limit on
 * GPU contexts is the WebGL LRU (`backends/webglContextLru.ts`), which caps
 * them regardless of how many panes are mounted.
 */
const OVERSCAN_ROOT_MARGIN = "50% 0px";

/**
 * Coalesce window for observer callbacks. Scrolling fires a burst of
 * intersection ticks; batching them into one `setState` avoids thrashing the
 * (relatively expensive) `classifyTabs` memo on every tick. The very first
 * batch flushes immediately (delay 0) so entering flow mode mounts the initial
 * on-screen zones without a visible compact-card flash.
 */
const COALESCE_MS = 120;

/**
 * Reusable zone-cell DOM registry + viewport-proximity signal that drives
 * flow-grid virtualization (Phase 3).
 *
 * Each zone cell registers its outer DOM node keyed by `tabId` via
 * {@link ZoneVirtualization.registerCell}. An `IntersectionObserver` rooted at
 * the flow-grid scroll container watches every registered cell and publishes
 * the set of tab-ids currently near the viewport. `classifyTabs` turns that set
 * into `assigned-live` (mount a real `TerminalInstance`) vs `assigned-virtual`
 * (render only a `CompactZoneCard`).
 *
 * The registry is deliberately generic — Phase 4 (scroll routing) reuses
 * `cellFor(tabId)` to `scrollIntoView()` a focused / attention-flagged session's
 * cell. Keying by `tabId` (not zone index) means the lookup survives zone
 * reassignment and matches how attention paths address sessions.
 *
 * Defensive by design: when `IntersectionObserver` is unavailable (jsdom / SSR)
 * or the layout is not flow mode, the near set stays empty of observer-driven
 * churn and callers fall back to the all-mounted preset behavior.
 */
export interface ZoneVirtualization {
  /** Tab-ids whose cell is at/near the viewport (identity changes on update). */
  nearViewport: Set<string>;
  /** Register (or re-register) a zone cell's DOM node for a tab id. */
  registerCell: (tabId: string, el: HTMLElement) => void;
  /** Unregister a tab's cell (on unmount or reassignment). */
  unregisterCell: (tabId: string) => void;
  /** Phase 4 hook: resolve a tab id to its live cell node, if mounted. */
  cellFor: (tabId: string) => HTMLElement | undefined;
}

export function useZoneVirtualization(
  rootRef: RefObject<HTMLElement | null>,
  isFlowMode: boolean,
): ZoneVirtualization {
  const [nearViewport, setNearViewport] = useState<Set<string>>(() => new Set());

  // Mutable working set updated synchronously from observer callbacks; published
  // to React state (a fresh Set) on a coalesced flush so the classification memo
  // recomputes at most once per burst.
  const nearRef = useRef<Set<string>>(new Set());
  const tabToEl = useRef<Map<string, HTMLElement>>(new Map());
  const elToTab = useRef<Map<Element, string>>(new Map());
  const observerRef = useRef<IntersectionObserver | null>(null);
  const flushTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const dirtyRef = useRef(false);
  const firstFlushDoneRef = useRef(false);

  const scheduleFlush = useCallback(() => {
    if (flushTimer.current) return;
    const delay = firstFlushDoneRef.current ? COALESCE_MS : 0;
    flushTimer.current = setTimeout(() => {
      flushTimer.current = null;
      if (!dirtyRef.current) return;
      dirtyRef.current = false;
      firstFlushDoneRef.current = true;
      setNearViewport(new Set(nearRef.current));
    }, delay);
  }, []);

  const registerCell = useCallback((tabId: string, el: HTMLElement) => {
    const prev = tabToEl.current.get(tabId);
    if (prev === el) return;
    if (prev) {
      elToTab.current.delete(prev);
      observerRef.current?.unobserve(prev);
    }
    tabToEl.current.set(tabId, el);
    elToTab.current.set(el, tabId);
    observerRef.current?.observe(el);
  }, []);

  const unregisterCell = useCallback(
    (tabId: string) => {
      const el = tabToEl.current.get(tabId);
      if (!el) return;
      tabToEl.current.delete(tabId);
      elToTab.current.delete(el);
      observerRef.current?.unobserve(el);
      if (nearRef.current.delete(tabId)) {
        dirtyRef.current = true;
        scheduleFlush();
      }
    },
    [scheduleFlush],
  );

  const cellFor = useCallback((tabId: string) => tabToEl.current.get(tabId), []);

  // (Re)build the observer when flow mode toggles. Leaving flow mode tears the
  // observer down and clears the near set so classification returns to the
  // all-mounted preset behavior.
  useEffect(() => {
    observerRef.current?.disconnect();
    observerRef.current = null;

    if (!isFlowMode || typeof IntersectionObserver === "undefined") {
      if (nearRef.current.size) {
        nearRef.current = new Set();
        firstFlushDoneRef.current = false;
        setNearViewport(new Set());
      }
      return;
    }

    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const tabId = elToTab.current.get(entry.target);
          if (!tabId) continue;
          if (entry.isIntersecting) {
            if (!nearRef.current.has(tabId)) {
              nearRef.current.add(tabId);
              dirtyRef.current = true;
            }
          } else if (nearRef.current.delete(tabId)) {
            dirtyRef.current = true;
          }
        }
        if (dirtyRef.current) scheduleFlush();
      },
      { root: rootRef.current ?? null, rootMargin: OVERSCAN_ROOT_MARGIN, threshold: 0 },
    );
    observerRef.current = observer;
    // Pick up cells that registered before the observer existed (callback refs
    // run during commit, this effect runs after).
    for (const el of tabToEl.current.values()) observer.observe(el);

    return () => {
      observer.disconnect();
      if (observerRef.current === observer) observerRef.current = null;
    };
    // rootRef is a stable ref object; re-run only on flow-mode change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isFlowMode, scheduleFlush]);

  useEffect(() => {
    return () => {
      if (flushTimer.current) clearTimeout(flushTimer.current);
    };
  }, []);

  return { nearViewport, registerCell, unregisterCell, cellFor };
}
