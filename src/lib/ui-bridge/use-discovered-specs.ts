/**
 * use-discovered-specs.ts
 *
 * Runtime spec loader (Section 13). Replaces the build-time
 * `getAllSpecs()` registry with a fetch from THIS runner's Spec API
 * (`GET http://127.0.0.1:<this instance's port>/apps/qontinui-runner/spec/list`),
 * with module-singleton caching and automatic SSE-driven invalidation on
 * `spec.changed`.
 *
 * Spec-multi-app Stream C: the runner-frontend always reads its OWN
 * specs (`app_id: "qontinui-runner"`). Other apps' specs are surfaced
 * through different views (the `/apps` registry + per-app browser).
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
import { resolvePort } from "@/lib/runner-api";
import type { DiscoveredSpec } from "../spec-prompt-builder";

/**
 * This runner's own app id in the `project.apps` registry.
 *
 * Exported so the snapshot enricher in `App.tsx` can stamp it as the
 * snapshot's top-level `appId`, which the Rust capture path reads to
 * attribute co-occurrence observations. There must be exactly one literal
 * for this id in the frontend — a second copy is the producer/consumer key
 * mismatch class this attribution exists to prevent.
 */
export const RUNNER_APP_ID = "qontinui-runner";

/**
 * Machine-readable code for "a fetch made on the caller's behalf failed
 * upstream". Prefixed onto the thrown message as `"<CODE>: <detail>"`, the
 * contract the terminal handlers already use. This throw does NOT go through
 * the SDK's `executeAction` (which since 0.24 hoists a handler error's `.code`
 * onto the result) — it propagates out of the `get_specs` IPC handler into
 * `useUIBridgeEventHandler`'s generic catch, which forwards `error.message`
 * and nothing else. So the prefix is the only carrier there, and the runner's
 * `classify_transport_error` reads it out of the message and answers
 * **HTTP 502**, not the 400 + `INTERNAL_ERROR` pair this used to produce.
 * `.code` is set as well, for any caller holding the Error itself.
 */
export const UPSTREAM_FETCH_FAILED = "UPSTREAM_FETCH_FAILED";

function upstreamFetchError(detail: string): Error & { code?: string } {
  const err = new Error(`${UPSTREAM_FETCH_FAILED}: ${detail}`) as Error & { code?: string };
  err.code = UPSTREAM_FETCH_FAILED;
  return err;
}

/**
 * THIS process's Spec API origin, resolved per call.
 *
 * ## The defect this closes (manual-test-loop iteration 26, item 1)
 *
 * These two URLs were module constants hardcoding `localhost:9876`. Every
 * runner NOT on 9876 therefore fetched its spec list — and opened a persistent
 * `EventSource` — against whichever OTHER PROCESS owned 9876. Measured live
 * from an instance on 9893:
 *
 *   GET http://127.0.0.1:9893/ui-bridge/control/specs
 *     -> 400 {"code":"INTERNAL_ERROR",
 *             "error":"GET http://localhost:9876/apps/qontinui-runner/spec/list
 *                      failed: HTTP 404 Not Found"}
 *   GET http://127.0.0.1:9893/apps/qontinui-runner/spec/list -> 200 (full list)
 *   GET http://127.0.0.1:9876/apps/qontinui-runner/spec/list -> 404 app-not-found
 *
 * A 404 rather than a connection refusal is the proof: the request WAS
 * answered, by the other process. The visible failure was therefore the benign
 * case — had 9876 had `qontinui-runner` registered (which it does whenever the
 * primary runner is up), this instance would have loaded ANOTHER RUNNER'S SPECS
 * with a 200 and no signal anywhere that it had crossed a process boundary.
 *
 * ## Why `resolvePort()` and not `getApiPort()` directly
 *
 * `resolvePort()` IS `getApiPort()` plus the `window.__QONTINUI_PORT__` the
 * runner injects at webview boot, and the ordering matters here specifically:
 * `loadDiscoveredSpecs()` is called from `App.tsx` BEFORE mount, while
 * `getApiPort()` is still sitting on its `9876` default waiting for
 * `useApiReady`'s async `api-ready` event. On a secondary runner that default
 * is the bug, arriving a few hundred milliseconds earlier. The injected global
 * is correct before any page JS runs, which is exactly the window this fetch
 * lives in.
 *
 * `127.0.0.1`, not `localhost`: Windows resolves `localhost` to `::1` first and
 * the runner binds the IPv4 loopback only, so the name costs a doomed IPv6
 * connect before the socket that answers.
 */
function specApiOrigin(): string {
  return `http://127.0.0.1:${resolvePort()}`;
}

function specListUrl(): string {
  return `${specApiOrigin()}/apps/${RUNNER_APP_ID}/spec/list`;
}

function specSubscribeUrl(): string {
  return `${specApiOrigin()}/apps/${RUNNER_APP_ID}/spec/subscribe`;
}

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
    eventSource = new EventSource(specSubscribeUrl());
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
  // Resolved ONCE per call, then reused for the message, so the URL a caller is
  // told about is the URL that was actually dialled.
  const url = specListUrl();

  let response: Response;
  try {
    response = await fetch(url, {
      method: "GET",
      headers: { Accept: "application/json" },
    });
  } catch (err) {
    // A refused connection is an upstream failure like any other — and after
    // the port fix it is the honest shape of "nothing is listening on my own
    // port", which is a very different diagnosis from the 404 another
    // process used to hand back.
    throw upstreamFetchError(
      `GET ${url} failed: ${err instanceof Error ? err.message : String(err)}`,
    );
  }

  if (!response.ok) {
    throw upstreamFetchError(`GET ${url} failed: HTTP ${response.status} ${response.statusText}`);
  }

  const body = (await response.json()) as SpecListResponse;
  if (!body.ok) {
    throw upstreamFetchError(
      `GET ${url} returned ok=false${body.reason ? `: ${body.reason}` : ""}`,
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
export async function loadDiscoveredSpec(id: string): Promise<DiscoveredSpec | null> {
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
