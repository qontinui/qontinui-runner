/**
 * ActionLogTable Component
 *
 * A hierarchical tabular view of action execution logs with tree-style visualization.
 * Displays pre-filtered actions received from the backend DisplayProfile system.
 *
 * Features:
 * - Tree-line visualization showing parent-child relationships
 * - Dynamic Type column width that adjusts to deepest nesting level
 * - Level filter dropdown to control maximum visible depth (default: 2)
 * - Colored circle toggle buttons (green + to expand, red - to collapse)
 * - 7-column table layout (Level, Action Type, Target, Result, Active States, Timestamp, Duration)
 * - Level-based color coding using Tailwind gray shades
 * - Formatted result display based on action type and execution status
 * - Relative timestamps from workflow start
 * - Row click handling for action selection
 * - Hover effects and zebra striping
 * - Fixed header with scrollable body
 *
 * NOTE: This component follows SRP - it ONLY renders the table. All filtering logic
 * has been moved to the backend DisplayProfile system.
 */

import React, { useState, useMemo } from "react";
import { ActionLogEntry } from "../types/displayProfile";
import { getAccentColors, getStatusColors } from "@/design-system";

/**
 * Props for the ActionLogTable component
 */
interface ActionLogTableProps {
  /** Pre-filtered list of actions from the backend DisplayProfile */
  actions: ActionLogEntry[];

  /** Callback fired when a row is clicked */
  onRowClick: (action: ActionLogEntry) => void;

  /** Workflow start time for calculating relative timestamps (Unix epoch in seconds) */
  workflowStartTime?: number;
}

/**
 * Safely extract a string value from a potentially nested object
 */
function safeStringify(value: unknown): string {
  if (value === null || value === undefined) {
    return "";
  }
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  if (typeof value === "object") {
    // If it's an object with an imageId property, extract that
    if ("imageId" in value) {
      return safeStringify(value.imageId);
    }
    // If it's an object with a name property, extract that
    if ("name" in value) {
      return safeStringify(value.name);
    }
    // If it's an object with an id property, extract that
    if ("id" in value) {
      return safeStringify(value.id);
    }
    // Otherwise, return JSON representation
    return JSON.stringify(value);
  }
  return String(value);
}

/**
 * Extract the target description from an action
 */
function formatTarget(action: ActionLogEntry): string {
  const config = (action.metadata?.config || {}) as Record<string, unknown>;

  switch (action.action_type) {
    case "Outgoing Transition":
    case "Incoming Transition": {
      // For transitions, show the target states from metadata
      const targetStates = action.metadata?.target_states as string[] | undefined;
      if (targetStates && Array.isArray(targetStates)) {
        return targetStates.join(", ");
      }
      return "-";
    }

    case "FIND":
      // Handle multiple images (new format)
      if (config.imageNames && Array.isArray(config.imageNames)) {
        const names = config.imageNames;
        if (names.length === 1) {
          return safeStringify(names[0]);
        } else if (names.length <= 3) {
          return names.join(", ");
        } else {
          return `${names.slice(0, 2).join(", ")}, +${names.length - 2} more`;
        }
      }
      // Handle single image (includes backward compat string from enrichment)
      if ("imageName" in config) {
        return safeStringify(config.imageName);
      } else if (
        "target" in config &&
        config.target &&
        typeof config.target === "object" &&
        "imageName" in config.target
      ) {
        return safeStringify(config.target.imageName);
      } else if (
        "target" in config &&
        config.target &&
        typeof config.target === "object" &&
        "imageId" in config.target
      ) {
        return safeStringify(config.target.imageId);
      } else if ("imageId" in config) {
        return safeStringify(config.imageId);
      } else if (
        "target" in config &&
        config.target &&
        typeof config.target === "object" &&
        "imageIds" in config.target &&
        Array.isArray(config.target.imageIds)
      ) {
        // Fallback to IDs if names not available
        const ids = config.target.imageIds as unknown[];
        if (ids.length === 1) {
          return safeStringify(ids[0]);
        } else {
          return `${ids.length} images`;
        }
      } else if ("imageIds" in config && Array.isArray(config.imageIds)) {
        // Fallback to IDs if names not available
        const ids = config.imageIds as unknown[];
        if (ids.length === 1) {
          return safeStringify(ids[0]);
        } else {
          return `${ids.length} images`;
        }
      } else if ("image" in config) {
        return safeStringify(config.image);
      }
      return "-";

    case "TYPE": {
      // Show the actual text that was typed (from runtime) or configured text
      const runtime = action.metadata?.runtime as Record<string, unknown> | undefined;

      // First check runtime for the actual typed text
      if (runtime && "typed_text" in runtime) {
        return `"${safeStringify(runtime.typed_text)}"`;
      }

      // Fall back to config values
      if ("text" in config) {
        return `"${safeStringify(config.text)}"`;
      } else if ("textToType" in config) {
        return `"${safeStringify(config.textToType)}"`;
      } else if (
        "textSource" in config &&
        config.textSource &&
        typeof config.textSource === "object" &&
        "text" in config.textSource
      ) {
        return `"${safeStringify(config.textSource.text)}"`;
      } else if (
        "textSource" in config &&
        config.textSource &&
        typeof config.textSource === "object" &&
        "stateId" in config.textSource
      ) {
        // Text from state reference - show the state name as fallback
        return `text from ${safeStringify(config.textSource.stateId)}`;
      }
      return "-";
    }

    case "CLICK":
      if (
        "target" in config &&
        config.target &&
        typeof config.target === "object" &&
        "type" in config.target &&
        config.target.type === "lastFindResult"
      ) {
        return "last find result";
      } else if (
        "target" in config &&
        config.target &&
        typeof config.target === "object" &&
        "type" in config.target &&
        config.target.type === "currentPosition"
      ) {
        return "current position";
      } else if (
        "target" in config &&
        config.target &&
        typeof config.target === "object" &&
        "type" in config.target &&
        config.target.type === "image" &&
        "imageId" in config.target
      ) {
        return safeStringify(config.target.imageId);
      } else if ("x" in config && "y" in config) {
        return `(${config.x}, ${config.y})`;
      }
      return "-";

    case "GO_TO_STATE":
      // Show state names
      if (config.stateNames && Array.isArray(config.stateNames)) {
        return config.stateNames.join(", ");
      } else if (config.stateIds && Array.isArray(config.stateIds)) {
        return config.stateIds.map(safeStringify).join(", ");
      }
      return "unknown state";

    case "RUN_WORKFLOW": {
      // Try to get workflow name from runtime data, fall back to ID
      const workflowRuntime = action.metadata?.runtime as Record<string, unknown> | undefined;
      if (workflowRuntime?.workflow_name) {
        return safeStringify(workflowRuntime.workflow_name);
      } else if (config.workflowName) {
        return safeStringify(config.workflowName);
      } else if (config.workflowId) {
        return safeStringify(config.workflowId);
      } else if (action.metadata?.workflow_name) {
        return safeStringify(action.metadata.workflow_name);
      }
      return "-";
    }

    case "WAIT":
      if ("duration" in config && typeof config.duration === "number") {
        const seconds = config.duration / 1000;
        return `${seconds}s`;
      }
      return "-";

    case "MOUSE_MOVE":
      if (
        "target" in config &&
        config.target &&
        typeof config.target === "object" &&
        "type" in config.target &&
        config.target.type === "lastFindResult"
      ) {
        return "to last find result";
      } else if (
        "target" in config &&
        config.target &&
        typeof config.target === "object" &&
        "type" in config.target &&
        config.target.type === "image" &&
        "imageId" in config.target
      ) {
        return `to ${safeStringify(config.target.imageId)}`;
      }
      return "-";

    default:
      // Generic handling
      if (
        "target" in config &&
        config.target &&
        typeof config.target === "object" &&
        "imageId" in config.target
      ) {
        return safeStringify(config.target.imageId);
      } else if ("imageId" in config) {
        return safeStringify(config.imageId);
      } else if ("target" in config) {
        return safeStringify(config.target);
      }
      return "-";
  }
}

/**
 * Format the execution result based on action type and runtime data
 */
function formatResult(action: ActionLogEntry): React.ReactNode {
  const config = (action.metadata?.config || {}) as Record<string, unknown>;

  // Check status first
  if (action.status === "failed") {
    return (
      <span className={`${getStatusColors("error").text} font-medium`}>
        <span className="mr-1">✗</span>
        {action.error || "Failed"}
      </span>
    );
  }

  if (action.status === "success") {
    // Format success details based on action type
    switch (action.action_type) {
      case "FIND": {
        // Check for match result in metadata
        const matchResult = action.metadata?.matchResult;
        if (matchResult) {
          if (Array.isArray(matchResult)) {
            // Multiple matches
            const best = matchResult[0];
            if (best && typeof best === "object" && "confidence" in best) {
              const typedBest = best as { confidence: number };
              return (
                <span className={getStatusColors("success").text}>
                  <span className="mr-1">✓</span>
                  found {matchResult.length} matches, best:{" "}
                  {(typedBest.confidence * 100).toFixed(1)}%
                </span>
              );
            }
          } else if (
            typeof matchResult === "object" &&
            matchResult !== null &&
            "x" in matchResult &&
            "y" in matchResult &&
            "w" in matchResult &&
            "h" in matchResult &&
            "confidence" in matchResult
          ) {
            // Single match
            const typedMatch = matchResult as {
              x: number;
              y: number;
              w: number;
              h: number;
              confidence: number;
            };
            return (
              <span className={getStatusColors("success").text}>
                <span className="mr-1">✓</span>
                found [{typedMatch.x},{typedMatch.y},{typedMatch.w},{typedMatch.h}]{" "}
                {(typedMatch.confidence * 100).toFixed(1)}%
              </span>
            );
          }
        }
        return (
          <span className={getStatusColors("success").text}>
            <span className="mr-1">✓</span>found
          </span>
        );
      }

      case "TYPE":
        // Text is already shown in Target column, just show success indicator
        return (
          <span className={getStatusColors("success").text}>
            <span className="mr-1">✓</span>typed
          </span>
        );

      case "CLICK": {
        const clickPosition = action.metadata?.clickPosition;
        if (
          clickPosition &&
          typeof clickPosition === "object" &&
          "x" in clickPosition &&
          "y" in clickPosition
        ) {
          const typedPosition = clickPosition as { x: number; y: number };
          return (
            <span className={getStatusColors("success").text}>
              <span className="mr-1">✓</span>
              clicked ({typedPosition.x},{typedPosition.y})
            </span>
          );
        }
        return (
          <span className={getStatusColors("success").text}>
            <span className="mr-1">✓</span>clicked
          </span>
        );
      }

      case "GO_TO_STATE":
        if (config.stateNames && Array.isArray(config.stateNames)) {
          return (
            <span className={getStatusColors("success").text}>
              <span className="mr-1">✓</span>→ {config.stateNames.join(", ")}
            </span>
          );
        }
        return (
          <span className={getStatusColors("success").text}>
            <span className="mr-1">✓</span>state activated
          </span>
        );

      default:
        return (
          <span className={getStatusColors("success").text}>
            <span className="mr-1">✓</span>success
          </span>
        );
    }
  }

  if (action.status === "running") {
    return <span className={getStatusColors("running").text}>running...</span>;
  }

  return <span className={getStatusColors("pending").text}>pending</span>;
}

/**
 * Extract active states after this action from state_context
 */
function formatActiveStates(action: ActionLogEntry): string {
  const stateContext = action.metadata?.state_context as { active_after?: string[] } | undefined;
  const activeStates = stateContext?.active_after;
  if (activeStates && Array.isArray(activeStates) && activeStates.length > 0) {
    return activeStates.join(", ");
  }
  return "-";
}

/**
 * Format timestamp relative to workflow start time
 */
function formatTimestamp(timestamp: number, startTime?: number): string {
  if (startTime === undefined) {
    // No start time - show absolute time
    return new Date(timestamp * 1000).toLocaleTimeString("en-US", {
      hour12: false,
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }

  // Calculate relative time
  const relativeSeconds = timestamp - startTime;

  if (relativeSeconds < 60) {
    // Less than 1 minute - show seconds with 2 decimals
    return `+${relativeSeconds.toFixed(2)}s`;
  } else {
    // 1 minute or more - show minutes and seconds
    const minutes = Math.floor(relativeSeconds / 60);
    const seconds = (relativeSeconds % 60).toFixed(0);
    return `+${minutes}m ${seconds}s`;
  }
}

/**
 * Format duration in milliseconds or seconds
 */
function formatDuration(durationSeconds: number | null | undefined): string {
  if (durationSeconds === undefined || durationSeconds === null) {
    return "-";
  }

  const ms = durationSeconds * 1000;

  if (ms < 1000) {
    // Less than 1 second - show milliseconds
    return `${Math.round(ms)}ms`;
  } else {
    // 1 second or more - show seconds with 2 decimal places
    return `${(ms / 1000).toFixed(2)}s`;
  }
}

/**
 * Get the background and text color for a level
 * Uses Tailwind gray shades: gray-100 (level 0) → gray-900 (level 10+)
 * Note: Must use explicit class names for Tailwind JIT to work
 */
function getLevelColor(level: number): { bg: string; text: string } {
  // Clamp level between 0 and 10
  const clampedLevel = Math.max(0, Math.min(10, level));

  // Map level to explicit Tailwind classes (JIT requires explicit class names)
  const colorMap: { bg: string; text: string }[] = [
    { bg: "bg-gray-100", text: "text-gray-900" }, // Level 0
    { bg: "bg-gray-200", text: "text-gray-900" }, // Level 1
    { bg: "bg-gray-300", text: "text-gray-900" }, // Level 2
    { bg: "bg-gray-400", text: "text-gray-900" }, // Level 3
    { bg: "bg-gray-500", text: "text-gray-900" }, // Level 4
    { bg: "bg-gray-600", text: "text-white" }, // Level 5
    { bg: "bg-gray-700", text: "text-white" }, // Level 6
    { bg: "bg-gray-800", text: "text-white" }, // Level 7
    { bg: "bg-gray-900", text: "text-white" }, // Level 8
    { bg: "bg-gray-950", text: "text-white" }, // Level 9
    { bg: "bg-gray-950", text: "text-white" }, // Level 10+
  ];

  return colorMap[clampedLevel];
}

/**
 * Calculate nesting level from metadata
 */
function getNestingLevel(action: ActionLogEntry): number {
  return (action.metadata?.level as number) || 0;
}

/**
 * Calculate the depth of an action in the tree (distance from root)
 */
function calculateDepth(action: ActionLogEntry, actions: ActionLogEntry[]): number {
  let depth = 0;
  let currentParentId = action.parent_action_id;

  while (currentParentId) {
    depth++;
    const parent = actions.find((a) => a.id === currentParentId);
    currentParentId = parent?.parent_action_id || null;
  }

  return depth;
}

/**
 * Generate tree line prefix for an action based on its position in hierarchy
 */
function getTreePrefix(
  action: ActionLogEntry,
  actions: ActionLogEntry[],
  parentChildMap: Map<string, string[]>,
  visibleActions: ActionLogEntry[],
): string {
  if (!action.parent_action_id) {
    // Root level - no prefix
    return "";
  }

  let prefix = "";

  // Build the ancestor chain from this action up to root
  const ancestors: ActionLogEntry[] = [];
  let current: ActionLogEntry | undefined = action;

  while (current && current.parent_action_id) {
    const parent = actions.find((a) => a.id === current.parent_action_id);
    if (parent) {
      ancestors.unshift(parent);
    }
    current = parent;
  }

  // Build prefix by checking each ancestor level
  // For each ancestor, we need to check if it has more siblings below it
  for (let i = 0; i < ancestors.length; i++) {
    const ancestor = ancestors[i];
    const ancestorParentId = ancestor.parent_action_id || "";
    const siblings = parentChildMap.get(ancestorParentId) || [];

    // Filter siblings to only include visible ones
    const visibleSiblings = siblings.filter((sibId) =>
      visibleActions.some((a) => a && a.id === sibId),
    );

    const isLastSibling = visibleSiblings.indexOf(ancestor.id) === visibleSiblings.length - 1;

    // For all ancestors, add continuation line if they have siblings below
    prefix += isLastSibling ? "   " : "│  ";
  }

  // Add the final branch character for this action itself
  if (ancestors.length === 0) {
    // Safety check: if no ancestors, just return the branch character
    return "├─ ";
  }

  const parent = ancestors[ancestors.length - 1];
  const ourSiblings = parentChildMap.get(parent.id) || [];
  const visibleOurSiblings = ourSiblings.filter((sibId) =>
    visibleActions.some((a) => a && a.id === sibId),
  );
  const isLastChild = visibleOurSiblings.indexOf(action.id) === visibleOurSiblings.length - 1;
  prefix += isLastChild ? "└─ " : "├─ ";

  return prefix;
}

/**
 * Calculate the maximum width needed for the Type column with tree prefixes
 */
function calculateTypeColumnWidth(
  actions: ActionLogEntry[],
  parentChildMap: Map<string, string[]>,
): number {
  let maxWidth = 0;

  for (const action of actions) {
    const _depth = calculateDepth(action, actions);
    // For width calculation, assume all actions are visible (worst case)
    const prefix = getTreePrefix(action, actions, parentChildMap, actions);
    // Each character in prefix is ~0.6em in monospace font
    const prefixWidth = prefix.length * 0.6;
    // Action type text in em units (rough estimate)
    const textWidth = action.action_type.length * 0.6;
    const totalWidth = prefixWidth + textWidth;

    maxWidth = Math.max(maxWidth, totalWidth);
  }

  // Return in rem units with some padding
  return Math.ceil(maxWidth) + 2;
}

/**
 * ActionLogTable component
 */
export default function ActionLogTable({
  actions,
  onRowClick,
  workflowStartTime,
}: ActionLogTableProps) {
  // Track which actions are collapsed (hidden children)
  const [collapsedActions, setCollapsedActions] = useState<Set<string>>(new Set());

  // Maximum level to display (default: 2)
  const [maxLevel, setMaxLevel] = useState<number>(2);

  // Toggle expand/collapse for an action
  const toggleExpand = (actionId: string) => {
    setCollapsedActions((prev) => {
      const next = new Set(prev);
      if (next.has(actionId)) {
        next.delete(actionId);
      } else {
        next.add(actionId);
      }
      return next;
    });
  };

  // Build a map of parent-child relationships
  const parentChildMap = useMemo(() => {
    const map = new Map<string, string[]>();
    for (const action of actions) {
      if (action.parent_action_id) {
        const children = map.get(action.parent_action_id) || [];
        children.push(action.id);
        map.set(action.parent_action_id, children);
      }
    }
    return map;
  }, [actions]);

  // When maxLevel changes, auto-collapse items at maxLevel that have deeper children
  React.useEffect(() => {
    // Build the ideal collapsed set based on maxLevel
    const idealCollapsed = new Set<string>();

    for (const action of actions) {
      const level = getNestingLevel(action);
      const children = parentChildMap.get(action.id) || [];

      // Check if this action is at maxLevel and has children beyond maxLevel
      const hasChildrenBeyondMax = children.some((childId) => {
        const child = actions.find((a) => a.id === childId);
        return child && getNestingLevel(child) > maxLevel;
      });

      // If at maxLevel with deeper children, it should be collapsed
      if (level === maxLevel && hasChildrenBeyondMax) {
        idealCollapsed.add(action.id);
      }
    }

    // Only update if the ideal state differs from current state
    setCollapsedActions((prev) => {
      const needsUpdate =
        idealCollapsed.size !== prev.size || Array.from(idealCollapsed).some((id) => !prev.has(id));

      return needsUpdate ? idealCollapsed : prev;
    });
  }, [maxLevel, actions, parentChildMap]); // Don't include collapsedActions in deps!

  // Filter visible actions based on collapsed state and max level (needs to be before typeColumnWidth)
  const visibleActions = useMemo(() => {
    const visible: ActionLogEntry[] = [];

    // Build a set of expanded ancestors for quick lookup
    const _expandedPaths = new Set<string>();

    for (const action of actions) {
      const level = getNestingLevel(action);

      // Check if any ancestor is collapsed or if we should show this based on maxLevel
      let isCollapsedByAncestor = false;
      let _isExplicitlyExpanded = false;
      let currentParent = action.parent_action_id;

      // Track all ancestors
      const ancestorChain: ActionLogEntry[] = [];
      while (currentParent) {
        const parentAction = actions.find((a) => a.id === currentParent);
        if (!parentAction) break;

        ancestorChain.unshift(parentAction);

        // If parent is collapsed, this action is hidden
        if (collapsedActions.has(currentParent)) {
          isCollapsedByAncestor = true;
          break;
        }

        currentParent = parentAction.parent_action_id || null;
      }

      // An action is visible if:
      // 1. No ancestor is collapsed, AND
      // 2. It's within maxLevel OR parent at maxLevel was manually expanded
      const withinMaxLevel = level <= maxLevel;

      // Check if immediate parent is at maxLevel and has been expanded to show deeper children
      let parentAtMaxLevelIsExpanded = false;
      if (action.parent_action_id && level > maxLevel) {
        const parent = actions.find((a) => a.id === action.parent_action_id);
        if (parent && getNestingLevel(parent) === maxLevel) {
          // Parent is at maxLevel - only show if NOT in collapsedActions
          // (NOT in collapsedActions means user clicked to expand)
          parentAtMaxLevelIsExpanded = !collapsedActions.has(action.parent_action_id);
        } else if (parent && getNestingLevel(parent) > maxLevel) {
          // Parent is also beyond maxLevel - check if it's visible (recursive)
          parentAtMaxLevelIsExpanded = visible.some((v) => v.id === action.parent_action_id);
        }
      }

      // Show if: not collapsed AND (within level OR parent at maxLevel is expanded)
      const shouldShow = withinMaxLevel || parentAtMaxLevelIsExpanded;

      if (!isCollapsedByAncestor && shouldShow) {
        visible.push(action);
      }
    }

    return visible;
  }, [actions, collapsedActions, maxLevel]);

  // Calculate dynamic Type column width
  const typeColumnWidth = useMemo(() => {
    return calculateTypeColumnWidth(actions, parentChildMap);
  }, [actions, parentChildMap]);

  // Get the maximum level present in the data
  const maxAvailableLevel = useMemo(() => {
    return Math.max(...actions.map((a) => getNestingLevel(a)));
  }, [actions]);

  if (actions.length === 0) {
    return (
      <div className="text-center text-text-muted py-8">
        <p>No actions to display</p>
      </div>
    );
  }

  return (
    <div className="w-full overflow-auto">
      {/* Level Filter Dropdown */}
      <div className="mb-4 flex items-center gap-2">
        <label htmlFor="level-filter" className="text-sm font-medium text-foreground">
          Max Level:
        </label>
        <select
          id="level-filter"
          value={maxLevel}
          onChange={(e) => setMaxLevel(Number(e.target.value))}
          className="px-3 py-1 bg-surface-raised text-foreground border border-border-default rounded focus:outline-none focus:ring-2 focus:ring-blue-500"
        >
          {Array.from({ length: maxAvailableLevel + 1 }, (_, i) => (
            <option key={i} value={i}>
              {i}
            </option>
          ))}
        </select>
        <span className="text-xs text-text-muted">(Showing levels 0-{maxLevel})</span>
      </div>

      <table className="w-full border-collapse">
        {/* Fixed Header */}
        <thead className="sticky top-0 bg-surface-raised text-foreground z-10">
          <tr>
            <th className="px-3 py-2 text-center text-sm font-semibold border-b-2 border-border-default w-16">
              Level
            </th>
            <th className="px-3 py-2 text-left text-sm font-semibold border-b-2 border-border-default w-8">
              {/* Expand/collapse column */}
            </th>
            <th
              className="px-3 py-2 text-left text-sm font-semibold border-b-2 border-border-default"
              style={{ minWidth: `${typeColumnWidth}rem` }}
            >
              Action Type
            </th>
            <th className="px-3 py-2 text-left text-sm font-semibold border-b-2 border-border-default">
              Target
            </th>
            <th className="px-3 py-2 text-left text-sm font-semibold border-b-2 border-border-default min-w-[200px]">
              Result
            </th>
            <th className="px-3 py-2 text-left text-sm font-semibold border-b-2 border-border-default">
              Active States
            </th>
            <th className="px-3 py-2 text-right text-sm font-semibold border-b-2 border-border-default">
              Timestamp
            </th>
            <th className="px-3 py-2 text-right text-sm font-semibold border-b-2 border-border-default">
              Duration
            </th>
          </tr>
        </thead>

        {/* Table Body */}
        <tbody>
          {visibleActions.map((action, index) => {
            const level = getNestingLevel(action);
            const levelColors = getLevelColor(level);
            const isEven = index % 2 === 0;
            const _hasChildren = parentChildMap.has(action.id);
            const _isCollapsed = collapsedActions.has(action.id);
            const treePrefix = getTreePrefix(action, actions, parentChildMap, visibleActions);

            // Check if this action has children at all
            const children = parentChildMap.get(action.id) || [];
            const hasAnyChildren = children.length > 0;

            // Check if children are currently visible in the table
            const hasVisibleChildren = children.some((childId) =>
              visibleActions.some((a) => a.id === childId),
            );

            // Check if this action is at maxLevel (children would be filtered by default)
            const _isAtMaxLevel = level === maxLevel;

            // Check if children are beyond maxLevel (would need manual expand to see)
            const _hasChildrenBeyondMaxLevel = children.some((childId) => {
              const child = actions.find((a) => a.id === childId);
              return child && getNestingLevel(child) > maxLevel;
            });

            // An action/workflow should show a toggle button if:
            // 1. It has children (regardless of whether they're filtered by maxLevel), AND
            // 2. It's marked as expandable or is a transition
            const isTransition =
              action.action_type === "Outgoing Transition" ||
              action.action_type === "Incoming Transition";
            const isExpandable = (action.is_expandable || isTransition) && hasAnyChildren;

            // Button shows:
            // - "+" (green) when children exist but are NOT currently visible
            //   This happens when:
            //   a) User manually collapsed, OR
            //   b) Children are beyond maxLevel and not manually expanded
            // - "−" (red) when children are currently visible in the table
            const childrenAreHidden = !hasVisibleChildren;

            return (
              <tr
                key={action.id}
                className={`
                  cursor-pointer transition-colors
                  hover:bg-primary/10
                  ${isEven ? "bg-surface-canvas/20" : "bg-surface-canvas/5"}
                `}
              >
                {/* Level Badge */}
                <td className="px-3 py-1 text-center" onClick={() => onRowClick(action)}>
                  <span
                    className={`
                      inline-flex items-center justify-center
                      w-4 h-4 rounded
                      text-xs font-semibold
                      ${levelColors.bg} ${levelColors.text}
                    `}
                  >
                    {level}
                  </span>
                </td>

                {/* Expand/Collapse Button */}
                <td className="px-1 py-1 text-center">
                  {isExpandable ? (
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        toggleExpand(action.id);
                      }}
                      className={`
                        w-3 h-3 flex items-center justify-center rounded-full
                        text-white font-bold leading-none
                        transition-all hover:scale-125 hover:opacity-100
                        ${
                          childrenAreHidden
                            ? `${getAccentColors("green").bg} hover:opacity-80`
                            : `${getAccentColors("red").bg} hover:opacity-80`
                        }
                      `}
                      style={{ fontSize: "8px" }}
                      title={childrenAreHidden ? "Expand children" : "Collapse children"}
                    >
                      {childrenAreHidden ? "+" : "−"}
                    </button>
                  ) : null}
                </td>

                {/* Action Type with Tree Prefix */}
                <td
                  className="px-3 py-1 text-sm font-mono whitespace-nowrap overflow-hidden text-ellipsis"
                  onClick={() => onRowClick(action)}
                  style={{ maxWidth: `${typeColumnWidth}rem` }}
                  title={`${treePrefix}${action.action_type}`}
                >
                  <span className="text-text-muted" style={{ whiteSpace: "pre" }}>
                    {treePrefix}
                  </span>
                  {action.action_type}
                </td>

                {/* Target */}
                <td className="px-3 py-1 text-sm font-mono" onClick={() => onRowClick(action)}>
                  {formatTarget(action)}
                </td>

                {/* Result */}
                <td className="px-3 py-1 text-sm font-mono" onClick={() => onRowClick(action)}>
                  {formatResult(action)}
                </td>

                {/* Active States */}
                <td
                  className="px-3 py-1 text-sm font-mono text-text-muted"
                  onClick={() => onRowClick(action)}
                >
                  {formatActiveStates(action)}
                </td>

                {/* Timestamp */}
                <td
                  className="px-3 py-1 text-sm font-mono text-right text-text-muted"
                  onClick={() => onRowClick(action)}
                >
                  {formatTimestamp(action.timestamp, workflowStartTime)}
                </td>

                {/* Duration */}
                <td
                  className="px-3 py-1 text-sm font-mono text-right text-text-muted"
                  onClick={() => onRowClick(action)}
                >
                  {formatDuration(action.duration)}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
