import { getApiPort } from "./runner-api";

function namespacedKey(key: string): string {
  const port = getApiPort();
  return port === 9876 ? key : `${key}:${port}`;
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
    const nsPrefix = namespacedKey(prefix);
    const out: Array<[string, string]> = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.includes(nsPrefix)) {
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
  /** Remove all keys whose namespaced form contains the given prefix. */
  removeByPrefix(prefix: string): void {
    const nsPrefix = namespacedKey(prefix);
    const keysToRemove: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.includes(nsPrefix)) {
        keysToRemove.push(key);
      }
    }
    for (const key of keysToRemove) {
      localStorage.removeItem(key);
    }
  },
};
