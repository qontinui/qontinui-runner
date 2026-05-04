/**
 * use-discovered-specs.ts
 *
 * Runtime spec loader (Section 13). Replaces the build-time
 * `getAllSpecs()` registry with a fetch from the runner's Spec API
 * (`GET http://localhost:9876/spec/list`), with module-singleton
 * caching and automatic SSE-driven invalidation on `spec.changed`.
 *
 * Two entry points:
 *   - `loadDiscoveredSpecs()` — async loader for non-React contexts
 *     (App.tsx pre-mount, relay handlers, etc.).
 *   - `useDiscoveredSpecs()` — React hook for components.
 *
 * Both share the same module-scoped cache so eight call sites
 * fanning out to eight HTTP fetches per page render is avoided.
 */

import { useEffect, useState } from "react";
import type { DiscoveredSpec } from "../spec-prompt-builder";

const SPEC_LIST_URL = "http://localhost:9876/spec/list";
const SPEC_SUBSCRIBE_URL = "http://localhost:9876/spec/subscribe";

// =============================================================================
// Module-scoped state
// =============================================================================

let cachedSpecs: DiscoveredSpec[] | null = null;
let lastError: Error | null = null;
let inFlight: Promise<DiscoveredSpec[]> | null = null;
let sseInitialized = false;
let eventSource: EventSource | null = null;

const subscribers = new Set<() => void>();

function notifySubscribers(): void {
  for (const fn of subscribers) {
    try {
      fn();
    } catch {
      // A misbehaving subscriber must not break siblings.
    }
  }
}

// =============================================================================
// SSE — lazy-init on first call to either entry point
// =============================================================================

function initSseOnce(): void {
  if (sseInitialized) return;
  sseInitialized = true;

  if (typeof window === "undefined" || typeof EventSource === "undefined") {
    // Non-browser environment — skip cleanly. The cache simply won't
    // auto-invalidate. Explicit refresh() still works.
    return;
  }

  try {
    eventSource = new EventSource(SPEC_SUBSCRIBE_URL);
    eventSource.addEventListener("spec.changed", () => {
      // Invalidate the cache and refetch in the background. Subscribers
      // are notified twice: once when the cache clears (so consumers see
      // loading), and once when the refetch resolves.
      cachedSpecs = null;
      inFlight = null;
      notifySubscribers();
      void loadDiscoveredSpecs().catch(() => {
        // Errors are captured into `lastError` by the loader.
      });
    });
    eventSource.onerror = () => {
      // EventSource auto-reconnects; nothing to do. Don't log to avoid
      // console spam when the runner Spec API is offline.
    };
  } catch {
    // Defensive: if construction fails, leave `sseInitialized = true`
    // so we don't retry on every call.
    eventSource = null;
  }
}

// =============================================================================
// Async loader (non-React)
// =============================================================================

interface SpecListResponse {
  ok: boolean;
  specs?: DiscoveredSpec[];
  reason?: string;
}

async function fetchSpecs(): Promise<DiscoveredSpec[]> {
  const response = await fetch(SPEC_LIST_URL, {
    method: "GET",
    headers: { Accept: "application/json" },
  });

  if (!response.ok) {
    throw new Error(
      `GET /spec/list failed: HTTP ${response.status} ${response.statusText}`
    );
  }

  const body = (await response.json()) as SpecListResponse;
  if (!body.ok) {
    throw new Error(
      `GET /spec/list returned ok=false${body.reason ? `: ${body.reason}` : ""}`
    );
  }

  return body.specs ?? [];
}

export async function loadDiscoveredSpecs(): Promise<DiscoveredSpec[]> {
  initSseOnce();

  if (cachedSpecs !== null) {
    return cachedSpecs;
  }
  if (inFlight !== null) {
    return inFlight;
  }

  const promise = fetchSpecs()
    .then((specs) => {
      cachedSpecs = specs;
      lastError = null;
      inFlight = null;
      notifySubscribers();
      return specs;
    })
    .catch((err: unknown) => {
      lastError = err instanceof Error ? err : new Error(String(err));
      inFlight = null;
      notifySubscribers();
      // Keep any previously cached array intact. Re-throw so callers see
      // the failure on first load; subsequent reads see the cache.
      throw lastError;
    });

  inFlight = promise;
  return promise;
}

/**
 * Single-spec async accessor for non-React contexts. Reuses the
 * module-singleton cache via `loadDiscoveredSpecs()`. Resolves to `null`
 * if no spec with the given id is loaded.
 */
export async function loadDiscoveredSpec(
  id: string
): Promise<DiscoveredSpec | null> {
  const specs = await loadDiscoveredSpecs();
  return specs.find((s) => s.specId === id) ?? null;
}

// =============================================================================
// React hook
// =============================================================================

interface UseDiscoveredSpecsResult {
  specs: DiscoveredSpec[];
  loading: boolean;
  error: Error | null;
  refresh: () => Promise<void>;
}

export function useDiscoveredSpecs(): UseDiscoveredSpecsResult {
  const [, setVersion] = useState(0);

  useEffect(() => {
    const bump = () => setVersion((v) => v + 1);
    subscribers.add(bump);

    // Trigger the load on first mount. Errors are captured into the
    // module-scoped `lastError` state and surfaced via the subscriber
    // bump — no need to handle here.
    if (cachedSpecs === null && inFlight === null) {
      void loadDiscoveredSpecs().catch(() => {
        // Already handled by the loader; the bump will surface the error.
      });
    }

    return () => {
      subscribers.delete(bump);
    };
  }, []);

  const refresh = async (): Promise<void> => {
    cachedSpecs = null;
    inFlight = null;
    notifySubscribers();
    try {
      await loadDiscoveredSpecs();
    } catch {
      // Error is already captured; the subscriber bump surfaces it.
    }
  };

  return {
    specs: cachedSpecs ?? [],
    loading: inFlight !== null,
    error: lastError,
    refresh,
  };
}

/**
 * Single-spec React hook. Subscribes to the same module-scoped cache
 * used by `useDiscoveredSpecs`, so it re-renders on cache updates and
 * SSE-driven `spec.changed` invalidations. Returns `null` while loading
 * or if the id is not present in the cache.
 */
export function useDiscoveredSpec(id: string): DiscoveredSpec | null {
  const [, setVersion] = useState(0);

  useEffect(() => {
    const bump = () => setVersion((v) => v + 1);
    subscribers.add(bump);

    // Trigger the load on first mount. Errors are captured into the
    // module-scoped `lastError` state and surfaced via the subscriber
    // bump — no need to handle here.
    if (cachedSpecs === null && inFlight === null) {
      void loadDiscoveredSpecs().catch(() => {
        // Already handled by the loader; the bump will surface the error.
      });
    }

    return () => {
      subscribers.delete(bump);
    };
  }, []);

  if (cachedSpecs === null) return null;
  return cachedSpecs.find((s) => s.specId === id) ?? null;
}
