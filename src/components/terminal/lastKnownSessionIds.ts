/**
 * Durable per-tab "last-known Claude session id" store.
 *
 * The `claudeSessionId` for a PTY-launched AI tab is captured best-effort by a
 * transcript poll (`useTabSessionIdCapture`). Two failure modes made tabs
 * unresumable at save time:
 *   1. The poll never landed (timed out) before the user closed the window.
 *   2. A live `tab` object transiently lost the id (state churn / remount).
 *
 * To make capture resilient we persist the last-known id per `tab.id` to
 * `instanceStorage` the moment it's observed, and let `saveSessionLayout`
 * fall back to it when the live tab object has no id. Best-effort: any storage
 * failure is swallowed (resumability is an enhancement, never load-bearing).
 *
 * Entries are pruned on read once older than {@link MAX_AGE_MS} so the map
 * can't grow unbounded across many sessions.
 */

import { instanceStorage } from "@/lib/instance-storage";

const STORAGE_KEY = "qontinui-terminal-last-known-session-ids";
const MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000; // 7 days

export interface LastKnownSessionId {
  claudeSessionId: string;
  claudeConfigDir?: string;
  savedAt: number;
}

type Store = Record<string, LastKnownSessionId>;

function readStore(): Store {
  return instanceStorage.getJSON<Store>(STORAGE_KEY, {});
}

function writeStore(store: Store): void {
  instanceStorage.setJSON(STORAGE_KEY, store);
}

/** Record (or refresh) the last-known session id for a tab. No-op for empty ids. */
export function rememberSessionId(
  tabId: string,
  claudeSessionId: string | undefined,
  claudeConfigDir: string | undefined,
): void {
  if (!claudeSessionId) return;
  try {
    const store = readStore();
    store[tabId] = { claudeSessionId, claudeConfigDir, savedAt: Date.now() };
    writeStore(store);
  } catch {
    // Best-effort only.
  }
}

/** Look up the last-known session id for a tab, or null if none / too old. */
export function recallSessionId(tabId: string): LastKnownSessionId | null {
  try {
    const store = readStore();
    const entry = store[tabId];
    if (!entry) return null;
    if (Date.now() - entry.savedAt >= MAX_AGE_MS) return null;
    return entry;
  } catch {
    return null;
  }
}

/** Drop entries older than {@link MAX_AGE_MS}. Best-effort housekeeping. */
export function pruneSessionIds(): void {
  try {
    const store = readStore();
    const now = Date.now();
    let changed = false;
    for (const [id, entry] of Object.entries(store)) {
      if (now - entry.savedAt >= MAX_AGE_MS) {
        delete store[id];
        changed = true;
      }
    }
    if (changed) writeStore(store);
  } catch {
    // Best-effort only.
  }
}
