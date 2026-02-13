/**
 * Fingerprint Generator for SDK Elements
 *
 * Generates ElementFingerprint objects from ExternalElement data.
 * These fingerprints enable cross-capture element matching, co-occurrence
 * analysis, and automated state machine construction.
 *
 * Matches the fingerprint schema used by the Python qontinui library
 * (qontinui.state_machine.fingerprint_types.ElementFingerprint).
 */

import type { ExternalElement, ElementFingerprint } from "../../types/ui-bridge-types";

// =============================================================================
// Configuration
// =============================================================================

export interface FingerprintConfig {
  viewportWidth: number;
  viewportHeight: number;
}

const DEFAULT_CONFIG: FingerprintConfig = {
  viewportWidth: 1920,
  viewportHeight: 1080,
};

// =============================================================================
// Constants
// =============================================================================

/** ARIA landmark roles that define semantic page regions */
const LANDMARK_ROLES = new Set([
  "banner",
  "complementary",
  "contentinfo",
  "form",
  "main",
  "navigation",
  "region",
  "search",
]);

/** Tag names that have implicit landmark roles */
const IMPLICIT_LANDMARK_MAP: Record<string, string> = {
  header: "banner",
  footer: "contentinfo",
  nav: "navigation",
  main: "main",
  aside: "complementary",
  form: "form",
  search: "search",
};

/** Patterns to strip from accessible names for normalization */
const DYNAMIC_PATTERNS = [
  /\b\d{1,2}[/:.-]\d{1,2}(?:[/:.-]\d{2,4})?\b/g, // dates/times
  /\b\d+\b/g, // standalone numbers
  /\b[a-f0-9]{8,}\b/gi, // hex IDs
  /\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi, // UUIDs
];

// =============================================================================
// Element Index
// =============================================================================

type ElementIndex = Map<string, ExternalElement>;

function buildElementIndex(elements: ExternalElement[]): ElementIndex {
  const index = new Map<string, ExternalElement>();
  for (const el of elements) {
    if (el.id) {
      index.set(el.id, el);
    }
  }
  return index;
}

// =============================================================================
// Structural Path
// =============================================================================

/**
 * Compute a tag-only structural path from root to this element.
 * e.g., "div > nav > ul > li > a"
 *
 * Uses parent references to walk up the tree.
 */
function computeStructuralPath(element: ExternalElement, index: ElementIndex): string {
  const pathParts: string[] = [];
  let current: ExternalElement | undefined = element;
  let depth = 0;
  const maxDepth = 30; // prevent infinite loops

  while (current && depth < maxDepth) {
    pathParts.unshift(current.tagName?.toLowerCase() || current.type || "unknown");
    if (!current.parent) break;
    current = index.get(current.parent);
    depth++;
  }

  return pathParts.join(" > ");
}

// =============================================================================
// Position Zone
// =============================================================================

/**
 * Classify element into a position zone based on its bounds relative to viewport.
 *
 * Zones: header, footer, sidebar-left, sidebar-right, main, modal
 */
function classifyPositionZone(element: ExternalElement, config: FingerprintConfig): string {
  // Modal detection via role
  const role = element.accessibility?.role || element.role || "";
  if (role === "dialog" || role === "alertdialog") {
    return "modal";
  }
  if (element.accessibility?.ariaLabel?.toLowerCase().includes("modal")) {
    return "modal";
  }

  const { x, y, width, height } = element.bounds;
  const centerX = x + width / 2;
  const centerY = y + height / 2;

  const { viewportWidth: vw, viewportHeight: vh } = config;
  const relY = centerY / vh;
  const relX = centerX / vw;

  // Header: top 12% of viewport
  if (relY < 0.12) {
    return "header";
  }

  // Footer: bottom 12% of viewport
  if (relY > 0.88) {
    return "footer";
  }

  // Sidebar-left: left 18% (excluding header/footer)
  if (relX < 0.18) {
    return "sidebar-left";
  }

  // Sidebar-right: right 18% (excluding header/footer)
  if (relX > 0.82) {
    return "sidebar-right";
  }

  return "main";
}

// =============================================================================
// Size Category
// =============================================================================

/**
 * Classify element size into categories matching the Python library.
 *
 * Categories: icon, button, small, medium, large, fullwidth, panel
 */
function classifySizeCategory(
  bounds: { width: number; height: number },
  config: FingerprintConfig,
): string {
  const { width: w, height: h } = bounds;
  const maxDim = Math.max(w, h);

  // Fullwidth: wider than 80% of viewport
  if (w > config.viewportWidth * 0.8) {
    return "fullwidth";
  }

  // Panel: both dimensions large
  if (w > 400 && h > 400) {
    return "panel";
  }

  // Icon: very small
  if (maxDim < 32) {
    return "icon";
  }

  // Button: small clickable element
  if (maxDim < 64 || (w < 200 && h < 50)) {
    return "button";
  }

  // Small
  if (maxDim < 150) {
    return "small";
  }

  // Medium
  if (maxDim < 400) {
    return "medium";
  }

  // Large
  return "large";
}

// =============================================================================
// Landmark Context
// =============================================================================

/**
 * Determine the nearest ARIA landmark context for an element.
 * Walks up the tree to find the first ancestor with a landmark role.
 */
function determineLandmarkContext(
  element: ExternalElement,
  index: ElementIndex,
): { context: string; label?: string } {
  let current: ExternalElement | undefined = element;
  let depth = 0;
  const maxDepth = 20;

  while (current && depth < maxDepth) {
    // Check explicit role
    const role = current.accessibility?.role || current.role || "";
    if (LANDMARK_ROLES.has(role)) {
      return {
        context: role,
        label: current.accessibility?.ariaLabel || undefined,
      };
    }

    // Check implicit landmark from tag name
    const tag = current.tagName?.toLowerCase() || "";
    const implicitRole = IMPLICIT_LANDMARK_MAP[tag];
    if (implicitRole) {
      return {
        context: implicitRole,
        label: current.accessibility?.ariaLabel || undefined,
      };
    }

    if (!current.parent) break;
    current = index.get(current.parent);
    depth++;
  }

  return { context: "none" };
}

// =============================================================================
// Accessible Name Normalization
// =============================================================================

/**
 * Normalize an accessible name by stripping dynamic content.
 * This makes fingerprints stable across data changes (e.g., "3 items" → "N items").
 */
function normalizeAccessibleName(name: string | undefined): string | undefined {
  if (!name) return undefined;

  let normalized = name.trim();
  if (!normalized) return undefined;

  for (const pattern of DYNAMIC_PATTERNS) {
    normalized = normalized.replace(pattern, "N");
  }

  // Collapse multiple spaces
  normalized = normalized.replace(/\s+/g, " ").trim();

  return normalized || undefined;
}

// =============================================================================
// Repeat Pattern Detection
// =============================================================================

interface RepeatPatternInfo {
  type: string; // "list", "grid", "table"
  containerTag: string;
  containerRole: string;
  index: number;
  count: number;
  itemSelector: string;
  itemRole: string;
}

/**
 * Detect if an element is part of a repeating pattern (list, grid, table).
 * Looks at siblings with the same tag+role within the parent container.
 */
function detectRepeatPattern(
  element: ExternalElement,
  index: ElementIndex,
): { isRepeating: boolean; pattern?: RepeatPatternInfo } {
  if (!element.parent) {
    return { isRepeating: false };
  }

  const parent = index.get(element.parent);
  if (!parent || !parent.children || parent.children.length < 3) {
    return { isRepeating: false };
  }

  const myTag = element.tagName?.toLowerCase() || "";
  const myRole = element.role || element.accessibility?.role || "";

  // Count siblings with same tag+role
  let sameTagCount = 0;
  let myIndex = 0;
  const siblings = parent.children
    .map((id) => index.get(id))
    .filter((el): el is ExternalElement => el !== undefined);

  for (let i = 0; i < siblings.length; i++) {
    const sib = siblings[i];
    const sibTag = sib.tagName?.toLowerCase() || "";
    const sibRole = sib.role || sib.accessibility?.role || "";

    if (sibTag === myTag && sibRole === myRole) {
      if (sib.id === element.id) {
        myIndex = sameTagCount;
      }
      sameTagCount++;
    }
  }

  if (sameTagCount < 3) {
    return { isRepeating: false };
  }

  // Determine container type
  const parentTag = parent.tagName?.toLowerCase() || "";
  const parentRole = parent.role || parent.accessibility?.role || "";

  let containerType = "list";
  if (parentTag === "table" || parentTag === "tbody" || parentRole === "grid") {
    containerType = "table";
  } else if (parentRole === "grid" || parentRole === "treegrid") {
    containerType = "grid";
  }

  return {
    isRepeating: true,
    pattern: {
      type: containerType,
      containerTag: parentTag,
      containerRole: parentRole,
      index: myIndex,
      count: sameTagCount,
      itemSelector: myTag,
      itemRole: myRole,
    },
  };
}

// =============================================================================
// Hash Computation
// =============================================================================

/**
 * Compute a deterministic hash from fingerprint properties.
 * Uses a simple string hash (FNV-1a) converted to hex.
 *
 * The hash includes: structuralPath, positionZone, landmarkContext, role,
 * accessibleName, sizeCategory, tagName
 */
function computeHash(parts: {
  structuralPath: string;
  positionZone: string;
  landmarkContext: string;
  role: string;
  accessibleName: string | undefined;
  sizeCategory: string;
  tagName: string;
}): string {
  const input = [
    parts.structuralPath,
    parts.positionZone,
    parts.landmarkContext,
    parts.role,
    parts.accessibleName || "",
    parts.sizeCategory,
    parts.tagName,
  ].join("|");

  // FNV-1a hash (32-bit), then convert to 16-char hex
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }

  // Convert to unsigned and produce a 16-char hex string
  // Use two rounds with different seeds for 64 bits total
  const hash1 = (hash >>> 0).toString(16).padStart(8, "0");

  let hash2 = 0x6c62272e;
  for (let i = input.length - 1; i >= 0; i--) {
    hash2 ^= input.charCodeAt(i);
    hash2 = Math.imul(hash2, 0x01000193);
  }
  const hash2Str = (hash2 >>> 0).toString(16).padStart(8, "0");

  return hash1 + hash2Str;
}

// =============================================================================
// Public API
// =============================================================================

/**
 * Generate a fingerprint for a single element.
 *
 * @param element - The element to fingerprint
 * @param index - Pre-built index of all elements (for tree traversal)
 * @param config - Viewport configuration
 */
export function generateElementFingerprint(
  element: ExternalElement,
  index: ElementIndex,
  config: FingerprintConfig = DEFAULT_CONFIG,
): ElementFingerprint {
  const structuralPath = computeStructuralPath(element, index);
  const positionZone = classifyPositionZone(element, config);
  const sizeCategory = classifySizeCategory(element.bounds, config);
  const { context: landmarkContext, label: landmarkLabel } = determineLandmarkContext(
    element,
    index,
  );
  const role = element.accessibility?.role || element.role || "";
  const tagName = element.tagName?.toLowerCase() || element.type || "";
  const accessibleName = normalizeAccessibleName(
    element.accessibleName || element.accessibility?.accessibleName || element.label,
  );
  const { isRepeating, pattern } = detectRepeatPattern(element, index);

  const relX =
    element.bounds.width > 0
      ? Math.min(
          1,
          Math.max(0, (element.bounds.x + element.bounds.width / 2) / config.viewportWidth),
        )
      : 0;
  const relY =
    element.bounds.height > 0
      ? Math.min(
          1,
          Math.max(0, (element.bounds.y + element.bounds.height / 2) / config.viewportHeight),
        )
      : 0;

  const hash = computeHash({
    structuralPath,
    positionZone,
    landmarkContext,
    role,
    accessibleName,
    sizeCategory,
    tagName,
  });

  const fingerprint: ElementFingerprint = {
    hash,
    structuralPath,
    positionZone,
    landmarkContext,
    role,
    tagName,
    sizeCategory,
    relativePosition: { top: relY, left: relX },
    isRepeating,
  };

  if (landmarkLabel) {
    fingerprint.landmarkLabel = landmarkLabel;
  }

  if (accessibleName) {
    fingerprint.accessibleName = accessibleName;
  }

  if (isRepeating && pattern) {
    fingerprint.repeatPattern = {
      type: pattern.type,
      containerTag: pattern.containerTag,
      containerRole: pattern.containerRole,
      index: pattern.index,
      count: pattern.count,
      itemSelector: pattern.itemSelector,
      itemRole: pattern.itemRole,
    };
  }

  return fingerprint;
}

/**
 * Generate fingerprints for all elements. Returns a map of element ID → fingerprint,
 * and also attaches the fingerprint to each element's `fingerprint` field.
 *
 * @param elements - All elements to fingerprint
 * @param config - Optional viewport configuration
 * @returns Map of fingerprint hash → ElementFingerprint
 */
export function generateFingerprints(
  elements: ExternalElement[],
  config?: Partial<FingerprintConfig>,
): {
  catalog: Record<string, ElementFingerprint>;
  elementFingerprintMap: Map<string, ElementFingerprint>;
} {
  const fullConfig: FingerprintConfig = { ...DEFAULT_CONFIG, ...config };
  const index = buildElementIndex(elements);
  const catalog: Record<string, ElementFingerprint> = {};
  const elementFingerprintMap = new Map<string, ElementFingerprint>();

  for (const element of elements) {
    if (!element.id) continue;
    // Skip invisible elements
    if (element.visible === false) continue;

    const fp = generateElementFingerprint(element, index, fullConfig);

    // Attach to element
    element.fingerprint = fp;

    // Add to catalog (hash → fingerprint)
    catalog[fp.hash] = fp;

    // Add to element map (element ID → fingerprint)
    elementFingerprintMap.set(element.id, fp);
  }

  return { catalog, elementFingerprintMap };
}

/**
 * Extract fingerprint hashes from a set of elements.
 * Returns unique hashes sorted alphabetically.
 */
export function extractFingerprintHashes(elements: ExternalElement[]): string[] {
  const hashes = new Set<string>();
  for (const el of elements) {
    if (el.fingerprint?.hash) {
      hashes.add(el.fingerprint.hash);
    }
  }
  return Array.from(hashes).sort();
}
