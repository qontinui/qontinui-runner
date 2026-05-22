/**
 * HTTP-backed ActionExecutorLike for the spec-CI executor.
 *
 * Bridges ui-bridge-auto's in-memory action contract to the runner's
 * `/ui-bridge/sdk/*` proxy. Element resolution runs in-process against the
 * registry's last-known snapshot (cheap, deterministic); action dispatch
 * goes over HTTP to the connected app.
 *
 * After every action we trigger a `refresh()` on the registry so the next
 * `findElement` call sees the post-action DOM. `waitForIdle` is implemented
 * as a polling loop on the snapshot itself — the CLI waits until the
 * element count is stable across consecutive snapshots within a quiet
 * window. Crude but effective; the underlying `waitAfter` types
 * (`element`, `vanish`, `change`, `stable`) all rely on `findElement`
 * which we already make deterministic via `refresh()`.
 */

import type {
  ActionExecutorLike,
  ElementQuery,
  TransitionAction,
} from "@qontinui/ui-bridge-auto/runtime";
import { findFirst, matchesQuery } from "@qontinui/ui-bridge-auto/runtime";
import { HttpRegistry, type BridgeKind } from "./http-registry.js";

const DEFAULT_IDLE_QUIET_MS = 400;
const DEFAULT_IDLE_TIMEOUT_MS = 5_000;
const IDLE_POLL_MS = 100;

export interface HttpActionExecutorOptions {
  /**
   * Hook fired immediately before each remote action dispatch — useful for
   * tracing/timing in the CLI report. The hook does not affect dispatch.
   */
  onAction?: (info: {
    elementId: string;
    action: string;
    params?: Record<string, unknown>;
  }) => void;
}

export class HttpActionExecutor implements ActionExecutorLike {
  constructor(
    private readonly runnerUrl: string,
    private readonly registry: HttpRegistry,
    private readonly bridge: BridgeKind = "sdk",
    private readonly opts: HttpActionExecutorOptions = {},
  ) {}

  // -- ActionExecutorLike --------------------------------------------------

  findElement(query: ElementQuery): { id: string } | null {
    const elements = this.registry.getAllElements();
    const result = findFirst(elements, query);
    return result.match ? { id: result.match.id } : null;
  }

  findAllElements(query: ElementQuery): { id: string }[] {
    const elements = this.registry.getAllElements();
    const out: { id: string }[] = [];
    for (const el of elements) {
      if (matchesQuery(el, query).matches) out.push({ id: el.id });
    }
    return out;
  }

  async executeAction(
    elementId: string,
    action: string,
    params?: Record<string, unknown>,
  ): Promise<void> {
    this.opts.onAction?.({ elementId, action, params });

    const url = `${this.runnerUrl.replace(/\/$/, "")}/ui-bridge/${this.bridge}/element/${encodeURIComponent(elementId)}/action`;
    const body = params ? { action, params } : { action };

    let resp: Response;
    try {
      resp = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
    } catch (err) {
      throw new Error(
        `executeAction transport error (element=${elementId} action=${action}): ${String(err)}`,
      );
    }
    if (!resp.ok) {
      const text = await resp.text();
      throw new Error(
        `executeAction HTTP ${resp.status} (element=${elementId} action=${action}): ${text.slice(0, 200)}`,
      );
    }

    // Don't trust the runner's `success: true` — the SDK relay sometimes
    // reports success when the underlying action no-ops. Refresh the
    // snapshot so subsequent findElement queries see the post-action DOM,
    // and let the caller's `waitAfter` confirm the change took effect.
    const refreshed = await this.registry.refresh();
    if (!refreshed.sdkConnected) {
      throw new Error(
        `SDK disconnected mid-execution after action ${action} on ${elementId}: ${refreshed.fallbackNote ?? "no detail"}`,
      );
    }
  }

  async waitForIdle(timeout?: number): Promise<void> {
    const budget = timeout ?? DEFAULT_IDLE_TIMEOUT_MS;
    const quiet = DEFAULT_IDLE_QUIET_MS;
    const deadline = Date.now() + budget;
    let lastCount = this.registry.getAllElements().length;
    let stableSince = Date.now();

    while (Date.now() < deadline) {
      await sleep(IDLE_POLL_MS);
      const snap = await this.registry.refresh();
      if (!snap.sdkConnected) {
        // SDK fell over during waitForIdle — surface immediately rather
        // than blocking until the budget expires. The CLI catches this
        // and records a structured error.
        throw new Error(`SDK disconnected during waitForIdle: ${snap.fallbackNote ?? ""}`);
      }
      const count = this.registry.getAllElements().length;
      if (count !== lastCount) {
        lastCount = count;
        stableSince = Date.now();
        continue;
      }
      if (Date.now() - stableSince >= quiet) return;
    }
    // Timeout is non-fatal — ui-bridge-auto's `waitForIdle` callers treat it
    // as best-effort. Return silently rather than throwing.
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/**
 * Synchronous element resolver shaped like `executor.findElement` but useful
 * outside the executor (e.g., the CLI's pre-transition state detection).
 * Kept here so callers don't have to import findFirst directly.
 */
export function resolveQuery(
  registry: HttpRegistry,
  query: ElementQuery,
): { id: string } | null {
  const elements = registry.getAllElements();
  const result = findFirst(elements, query);
  return result.match ? { id: result.match.id } : null;
}

/**
 * Convenience: navigate the runner-connected app to a URL. Used at CLI
 * startup when `--page-url` is supplied so the executor lands on the right
 * page before transitions run.
 */
export async function navigateRunner(
  runnerUrl: string,
  url: string,
  bridge: BridgeKind = "sdk",
): Promise<void> {
  const endpoint = `${runnerUrl.replace(/\/$/, "")}/ui-bridge/${bridge}/page/navigate`;
  const resp = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ url }),
  });
  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(`page/navigate HTTP ${resp.status}: ${text.slice(0, 200)}`);
  }
}

// Re-export the unused TransitionAction type purely so it's discoverable when
// callers import this module — silences an `unused import` flag in callers
// who pull only HttpActionExecutor + this hint.
export type { TransitionAction };
