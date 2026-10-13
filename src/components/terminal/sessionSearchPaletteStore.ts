/**
 * Open state of the session search palette, shared by its two openers — the
 * `Ctrl+Shift+Space` chord ({@link useSessionSearchHotkey}) and the
 * `/sessions.find` command — and the palette itself (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4).
 *
 * The store counts mounted palettes, so an opener can say honestly whether
 * anything was there to open: `/sessions.find` reports a failure rather than
 * "opened" when no palette is mounted.
 */

import { useEffect, useSyncExternalStore, type RefObject } from "react";

import { GLOBAL_CHORDS, matchesChord } from "@/lib/globalChords";
import { isSurfaceVisible } from "@/lib/surfaceVisible";

let open = false;
let mounted = 0;
const listeners = new Set<() => void>();

function emit(): void {
  for (const l of listeners) l();
}

export const sessionSearchPaletteStore = {
  isOpen(): boolean {
    return open;
  },
  setOpen(next: boolean): void {
    if (open === next) return;
    open = next;
    emit();
  },
  toggle(): void {
    sessionSearchPaletteStore.setOpen(!open);
  },
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  /** How many palettes are mounted to show the open state. */
  mountedCount(): number {
    return mounted;
  },
  /** Register a mounted palette; returns its unregister. */
  mount(): () => void {
    mounted++;
    return () => {
      mounted = Math.max(0, mounted - 1);
      if (mounted === 0) sessionSearchPaletteStore.setOpen(false);
    };
  },
};

/**
 * Open the palette. `false` when no palette is mounted, so the caller reports
 * that nothing opened instead of claiming it did.
 */
export function requestSessionSearchOpen(): boolean {
  if (mounted === 0) return false;
  sessionSearchPaletteStore.setOpen(true);
  return true;
}

/** The palette's open state, as React state. */
export function useSessionSearchPaletteOpen(): boolean {
  return useSyncExternalStore(
    sessionSearchPaletteStore.subscribe,
    sessionSearchPaletteStore.isOpen,
    sessionSearchPaletteStore.isOpen,
  );
}

/**
 * `Ctrl+Shift+Space` toggles the palette — while `surfaceRef`'s element is
 * visible, so the chord is inert on every page that does not show the
 * Session Manager's host (the Terminal page stays mounted, `display:none`,
 * behind the other tabs).
 */
export function useSessionSearchHotkey(surfaceRef: RefObject<Element | null>): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!matchesChord(e, GLOBAL_CHORDS.sessionSearch)) return;
      if (!isSurfaceVisible(surfaceRef.current)) return;
      e.preventDefault();
      sessionSearchPaletteStore.toggle();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [surfaceRef]);
}
