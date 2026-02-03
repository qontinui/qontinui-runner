/**
 * Cross-View Navigation Types and Utilities
 *
 * Provides navigation context and breadcrumb support for navigating
 * between States, Transitions, and Fingerprints across different views.
 */

// =============================================================================
// Types
// =============================================================================

/** Types of items that can be navigated to */
export type NavigationItemType = "graph" | "state" | "transition" | "fingerprint";

/** A single item in the navigation history */
export interface NavigationItem {
  /** Type of item */
  type: NavigationItemType;
  /** Unique identifier (stateId, transitionKey, fingerprintHash) */
  id: string;
  /** Display label for breadcrumb */
  label: string;
  /** Timestamp when navigated */
  timestamp: number;
}

/** Navigation history stack */
export interface NavigationHistory {
  /** Stack of navigation items (most recent last) */
  items: NavigationItem[];
  /** Maximum items to keep in history */
  maxItems: number;
}

/** Current navigation state */
export interface NavigationState {
  /** Currently selected state ID */
  selectedStateId: string | null;
  /** Currently selected transition key (format: "fromStateId->toStateId") */
  selectedTransitionId: string | null;
  /** Currently selected fingerprint hash */
  selectedFingerprintId: string | null;
  /** Navigation history for breadcrumbs */
  history: NavigationHistory;
  /** Current active view */
  activeView: "states" | "graph" | "transitions" | "matrix" | "stats";
}

/** Navigation actions */
export interface NavigationActions {
  /** Navigate to a state */
  navigateToState: (stateId: string, stateName: string) => void;
  /** Navigate to a transition */
  navigateToTransition: (fromStateId: string, toStateId: string, actionType?: string) => void;
  /** Navigate to a fingerprint */
  navigateToFingerprint: (hash: string, label?: string) => void;
  /** Navigate to graph view */
  navigateToGraph: () => void;
  /** Go back in navigation history */
  navigateBack: () => void;
  /** Clear selection */
  clearSelection: () => void;
  /** Set active view without adding to history */
  setActiveView: (view: NavigationState["activeView"]) => void;
}

// =============================================================================
// Utilities
// =============================================================================

/** Create initial navigation state */
export function createInitialNavigationState(): NavigationState {
  return {
    selectedStateId: null,
    selectedTransitionId: null,
    selectedFingerprintId: null,
    history: {
      items: [],
      maxItems: 5,
    },
    activeView: "states",
  };
}

/** Add item to navigation history */
export function addToHistory(
  history: NavigationHistory,
  item: Omit<NavigationItem, "timestamp">,
): NavigationHistory {
  const newItem: NavigationItem = {
    ...item,
    timestamp: Date.now(),
  };

  // Don't add duplicate of the last item
  const lastItem = history.items[history.items.length - 1];
  if (lastItem && lastItem.type === newItem.type && lastItem.id === newItem.id) {
    return history;
  }

  // Add new item and trim to max
  const newItems = [...history.items, newItem];
  if (newItems.length > history.maxItems) {
    newItems.shift();
  }

  return {
    ...history,
    items: newItems,
  };
}

/** Remove last item from navigation history */
export function popHistory(history: NavigationHistory): {
  history: NavigationHistory;
  item: NavigationItem | null;
} {
  if (history.items.length === 0) {
    return { history, item: null };
  }

  const newItems = [...history.items];
  const item = newItems.pop() ?? null;

  return {
    history: { ...history, items: newItems },
    item,
  };
}

/** Create transition key from state IDs */
export function createTransitionKey(fromStateId: string, toStateId: string): string {
  return `${fromStateId}->${toStateId}`;
}

/** Parse transition key into state IDs */
export function parseTransitionKey(key: string): { fromStateId: string; toStateId: string } | null {
  const parts = key.split("->");
  if (parts.length !== 2 || !parts[0] || !parts[1]) {
    return null;
  }
  return {
    fromStateId: parts[0],
    toStateId: parts[1],
  };
}

/** Truncate label for display */
export function truncateLabel(label: string, maxLength: number = 20): string {
  if (label.length <= maxLength) {
    return label;
  }
  return label.slice(0, maxLength - 3) + "...";
}

/** Get icon name for navigation item type */
export function getNavigationItemIcon(type: NavigationItemType): string {
  switch (type) {
    case "graph":
      return "Share2";
    case "state":
      return "Layers";
    case "transition":
      return "GitBranch";
    case "fingerprint":
      return "Fingerprint";
    default:
      return "Circle";
  }
}
