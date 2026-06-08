import { getApiPort } from "@/lib/runner-api";
import {
  type RegisteredElement,
  type RegisteredComponent,
  serializeRegisteredElement,
} from "@qontinui/ui-bridge";
import type { SerializedElement, SerializedComponent } from "./types";

/**
 * Send a response to the Rust backend via HTTP fallback.
 * Used when the Tauri event system is unresponsive.
 */
export async function httpSendResponse(response: unknown): Promise<boolean> {
  try {
    const port = getApiPort();
    const resp = await fetch(`http://localhost:${port}/ui-bridge/ipc-response`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(response),
    });
    return resp.ok;
  } catch {
    return false;
  }
}

/**
 * Send a pong to the Rust backend via HTTP fallback.
 */
export async function httpSendPong(): Promise<boolean> {
  try {
    const port = getApiPort();
    const resp = await fetch(`http://localhost:${port}/ui-bridge/pong`, {
      method: "POST",
    });
    return resp.ok;
  } catch {
    return false;
  }
}

/**
 * Map task run status strings from the runner to SDK-compatible workflow status values.
 */
export function mapTaskRunStatus(status: string): string {
  switch (status) {
    case "in_progress":
    case "running":
      return "running";
    case "completed":
    case "complete":
    case "success":
      return "completed";
    case "failed":
    case "error":
      return "failed";
    case "cancelled":
    case "stopped":
      return "cancelled";
    default:
      return "pending";
  }
}

/**
 * Serialize a RegisteredElement to a plain object (removes DOM references)
 */
export function serializeElement(element: RegisteredElement): SerializedElement {
  // Delegate to ui-bridge's shared serializer so this path can't drift from
  // BridgeSnapshot's element shape. The runner mounts UI Bridge routes under
  // /ui-bridge/control/*, so override the default base path. Add registeredAt
  // /mounted on top — they're single-element-detail-only and not part of the
  // snapshot contract.
  const base = serializeRegisteredElement(element, {
    componentBasePath: "/ui-bridge/control/component",
  });
  return {
    ...base,
    registeredAt: element.registeredAt,
    mounted: element.mounted,
  } as SerializedElement;
}

/**
 * Serialize a RegisteredComponent to a plain object.
 *
 * Phase 3.1 (plan 2026-05-03): forward the `scope` annotation so component
 * listings on the runner carry the same discoverability hint the SDK
 * documents. `undefined` is the documented default for `'route'` (see
 * `RegisteredComponent.scope` JSDoc), so we materialize that default here
 * rather than dropping the key. This guarantees clients can always read
 * `component.scope` without ad-hoc `??= 'route'` plumbing.
 */
export function serializeComponent(component: RegisteredComponent): SerializedComponent {
  return {
    id: component.id,
    name: component.name,
    description: component.description,
    actions: component.actions.map((a) => ({
      id: a.id,
      label: a.label,
      description: a.description,
      paramSchema: a.paramSchema,
      path: `/ui-bridge/control/component/${component.id}/action/${a.id}`,
    })),
    actionInvocationPath: `/ui-bridge/control/component/${component.id}/action/{actionId}`,
    elementIds: component.elementIds,
    registeredAt: component.registeredAt,
    mounted: component.mounted,
    scope: component.scope ?? "route",
  };
}

/**
 * Typed accessor for the global `window.__UI_BRIDGE__` object.
 */
export function getUIBridgeGlobal(): Record<string, unknown> | undefined {
  return (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
    | Record<string, unknown>
    | undefined;
}

/**
 * Derive a short canonical alias for a tab id by stripping a known
 * namespacing prefix (`sm-tab-`). The runner's `MainTabId` catalog
 * doesn't use that prefix, so canonical is typically the id itself —
 * but tabbed sub-pages (state-machine's `sm-tab-graph` / `sm-tab-states`
 * etc.) use a prefix scheme, and exposing the bare segment as
 * `canonical` lets agents reason about the tab uniformly across pages.
 *
 * Used in three places that emit the tab catalog: `tabs_list` and
 * `get_playbook` IPC handlers in `usePageEvents.ts`, and the
 * `availableTabs` enrichment in `useDiscoveryEvents.ts`. Lifting this
 * to a shared helper avoids the three sites drifting apart on what
 * "canonical" means.
 */
export function toTabCanonical(tabId: string): string {
  const prefix = "sm-tab-";
  return tabId.startsWith(prefix) ? tabId.slice(prefix.length) : tabId;
}

/**
 * Compute the Levenshtein edit distance between two strings.
 *
 * Used by the IPC error paths in {@link ./useControlEvents.ts useControlEvents}
 * to suggest closest-match element ids when the caller hits an
 * unknown id (typo recovery). The implementation is a classic
 * iterative DP — O(|a|*|b|) time, O(min(|a|,|b|)) space — and
 * stable enough that we keep it inline rather than pulling in a
 * dependency for ~15 LOC of arithmetic.
 *
 * Returns the minimum number of single-character insertions,
 * deletions, or substitutions to transform `a` into `b`.
 */
export function levenshtein(a: string, b: string): number {
  if (a === b) return 0;
  if (a.length === 0) return b.length;
  if (b.length === 0) return a.length;

  // Always iterate over the shorter string in the inner loop to keep
  // the row buffer small.
  const [shorter, longer] = a.length <= b.length ? [a, b] : [b, a];
  const m = shorter.length;
  const n = longer.length;

  let prev: number[] = new Array(m + 1);
  let curr: number[] = new Array(m + 1);
  for (let j = 0; j <= m; j++) prev[j] = j;

  for (let i = 1; i <= n; i++) {
    curr[0] = i;
    const longerCh = longer.charCodeAt(i - 1);
    for (let j = 1; j <= m; j++) {
      const cost = shorter.charCodeAt(j - 1) === longerCh ? 0 : 1;
      const del = prev[j] + 1;
      const ins = curr[j - 1] + 1;
      const sub = prev[j - 1] + cost;
      curr[j] = del < ins ? (del < sub ? del : sub) : ins < sub ? ins : sub;
    }
    [prev, curr] = [curr, prev];
  }
  return prev[m];
}

/**
 * Cap on auto-awaiting a top-level Promise returned by `page_evaluate`.
 * Without this, a caller passing `(async () => { await new Promise(() => {}) })()`
 * would wedge the eval response forever. 30 s is generous for legitimate
 * async work (network round-trip, batch DOM scan) while still bounding
 * the worst case. Used by both the legacy IPC `page_evaluate` branch
 * (`usePageEvents.ts`) and the tagged Tauri-event evaluate handler
 * (`useUIBridgeEvaluateHandler.ts`).
 */
export const PAGE_EVALUATE_PROMISE_TIMEOUT_MS = 30_000;

/**
 * Duck-typed thenable check. Spec-correct (matches what `await` itself
 * does): "thenable" is "has a callable `.then`". Catches cross-realm
 * Promises (e.g. iframes whose Promise constructor lives in a different
 * realm) that `instanceof Promise` would miss.
 */
export function isThenable(value: unknown): value is PromiseLike<unknown> {
  return (
    value !== null &&
    (typeof value === "object" || typeof value === "function") &&
    typeof (value as { then?: unknown }).then === "function"
  );
}

/**
 * If `value` is a thenable, await it with a timeout cap; otherwise
 * return as-is (no Promise-wrap allocation for the common synchronous
 * case). Used by the `page_evaluate` handlers to auto-resolve top-level
 * Promises returned by expressions like `(async () => ({a:1}))()` so
 * callers don't have to spell out `.then(v => v)` to unwrap them.
 *
 * On timeout, throws an Error whose message names the elapsed seconds —
 * the caller's existing try/catch maps that to the standard error
 * envelope, so timeouts are surfaced as `success: false, error: "..."`
 * rather than wedging the response forever. Rejections of the awaited
 * Promise propagate normally (same try/catch handles them).
 */
export async function awaitWithTimeout(value: unknown, timeoutMs: number): Promise<unknown> {
  if (!isThenable(value)) {
    return value;
  }
  let timer: ReturnType<typeof setTimeout> | null = null;
  const timeoutPromise = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      reject(
        new Error(
          `page_evaluate: Promise did not resolve within ${(timeoutMs / 1000).toFixed(1)}s`,
        ),
      );
    }, timeoutMs);
  });
  try {
    return await Promise.race([value, timeoutPromise]);
  } finally {
    if (timer !== null) clearTimeout(timer);
  }
}

/**
 * Pick up to 5 element ids closest to `target` by Levenshtein distance,
 * filtered to ids whose distance is at most `floor(target.length / 2)`.
 *
 * This is the closest-match scaffolding used by the element-not-found
 * IPC error path to surface typo-recovery suggestions in the response
 * `hint.closestMatches` field. The threshold cap prevents `"foo"` from
 * matching every 4+ char id in the registry — short queries get tight
 * thresholds so we only return genuinely similar ids.
 */
export function closestElementIds(target: string, candidates: readonly string[]): string[] {
  if (!target) return [];
  const threshold = Math.floor(target.length / 2);
  const scored: Array<{ id: string; distance: number }> = [];
  for (const id of candidates) {
    if (id === target) continue;
    const distance = levenshtein(target, id);
    if (distance <= threshold) {
      scored.push({ id, distance });
    }
  }
  scored.sort((a, b) => a.distance - b.distance || a.id.localeCompare(b.id));
  return scored.slice(0, 5).map((s) => s.id);
}

/**
 * Per-element action-allow gate for the `execute_action` IPC handler.
 *
 * Returns `true` when `action` may be dispatched to the element. An element
 * with NO declared action set (`allowedActions` empty) is permissive (the SDK's
 * global supported-action validation still applies downstream). When a declared
 * set is present, the action must be in it — EXCEPT `hoverClick`, a click-variant
 * (reveal a `pointer-events:none`-until-`:hover`/`group-hover` control, then
 * click) that is allowed wherever `click` is. This mirrors the runner-side Rust
 * `is_action_advertised` click-variant exemption so the two advertised-action
 * gates (Rust pre-IPC + this frontend pre-check) can't disagree and silently
 * reject a hover-gated toolbar button registered with an explicit `actions=` prop.
 */
export function isElementActionAllowed(
  allowedActions: readonly string[],
  action: string,
): boolean {
  if (allowedActions.length === 0) return true;
  if (allowedActions.includes(action)) return true;
  if (action === "hoverClick" && allowedActions.includes("click")) return true;
  return false;
}
