import { getApiPort } from "./runner-api";

function namespacedKey(key: string): string {
  const port = getApiPort();
  return port === 9876 ? key : `${key}:${port}`;
}

/**
 * Whether raw `localStorage` key `rawKey` is one of THIS instance's keys under
 * `prefix`. A secondary instance stores `<key>:<port>`, so its keys must start
 * with the prefix AND end with its own port; the primary (9876) stores the bare
 * key, so a key carrying another instance's `:<port>` suffix is not its own.
 * (The old `includes(namespacedKey(prefix))` matched nothing on a secondary —
 * its namespaced prefix is `page:X::<port>` — and matched every instance's
 * keys on the primary.) Exported for tests.
 */
export function isOwnKeyUnderPrefix(rawKey: string, prefix: string, port: number): boolean {
  if (!rawKey.startsWith(prefix)) return false;
  if (port !== 9876) return rawKey.endsWith(`:${port}`);
  return !/:\d{4,5}$/.test(rawKey);
}

export const instanceStorage = {
  getItem(key: string): string | null {
    return localStorage.getItem(namespacedKey(key));
  },
  setItem(key: string, value: string): void {
    localStorage.setItem(namespacedKey(key), value);
  },
  removeItem(key: string): void {
    localStorage.removeItem(namespacedKey(key));
  },
  getJSON<T>(key: string, fallback: T): T {
    try {
      const raw = localStorage.getItem(namespacedKey(key));
      return raw ? JSON.parse(raw) : fallback;
    } catch {
      return fallback;
    }
  },
  setJSON(key: string, value: unknown): void {
    try {
      localStorage.setItem(namespacedKey(key), JSON.stringify(value));
    } catch {
      /* quota exceeded */
    }
  },
  /**
   * The raw `[key, value]` entries `removeByPrefix(prefix)` would remove (same
   * match), so a removal can be undone with {@link restoreEntries}.
   */
  entriesByPrefix(prefix: string): Array<[string, string]> {
    const port = getApiPort();
    const out: Array<[string, string]> = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && isOwnKeyUnderPrefix(key, prefix, port)) {
        const value = localStorage.getItem(key);
        if (value !== null) out.push([key, value]);
      }
    }
    return out;
  },
  /** Write back raw entries taken by {@link entriesByPrefix}. */
  restoreEntries(entries: ReadonlyArray<readonly [string, string]>): void {
    for (const [storageKey, value] of entries) {
      try {
        localStorage.setItem(storageKey, value);
      } catch {
        /* quota exceeded */
      }
    }
  },
  /** Remove all of this instance's keys under the given prefix (see {@link isOwnKeyUnderPrefix}). */
  removeByPrefix(prefix: string): void {
    const port = getApiPort();
    const keysToRemove: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && isOwnKeyUnderPrefix(key, prefix, port)) {
        keysToRemove.push(key);
      }
    }
    for (const key of keysToRemove) {
      localStorage.removeItem(key);
    }
  },
};
