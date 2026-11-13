/**
 * The DOM subtree read behind `GET /ui-bridge/control/element/{id}/tree`.
 *
 * Walks a registered element's rendered subtree breadth-first and serializes
 * each node's tagName, a filtered attribute set and its child count. Every
 * attribute value passes through the SDK's §4.6 redaction verdict for that
 * node, on the same two axes the element read (`serializeRegisteredElement`)
 * uses:
 *
 * - PASSTHROUGH — addressing and structural keys served raw whatever the
 *   verdict. `role`, `data-testid`, `data-awas-element` and the SDK's two
 *   identifier attributes are served raw by the element read too. `name`,
 *   `type` and `class` are kept raw here by choice, as developer-authored
 *   structure; that is wider than the SDK's own attribute-record scrub
 *   (`scrubRecord`), which redacts every value inside a boundary. The SDK's
 *   control markers (`data-bridge-redact`, `data-bridge-invisible`) stay
 *   raw; `data-bridge-redact` is what shows where a boundary starts.
 * - CONTENT axis (`verdict.content`, a `data-bridge-redact` boundary only):
 *   `title`, `aria-label`.
 * - VALUE axis (`verdict.value`, boundary OR password input — the gate the
 *   SDK's `state.dataset` rides): every other key, including any other
 *   `data-bridge-*`. The passthrough is an exact allowlist, so a key added
 *   later fails closed.
 *
 * Redacted values become `REDACTED_VALUE` and their keys survive, so a reader
 * can tell "redacted" from "absent". Never add `value` to the key list:
 * react-dom mirrors a controlled input's live value into that attribute.
 */

import {
  REDACTED_VALUE,
  UI_BRIDGE_ID_ATTR,
  UI_BRIDGE_TEST_ID_ATTR,
  isContentRedacted,
  verdictOf,
} from "@qontinui/ui-bridge";

/** Maximum nodes returned, root included. */
export const DOM_TREE_NODE_CAP = 200;
/** `class` values longer than this are truncated with an ellipsis. */
export const DOM_TREE_CLASS_MAX_LEN = 80;

export interface DomTreeNode {
  tagName: string;
  attrs: Record<string, string>;
  childCount: number;
  children: DomTreeNode[];
}

export interface ElementDomTree {
  /** Depth of the deepest returned descendant (0 when none were returned). */
  depth: number;
  tree: DomTreeNode;
  truncated: boolean;
}

/** Keys served raw whatever the verdict (structural / addressing). */
const PASSTHROUGH_KEYS: ReadonlySet<string> = new Set([
  "role",
  "name",
  "type",
  "class",
  "data-testid",
  "data-awas-element",
  "data-bridge-redact",
  "data-bridge-invisible",
  UI_BRIDGE_ID_ATTR,
  UI_BRIDGE_TEST_ID_ATTR,
]);

/** Keys hidden on the CONTENT axis (boundary only). */
const CONTENT_KEYS: ReadonlySet<string> = new Set(["title", "aria-label"]);

const NAMED_KEYS = ["role", "title", "aria-label", "name", "type"] as const;

const camelToKebab = (s: string): string => s.replace(/[A-Z]/g, (m) => "-" + m.toLowerCase());

/**
 * Serialize one node's attributes, scrubbed per that node's §4.6 verdict.
 * Exported for the walk's tests; the walk is the only production caller.
 */
export function serializeDomTreeAttrs(el: Element): Record<string, string> {
  const raw: Record<string, string> = {};
  for (const k of NAMED_KEYS) {
    const v = el.getAttribute(k);
    if (v != null) raw[k] = v;
  }
  const cls = el.getAttribute("class");
  if (cls && cls.length > 0) {
    raw["class"] =
      cls.length > DOM_TREE_CLASS_MAX_LEN ? cls.slice(0, DOM_TREE_CLASS_MAX_LEN) + "…" : cls;
  }
  const dataset = (el as HTMLElement).dataset;
  if (dataset) {
    for (const k in dataset) {
      const v = dataset[k];
      if (typeof v === "string") raw[`data-${camelToKebab(k)}`] = v;
    }
  }

  // The SDK's verdict accepts any Element at runtime (it walks `parentElement`
  // and reads attributes); SVG nodes inside a boundary must be scrubbed too.
  const verdict = verdictOf(el as HTMLElement);
  if (!verdict.value && !verdict.content) return raw;

  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(raw)) {
    if (PASSTHROUGH_KEYS.has(k)) out[k] = v;
    else if (CONTENT_KEYS.has(k)) out[k] = verdict.content ? REDACTED_VALUE : v;
    else out[k] = verdict.value ? REDACTED_VALUE : v;
  }
  return out;
}

/**
 * Walk `root`'s subtree breadth-first to `depthBudget` levels (callers clamp
 * it), capped at `DOM_TREE_NODE_CAP` nodes. BFS so `truncated` reflects a
 * breadth-first cap rather than DFS skew.
 */
export function buildElementDomTree(root: Element, depthBudget: number): ElementDomTree {
  let nodeCount = 0;
  let truncated = false;
  let actualDepth = 0;

  const rootNode: DomTreeNode = {
    tagName: root.tagName,
    attrs: serializeDomTreeAttrs(root),
    childCount: root.children.length,
    children: [],
  };
  nodeCount++;

  // Each frame: (parentNode, parentEl, depth of the parent's children)
  const queue: Array<{ node: DomTreeNode; el: Element; depth: number }> = [
    { node: rootNode, el: root, depth: 1 },
  ];

  while (queue.length > 0) {
    const frame = queue.shift()!;
    if (frame.depth > depthBudget) continue;
    for (const childEl of Array.from(frame.el.children)) {
      if (nodeCount >= DOM_TREE_NODE_CAP) {
        truncated = true;
        break;
      }
      const childNode: DomTreeNode = {
        tagName: childEl.tagName,
        attrs: serializeDomTreeAttrs(childEl),
        childCount: childEl.children.length,
        children: [],
      };
      nodeCount++;
      frame.node.children.push(childNode);
      if (frame.depth > actualDepth) actualDepth = frame.depth;
      if (frame.depth + 1 <= depthBudget) {
        queue.push({ node: childNode, el: childEl, depth: frame.depth + 1 });
      }
    }
    if (truncated) break;
  }

  // If root had no children, depth is 0 (no descendants returned).
  if (rootNode.children.length === 0) actualDepth = 0;

  return { depth: actualDepth, tree: rootNode, truncated };
}

/**
 * The `/debug/element-tree` label for a registered element: collapsed to
 * `REDACTED_VALUE` inside a §4.6 boundary, as the SDK's own handler does.
 */
export function debugTreeLabel(domEl: unknown, label: string | undefined): string | undefined {
  if (label === undefined) return undefined;
  if (domEl instanceof Element && isContentRedacted(domEl as HTMLElement)) return REDACTED_VALUE;
  return label;
}
