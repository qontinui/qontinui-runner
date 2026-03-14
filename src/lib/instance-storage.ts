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
    } catch { /* quota exceeded */ }
  },
};
