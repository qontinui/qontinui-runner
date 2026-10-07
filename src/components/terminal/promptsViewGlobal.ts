import { useSyncExternalStore } from "react";
import { instanceStorage } from "@/lib/instance-storage";

/**
 * The Terminal page's GLOBAL prompts-view switch — one setting that turns the
 * operator's-prompts panel on or off for every session on every page tab.
 *
 * ## Default + overrides, invalidated by an epoch
 *
 * Each session can still show or hide its own panel (the zone header button,
 * the panel's X). Those per-session choices are stored per page as OVERRIDES
 * of the global default rather than as absolute open/closed states:
 *
 *     open(tab) = global.enabled XOR overrides.has(tab)
 *
 * Flipping the global switch bumps `epoch`. A page's override set carries the
 * epoch it was written under, and one written under an older epoch is
 * discarded on read — so "turn prompts on everywhere" really does reach every
 * session, including ones on pages that are not mounted right now, without
 * this module having to know which pages exist.
 *
 * The legacy per-page format (a bare `string[]` of open tabs, written before
 * this switch existed) reads as epoch 0 under a default of OFF, which is
 * exactly what it meant.
 */
export interface PromptsViewGlobal {
  enabled: boolean;
  epoch: number;
}

const STORAGE_KEY = "zone-prompts-view-global";
const DEFAULT: PromptsViewGlobal = { enabled: false, epoch: 0 };

// Storage is the ONLY copy. There is deliberately no module-level cache that
// outlives a read: a detached window that was not subscribed when another
// window flipped the switch would otherwise keep a stale value — and then
// reuse an epoch number, which would let stale per-session overrides survive
// a "show in all sessions". Reading on every snapshot also follows a runner
// whose API port (and so its storage namespace) is settled after import.
// `lastRaw` / `lastValue` exist only so `useSyncExternalStore` sees a stable
// object while the stored string is unchanged.
let lastRaw: string | null | undefined;
let lastValue: PromptsViewGlobal = DEFAULT;

function parse(raw: string | null): PromptsViewGlobal {
  if (!raw) return DEFAULT;
  try {
    const v = JSON.parse(raw) as Partial<PromptsViewGlobal> | null;
    if (!v || typeof v.enabled !== "boolean" || typeof v.epoch !== "number") return DEFAULT;
    return { enabled: v.enabled, epoch: v.epoch };
  } catch {
    return DEFAULT;
  }
}

export function getPromptsViewGlobal(): PromptsViewGlobal {
  let raw: string | null;
  try {
    raw = instanceStorage.getItem(STORAGE_KEY);
  } catch {
    // Storage blocked or absent (private mode, a non-DOM render): the
    // default is the honest reading — nothing was ever switched on here.
    raw = null;
  }
  if (raw !== lastRaw) {
    lastRaw = raw;
    lastValue = parse(raw);
  }
  return lastValue;
}

const listeners = new Set<() => void>();
const notify = () => {
  for (const l of listeners) l();
};

// Another window of the same runner instance (a detached page) flipping the
// switch arrives as a `storage` event. `useSyncExternalStore` re-reads the
// snapshot on notify and ignores it when nothing changed.
function subscribe(listener: () => void): () => void {
  if (listeners.size === 0 && typeof window !== "undefined") {
    window.addEventListener("storage", notify);
  }
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && typeof window !== "undefined") {
      window.removeEventListener("storage", notify);
    }
  };
}

/**
 * Show (or hide) the prompts view in every session on every page, clearing
 * every per-session override. Returns whether the switch was recorded —
 * `false` when storage refused the write, so nothing changed.
 *
 * The epoch is bumped even when `enabled` is unchanged: "turn it on
 * everywhere" while it is already the default still has to re-open the
 * sessions the operator closed one by one. It is bumped from the STORED value,
 * so two windows can never hand out the same epoch.
 */
export function setPromptsViewForAll(enabled: boolean): boolean {
  const prev = getPromptsViewGlobal();
  try {
    instanceStorage.setItem(STORAGE_KEY, JSON.stringify({ enabled, epoch: prev.epoch + 1 }));
  } catch {
    // Storage unavailable or over quota — the switch cannot be recorded.
    return false;
  }
  notify();
  return true;
}

export function usePromptsViewGlobal(): PromptsViewGlobal {
  return useSyncExternalStore(subscribe, getPromptsViewGlobal, getPromptsViewGlobal);
}

/** Per-page storage shape for the override set. */
export interface StoredPromptOverrides {
  epoch: number;
  tabs: string[];
}

/**
 * Decode a page's stored overrides against the current epoch. Anything
 * written under an older epoch — or the legacy bare array once the switch has
 * been used — is stale and reads as no overrides.
 */
export function decodePromptOverrides(raw: unknown, epoch: number): Set<string> {
  if (Array.isArray(raw)) {
    return epoch === 0 ? new Set(raw.filter((t): t is string => typeof t === "string")) : new Set();
  }
  if (raw && typeof raw === "object") {
    const r = raw as Partial<StoredPromptOverrides>;
    if (r.epoch === epoch && Array.isArray(r.tabs)) {
      return new Set(r.tabs.filter((t): t is string => typeof t === "string"));
    }
  }
  return new Set();
}
