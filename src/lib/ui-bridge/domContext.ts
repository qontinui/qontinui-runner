/**
 * DOM Context Helper Functions
 *
 * Provides utilities for building DOM context information from ExternalElement data.
 * Used by the ElementDescriptionPanel to show path breadcrumbs, parent info, and siblings.
 */

import type { ExternalElement } from "../../hooks/useExternalUIBridge";

/**
 * A path segment representing one element in the DOM path
 */
export interface PathSegment {
  /** Element ID (can be used to navigate/select) */
  elementId: string | null;
  /** Tag name (div, button, etc.) */
  tagName: string;
  /** Element type (button, input, etc.) */
  type?: string;
  /** ARIA role if present */
  role?: string;
  /** Label or accessible name */
  label?: string;
  /** Whether this is the target element */
  isCurrent?: boolean;
}

/**
 * Parent element information
 */
export interface ParentInfo {
  /** Parent element ID */
  elementId: string;
  /** Tag name */
  tagName: string;
  /** Element type */
  type: string;
  /** Label or text */
  label?: string;
  /** ARIA role */
  role?: string;
}

/**
 * Sibling element information
 */
export interface SiblingInfo {
  /** Element ID */
  elementId: string;
  /** Tag name */
  tagName: string;
  /** Element type */
  type: string;
  /** Label or text */
  label?: string;
  /** ARIA role */
  role?: string;
  /** Whether this is the current element */
  isCurrent: boolean;
}

/**
 * Landmark context information
 */
export interface LandmarkInfo {
  /** Landmark role (banner, navigation, main, etc.) */
  role: string;
  /** Element ID of the landmark */
  elementId?: string;
  /** Label if present */
  label?: string;
  /** Position zone from fingerprint */
  positionZone?: string;
}

/**
 * Full DOM context for an element
 */
export interface DOMContext {
  /** Path from root to the element */
  path: PathSegment[];
  /** Immediate parent element */
  parent: ParentInfo | null;
  /** Sibling elements (includes current element marked) */
  siblings: SiblingInfo[];
  /** Nearest ARIA landmark */
  landmark: LandmarkInfo | null;
}

/**
 * Build a map of element ID to element for faster lookups
 */
function buildElementMap(elements: ExternalElement[]): Map<string, ExternalElement> {
  return new Map(elements.map((el) => [el.id, el]));
}

/**
 * Get a display label for an element
 */
function getElementLabel(element: ExternalElement): string | undefined {
  return element.label || element.accessibleName || element.text || undefined;
}

/**
 * Build the path from root to the given element
 *
 * @param element - The target element
 * @param allElements - All elements in the page
 * @returns Array of PathSegments from root to element
 */
export function buildElementPath(
  element: ExternalElement,
  allElements: ExternalElement[],
): PathSegment[] {
  const elementMap = buildElementMap(allElements);
  const path: PathSegment[] = [];

  // Walk up the parent chain
  let current: ExternalElement | undefined = element;
  while (current) {
    path.unshift({
      elementId: current.id,
      tagName: current.tagName.toLowerCase(),
      type: current.type,
      role: current.role || current.accessibility?.role,
      label: getElementLabel(current),
      isCurrent: current.id === element.id,
    });

    // Move to parent
    if (current.parent) {
      current = elementMap.get(current.parent);
    } else {
      break;
    }
  }

  // If we have a structural path from fingerprint, use it to fill in missing ancestors
  // The fingerprint path might include elements not in our element list
  if (element.fingerprint?.structuralPath && path.length < 3) {
    const structuralParts = element.fingerprint.structuralPath.split(" > ");
    const existingTags = path.map((p) => p.tagName);

    // Add structural path segments that aren't already in our path
    const additionalSegments: PathSegment[] = [];
    for (const part of structuralParts) {
      const tagName = part.toLowerCase();
      if (!existingTags.includes(tagName)) {
        additionalSegments.push({
          elementId: null, // Not clickable - we don't have this element
          tagName,
          isCurrent: false,
        });
      }
    }

    // Prepend structural segments
    if (additionalSegments.length > 0) {
      path.unshift(...additionalSegments);
    }
  }

  return path;
}

/**
 * Find the parent element of the given element
 *
 * @param element - The element to find the parent of
 * @param allElements - All elements in the page
 * @returns ParentInfo or null if no parent
 */
export function findParent(
  element: ExternalElement,
  allElements: ExternalElement[],
): ParentInfo | null {
  if (!element.parent) {
    return null;
  }

  const elementMap = buildElementMap(allElements);
  const parent = elementMap.get(element.parent);

  if (!parent) {
    return null;
  }

  return {
    elementId: parent.id,
    tagName: parent.tagName.toLowerCase(),
    type: parent.type,
    label: getElementLabel(parent),
    role: parent.role || parent.accessibility?.role,
  };
}

/**
 * Find sibling elements (elements with the same parent)
 *
 * @param element - The element to find siblings of
 * @param allElements - All elements in the page
 * @returns Array of SiblingInfo, with current element marked
 */
export function findSiblings(
  element: ExternalElement,
  allElements: ExternalElement[],
): SiblingInfo[] {
  const elementMap = buildElementMap(allElements);

  // If no parent, no siblings
  if (!element.parent) {
    return [
      {
        elementId: element.id,
        tagName: element.tagName.toLowerCase(),
        type: element.type,
        label: getElementLabel(element),
        role: element.role || element.accessibility?.role,
        isCurrent: true,
      },
    ];
  }

  const parent = elementMap.get(element.parent);
  if (!parent || !parent.children) {
    return [
      {
        elementId: element.id,
        tagName: element.tagName.toLowerCase(),
        type: element.type,
        label: getElementLabel(element),
        role: element.role || element.accessibility?.role,
        isCurrent: true,
      },
    ];
  }

  // Get all siblings from parent's children list
  const siblings: SiblingInfo[] = [];
  for (const childId of parent.children) {
    const sibling = elementMap.get(childId);
    if (sibling) {
      siblings.push({
        elementId: sibling.id,
        tagName: sibling.tagName.toLowerCase(),
        type: sibling.type,
        label: getElementLabel(sibling),
        role: sibling.role || sibling.accessibility?.role,
        isCurrent: sibling.id === element.id,
      });
    }
  }

  // If the current element isn't in the siblings list, add it
  if (!siblings.some((s) => s.isCurrent)) {
    siblings.push({
      elementId: element.id,
      tagName: element.tagName.toLowerCase(),
      type: element.type,
      label: getElementLabel(element),
      role: element.role || element.accessibility?.role,
      isCurrent: true,
    });
  }

  return siblings;
}

/**
 * ARIA landmark roles
 */
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

/**
 * HTML elements that have implicit landmark roles
 */
const IMPLICIT_LANDMARKS: Record<string, string> = {
  header: "banner",
  nav: "navigation",
  main: "main",
  aside: "complementary",
  footer: "contentinfo",
  form: "form",
  search: "search",
  section: "region",
};

/**
 * Find the nearest ARIA landmark ancestor
 *
 * @param element - The element to find the landmark for
 * @param allElements - All elements in the page
 * @returns LandmarkInfo or null if no landmark found
 */
export function findNearestLandmark(
  element: ExternalElement,
  allElements: ExternalElement[],
): LandmarkInfo | null {
  const elementMap = buildElementMap(allElements);

  // First check fingerprint for landmark info
  if (element.fingerprint) {
    const { landmarkContext, landmarkLabel, positionZone } = element.fingerprint;

    if (landmarkContext && landmarkContext !== "none") {
      return {
        role: landmarkContext,
        label: landmarkLabel,
        positionZone,
      };
    }

    // If we have position zone but no landmark context, we can infer some info
    if (positionZone) {
      const zoneToRole: Record<string, string> = {
        header: "banner",
        footer: "contentinfo",
        main: "main",
        "sidebar-left": "complementary",
        "sidebar-right": "complementary",
      };

      if (zoneToRole[positionZone]) {
        return {
          role: zoneToRole[positionZone],
          positionZone,
        };
      }
    }
  }

  // Walk up the parent chain looking for landmarks
  let current: ExternalElement | undefined = element;
  while (current) {
    const role = current.role || current.accessibility?.role;

    // Check for explicit landmark role
    if (role && LANDMARK_ROLES.has(role)) {
      return {
        role,
        elementId: current.id,
        label: current.accessibility?.ariaLabel || getElementLabel(current),
      };
    }

    // Check for implicit landmark from tag name
    const implicitRole = IMPLICIT_LANDMARKS[current.tagName.toLowerCase()];
    if (implicitRole) {
      return {
        role: implicitRole,
        elementId: current.id,
        label: current.accessibility?.ariaLabel || getElementLabel(current),
      };
    }

    // Move to parent
    if (current.parent) {
      current = elementMap.get(current.parent);
    } else {
      break;
    }
  }

  return null;
}

/**
 * Build complete DOM context for an element
 *
 * @param element - The element to build context for
 * @param allElements - All elements in the page
 * @returns DOMContext with path, parent, siblings, and landmark info
 */
export function buildDOMContext(
  element: ExternalElement,
  allElements: ExternalElement[],
): DOMContext {
  return {
    path: buildElementPath(element, allElements),
    parent: findParent(element, allElements),
    siblings: findSiblings(element, allElements),
    landmark: findNearestLandmark(element, allElements),
  };
}

/**
 * Format a path segment for display
 */
export function formatPathSegment(segment: PathSegment): string {
  const parts: string[] = [segment.tagName];

  if (segment.role && segment.role !== segment.tagName) {
    parts.push(`[${segment.role}]`);
  }

  if (segment.label) {
    // Truncate long labels
    const maxLen = 20;
    const label =
      segment.label.length > maxLen ? segment.label.slice(0, maxLen) + "..." : segment.label;
    parts.push(`"${label}"`);
  }

  return parts.join(" ");
}

/**
 * Get a human-readable landmark name
 */
export function getLandmarkDisplayName(role: string): string {
  const displayNames: Record<string, string> = {
    banner: "Header",
    navigation: "Navigation",
    main: "Main Content",
    complementary: "Sidebar",
    contentinfo: "Footer",
    form: "Form",
    search: "Search",
    region: "Region",
  };

  return displayNames[role] || role;
}
