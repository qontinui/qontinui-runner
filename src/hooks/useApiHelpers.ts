/**
 * Shared API fetch helper for evaluation hooks.
 */

import { getApiBase } from "@/lib/runner-api";

export async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${getApiBase()}${path}`, {
    headers: { "Content-Type": "application/json", ...(init?.headers as Record<string, string>) },
    ...init,
  });
  if (!res.ok) {
    const text = await res.text().catch(() => res.statusText);
    throw new Error(`API ${res.status}: ${text}`);
  }
  return res.json() as Promise<T>;
}
