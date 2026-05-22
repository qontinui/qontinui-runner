/**
 * Snapshot-backed RegistryLike for the spec-CI executor.
 *
 * The runner's `/ui-bridge/sdk/snapshot` endpoint returns the connected app's
 * element registry as JSON. ui-bridge-auto's matcher (`matchesQuery`) reads
 * properties off a real `HTMLElement` — `getAttribute`, `tagName`,
 * `textContent`, etc. To keep the matcher unchanged while running outside a
 * browser, we reconstruct a minimal jsdom document with one synthetic node
 * per snapshot element and expose them as `QueryableElement`s.
 *
 * The reconstruction is deliberately shallow: we mirror tagName, attributes,
 * textContent, and visible state, NOT DOM hierarchy. Criteria that reach
 * across the tree (`parent`, `ancestor`, `hasChild`) won't resolve — that's
 * acceptable for the v1 spec-CI surface because authors are coached to use
 * `id` and `role`/`text` combinators that don't need hierarchy. If a real
 * spec ever needs hierarchy resolution, the answer is to run inside a real
 * browser via Bucket C7's headless launcher rather than to expand the jsdom
 * shim.
 */

import { JSDOM } from "jsdom";
import type { QueryableElement, RegistryLike } from "@qontinui/ui-bridge-auto/runtime";

// `QueryableElementState` is the live `el.getState()` return type. ui-bridge-auto
// exports `QueryableElement` from `/runtime` but not its inner state shape —
// mirror the shape locally so we don't depend on a private re-export.
interface QueryableElementState {
  visible?: boolean;
  enabled?: boolean;
  focused?: boolean;
  checked?: boolean;
  textContent?: string;
  value?: string | number | boolean;
  rect?: { x: number; y: number; width: number; height: number };
  computedStyles?: Record<string, string>;
  visibilityRatio?: number;
}

// ---------------------------------------------------------------------------
// Snapshot wire shape (mirrors qontinui-schemas/rust/src/ui_bridge.rs and the
// envelope the runner sends back from /ui-bridge/sdk/snapshot)
// ---------------------------------------------------------------------------

interface SnapshotElement {
  id: string;
  type?: string;
  label?: string | null;
  tagName?: string | null;
  role?: string | null;
  ariaLabel?: string | null;
  accessibleName?: string | null;
  text?: string | null;
  identifier?: {
    htmlId?: string | null;
    testId?: string | null;
    awasId?: string | null;
    uiId?: string | null;
    xpath?: string;
    selector?: string;
  };
  state?: {
    visible?: boolean;
    enabled?: boolean;
    focused?: boolean;
    checked?: boolean | null;
    value?: string | number | null;
    textContent?: string | null;
    accessibleName?: string | null;
    computedStyles?: Record<string, string>;
    rect?: { x: number; y: number; width: number; height: number };
  };
  bbox?: { x: number; y: number; width: number; height: number };
}

interface SnapshotEnvelope {
  success?: boolean;
  data: {
    elements?: SnapshotElement[];
    note?: string;
    reason?: string;
  };
}

// ---------------------------------------------------------------------------
// Synthetic HTMLElement construction
// ---------------------------------------------------------------------------

const synthesisDom = new JSDOM("<!doctype html><html><body></body></html>");
const document = synthesisDom.window.document;

/**
 * Build a detached jsdom element that reflects the snapshot row's structural
 * surface. The matcher reads `.tagName`, `.getAttribute(...)`, and
 * `.textContent`; everything else flows through `QueryableElement.getState()`,
 * which reads directly from the snapshot row.
 */
function synthesizeElement(el: SnapshotElement): HTMLElement {
  // Default to a generic <div> when tagName is absent — matches the snapshot
  // convention for content blocks the SDK didn't tag.
  const tag = (el.tagName ?? "div").toLowerCase();
  // jsdom rejects some pseudo-tags; fall back to `div` if construction throws.
  let node: HTMLElement;
  try {
    node = document.createElement(tag);
  } catch {
    node = document.createElement("div");
  }

  if (el.role) node.setAttribute("role", el.role);
  if (el.ariaLabel) node.setAttribute("aria-label", el.ariaLabel);
  if (el.identifier?.htmlId) node.id = el.identifier.htmlId;
  if (el.identifier?.testId) node.setAttribute("data-testid", el.identifier.testId);

  // Surface the visible text. The matcher prefers el.element.textContent over
  // QueryableElementState.textContent in a couple of branches, so set both.
  const text = el.text ?? el.state?.textContent ?? "";
  if (text) node.textContent = text;

  // Type attribute carries useful disambiguation for <input>; preserve it.
  if (el.type && tag === "input") {
    node.setAttribute("type", el.type);
  }

  return node;
}

function toQueryableState(el: SnapshotElement): QueryableElementState {
  const s = el.state ?? {};
  const out: QueryableElementState = {};
  if (s.visible !== undefined) out.visible = s.visible;
  if (s.enabled !== undefined) out.enabled = s.enabled;
  if (s.focused !== undefined) out.focused = s.focused;
  if (s.checked !== undefined && s.checked !== null) out.checked = s.checked;
  if (s.value !== undefined && s.value !== null) {
    out.value = s.value as string | number | boolean;
  }
  // Prefer the nested textContent; fall back to top-level text so the state's
  // text query path still sees content the matcher cares about.
  const txt = s.textContent ?? el.text ?? null;
  if (txt !== null) out.textContent = txt;
  const rect = s.rect ?? el.bbox;
  if (rect) out.rect = rect;
  if (s.computedStyles) out.computedStyles = s.computedStyles;
  return out;
}

function toQueryable(el: SnapshotElement): QueryableElement {
  const state = toQueryableState(el);
  return {
    id: el.id,
    element: synthesizeElement(el),
    type: el.type ?? "div",
    label: el.label ?? undefined,
    getState: () => state,
    getIdentifier: () => ({
      selector: el.identifier?.selector,
      xpath: el.identifier?.xpath,
      htmlId: el.identifier?.htmlId ?? undefined,
    }),
  };
}

// ---------------------------------------------------------------------------
// Snapshot fetch + parse
// ---------------------------------------------------------------------------

export class SnapshotFetchError extends Error {
  constructor(
    message: string,
    public readonly status?: number,
    public readonly body?: string,
  ) {
    super(message);
    this.name = "SnapshotFetchError";
  }
}

export interface SnapshotResult {
  elements: SnapshotElement[];
  sdkConnected: boolean;
  fallbackNote?: string;
}

/**
 * Bridge surface to target.
 *
 * - `sdk` (default) — proxy to an externally-connected app's SDK (qontinui-web
 *   running in a real browser). Required for spec-CI against qontinui-web.
 * - `control` — the runner's OWN React webview. Always available because the
 *   runner's webview-side SDK is loaded directly into its Tauri window.
 *   Use for self-tests + when authoring specs for the runner's own UI.
 */
export type BridgeKind = "sdk" | "control";

/**
 * Fetch the latest snapshot from the runner. Detects SDK-fallback (when the
 * runner returns a native-window capture because no SDK is connected) so the
 * CLI can fail fast rather than authoring against an empty/wrong page.
 */
export async function fetchSnapshot(
  runnerUrl: string,
  bridge: BridgeKind = "sdk",
): Promise<SnapshotResult> {
  const url = `${runnerUrl.replace(/\/$/, "")}/ui-bridge/${bridge}/snapshot`;
  let resp: Response;
  try {
    resp = await fetch(url);
  } catch (err) {
    throw new SnapshotFetchError(`snapshot transport error: ${String(err)}`);
  }
  if (!resp.ok) {
    const body = await resp.text();
    throw new SnapshotFetchError(
      `snapshot http ${resp.status}`,
      resp.status,
      body.slice(0, 200),
    );
  }
  const envelope = (await resp.json()) as SnapshotEnvelope;
  const data = envelope.data ?? {};
  const note = data.note ?? "";
  const reason = data.reason ?? "";
  // Match the heuristic spec_audit.py uses to detect SDK disconnect.
  const sdkConnected =
    !note.includes("SDK was not connected") &&
    !reason.includes("UI Bridge request timed out");
  return {
    elements: data.elements ?? [],
    sdkConnected,
    fallbackNote: sdkConnected ? undefined : `${note} / ${reason}`.trim(),
  };
}

// ---------------------------------------------------------------------------
// HttpRegistry
// ---------------------------------------------------------------------------

/**
 * A `RegistryLike` whose contents are refreshed on demand from the runner's
 * snapshot endpoint. Event subscription is a no-op (HTTP is not event-driven)
 * — the CLI manually calls `refresh()` after each action and re-runs
 * `StateDetector.evaluate()`.
 */
export class HttpRegistry implements RegistryLike {
  private elements: QueryableElement[] = [];

  constructor(
    private readonly runnerUrl: string,
    private readonly bridge: BridgeKind = "sdk",
  ) {}

  /** Pull the latest snapshot and rebuild the QueryableElement cache. */
  async refresh(): Promise<SnapshotResult> {
    const snap = await fetchSnapshot(this.runnerUrl, this.bridge);
    if (!snap.sdkConnected) {
      // Don't poison the cache with native-fallback data — preserve whatever
      // was there from the last successful snapshot. Caller checks the
      // sdkConnected flag and decides whether to abort.
      return snap;
    }
    this.elements = snap.elements.map(toQueryable);
    return snap;
  }

  // -- RegistryLike --------------------------------------------------------

  getAllElements(): QueryableElement[] {
    return this.elements;
  }

  on(
    _type: string,
    _listener: (event: { type: string; data: unknown }) => void,
  ): () => void {
    // HTTP isn't event-driven. StateDetector subscribes here to know when
    // to re-evaluate; we won't ever invoke the listener. Returning a no-op
    // unsubscribe is correct — the CLI drives evaluation manually after
    // each transition lands.
    return () => {};
  }
}
