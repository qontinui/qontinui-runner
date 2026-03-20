/**
 * TreeNode Component
 *
 * A reusable component for rendering a single node in the hierarchical event tree.
 * Supports expand/collapse functionality, indentation based on nesting level,
 * and different visual representations for workflows and actions.
 */

import React from "react";
import {
  ChevronRight,
  ChevronDown,
  CheckCircle2,
  XCircle,
  FolderOpen,
  Activity,
  Loader,
} from "lucide-react";
import { DisplayNode } from "../types/treeEvents";
import { getAccentColors, getStatusColors } from "@/design-system";

/**
 * Type for action config target with optional properties
 */
interface ActionTarget {
  type?: string;
  imageId?: string;
  imageName?: string;
  imageIds?: string[];
}

/**
 * Type for text source configuration
 */
interface TextSource {
  text?: string;
  stateId?: string;
}

/**
 * Type for action configuration with known properties
 * Allows additional unknown properties via index signature
 */
interface ActionConfig {
  text?: string;
  textToType?: string;
  textSource?: TextSource;
  target?: ActionTarget;
  x?: number;
  y?: number;
  imageNames?: string[];
  imageName?: string;
  imageId?: string;
  imageIds?: string[];
  image?: string;
  condition?: string;
  duration?: number;
  workflowId?: string;
  stateNames?: string[];
  stateIds?: string[];
  [key: string]: unknown;
}

interface TreeNodeProps {
  /**
   * The tree node to render
   */
  node: DisplayNode;

  /**
   * Whether this node is currently expanded
   */
  isExpanded: boolean;

  /**
   * Callback fired when the expand/collapse toggle is clicked
   */
  onToggle: (id: string) => void;

  /**
   * Set of all currently expanded node IDs (for recursion)
   */
  expandedNodes: Set<string>;

  /**
   * Optional level override for indentation calculation
   */
  level?: number;
}

/**
 * Helper function to format action details from metadata
 */
function formatActionDetails(node: DisplayNode): string {
  const details: string[] = [];
  const config = (node.metadata.config || {}) as ActionConfig;

  // Handle different action types
  switch (node.name) {
    case "TYPE":
      // Check for text in various possible locations
      // Priority: actual text > textToType > text from state reference
      if (config.text) {
        details.push(`"${config.text}"`);
      } else if (config.textToType) {
        details.push(`"${config.textToType}"`);
      } else if (config.textSource?.text) {
        details.push(`"${config.textSource.text}"`);
      } else if (config.textSource?.stateId) {
        // Text comes from a state's string - show both state name and actual text if available
        const stateRef = config.textSource.stateId;
        // Check if there's actual typed text in metadata (from execution result)
        if (node.metadata.typedText) {
          details.push(`"${node.metadata.typedText}" (from ${stateRef})`);
        } else {
          details.push(`text from ${stateRef}`);
        }
      }
      break;

    case "CLICK":
      if (config.target?.type === "lastFindResult") {
        details.push("on last find result");
      } else if (config.target?.type === "currentPosition") {
        details.push("at current position");
      } else if (config.target?.type === "image" && config.target.imageId) {
        details.push(`on "${config.target.imageId}"`);
      } else if (config.x !== undefined && config.y !== undefined) {
        details.push(`at (${config.x}, ${config.y})`);
      }
      break;

    case "FIND":
      // Handle multiple images (new format)
      if (config.imageNames && Array.isArray(config.imageNames)) {
        const names = config.imageNames;
        if (names.length === 1) {
          details.push(`"${names[0]}"`);
        } else if (names.length <= 3) {
          details.push(`"${names.join('", "')}"`);
        } else {
          details.push(`${names.length} images: "${names.slice(0, 2).join('", "')}", ...`);
        }
      }
      // Handle single image (includes backward compat string from enrichment)
      else if (config.imageName) {
        details.push(`"${config.imageName}"`);
      } else if (config.target?.imageName) {
        details.push(`"${config.target.imageName}"`);
      } else if (config.target?.imageId) {
        details.push(`"${config.target.imageId}"`);
      } else if (config.imageId) {
        details.push(`"${config.imageId}"`);
      } else if (config.target?.imageIds && Array.isArray(config.target.imageIds)) {
        // Fallback to IDs if names not available
        const ids = config.target.imageIds;
        if (ids.length === 1) {
          details.push(`"${ids[0]}"`);
        } else {
          details.push(`${ids.length} images`);
        }
      } else if (config.imageIds && Array.isArray(config.imageIds)) {
        // Fallback to IDs if names not available
        const ids = config.imageIds;
        if (ids.length === 1) {
          details.push(`"${ids[0]}"`);
        } else {
          details.push(`${ids.length} images`);
        }
      } else if (config.image) {
        details.push(`"${config.image}"`);
      }
      break;

    case "IF": {
      // Image ID is shown in main label as FIND, no need to repeat
      // Only show additional condition details if not image-based
      const target = config.target;
      const hasImageCondition =
        (typeof target === "object" && target !== null && target.imageId) || config.imageId;

      if (!hasImageCondition && config.condition) {
        details.push(`${config.condition}`);
      }
      break;
    }

    case "WAIT":
      if (config.duration !== undefined) {
        const seconds = config.duration / 1000;
        details.push(`${seconds}s`);
      }
      break;

    case "GO_TO_STATE":
      // State names are shown in main label, no need to repeat here
      break;

    case "RUN_WORKFLOW":
      if (config.workflowId) {
        details.push(`"${config.workflowId}"`);
      }
      break;

    case "MOUSE_MOVE":
      if (config.target?.type === "lastFindResult") {
        details.push("to last find result");
      } else if (config.target?.type === "image" && config.target.imageId) {
        details.push(`to "${config.target.imageId}"`);
      }
      break;

    default:
      // Generic handling
      if (config.target?.imageId) {
        details.push(`"${config.target.imageId}"`);
      } else if (config.imageId) {
        details.push(`"${config.imageId}"`);
      }
  }

  return details.length > 0 ? ` ${details.join(", ")}` : "";
}

/**
 * Calculate nesting level from parent chain
 */
function calculateNestingLevel(node: DisplayNode): number {
  let level = 0;
  let current: DisplayNode | null = node.parent;
  while (current) {
    level++;
    current = current.parent;
  }
  return level;
}

/**
 * TreeNode component for rendering hierarchical event nodes
 */
export const TreeNode: React.FC<TreeNodeProps> = ({
  node,
  isExpanded,
  onToggle,
  expandedNodes,
  level,
}) => {
  const nestingLevel = level !== undefined ? level : calculateNestingLevel(node);
  // Each level adds 20px of indentation
  const indentation = nestingLevel * 20;
  const isExpandable = node.metadata?.is_expandable || node.children.length > 0;
  const isWorkflow = node.node_type === "workflow";
  const isTransition = node.node_type === "transition";

  /**
   * Renders the toggle button for expandable nodes
   */
  const renderToggleButton = () => {
    if (!isExpandable) {
      return <div className="w-5 h-5" />; // Spacer for alignment
    }

    return (
      <button
        onClick={(e) => {
          e.stopPropagation();
          onToggle(node.id);
        }}
        className="w-5 h-5 flex items-center justify-center text-muted-foreground hover:text-foreground transition-colors"
        aria-label={isExpanded ? "Collapse" : "Expand"}
      >
        {isExpanded ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />}
      </button>
    );
  };

  /**
   * Renders the icon for the event type
   */
  const renderIcon = () => {
    if (isWorkflow) {
      return <FolderOpen className="w-4 h-4 text-primary" />;
    }
    if (isTransition) {
      return <Activity className={`w-4 h-4 ${getAccentColors("purple").text}`} />;
    }
    return <Activity className="w-4 h-4 text-secondary" />;
  };

  /**
   * Renders the success/failure indicator
   */
  const renderStatusIndicator = () => {
    switch (node.status) {
      case "success":
        return <CheckCircle2 className={`w-4 h-4 ${getStatusColors("success").icon}`} />;
      case "failed":
        return <XCircle className={`w-4 h-4 ${getStatusColors("error").icon}`} />;
      case "running":
        return <Loader className={`w-4 h-4 ${getStatusColors("running").icon} animate-spin`} />;
      case "pending":
      default:
        return null;
    }
  };

  /**
   * Format the timestamp for display
   */
  const formatTimestamp = (timestamp: number) => {
    return new Date(timestamp * 1000).toLocaleTimeString("en-US", {
      hour12: false,
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  };

  /**
   * Format duration for display
   */
  const formatDuration = (duration: number | null | undefined) => {
    if (duration === undefined || duration === null) {
      return null;
    }

    // Convert to milliseconds
    const ms = duration * 1000;

    if (ms < 1000) {
      // Less than 1 second - show milliseconds
      return `${Math.round(ms)}ms`;
    } else if (ms < 60000) {
      // Less than 1 minute - show seconds with 2 decimal places
      return `${(ms / 1000).toFixed(2)}s`;
    } else {
      // 1 minute or more - show minutes and seconds
      const minutes = Math.floor(ms / 60000);
      const seconds = ((ms % 60000) / 1000).toFixed(0);
      return `${minutes}m ${seconds}s`;
    }
  };

  /**
   * Get the display label for the node
   */
  const getNodeLabel = () => {
    if (isWorkflow) {
      return node.name;
    }

    if (isTransition) {
      // Transitions should show their type and target states
      const targetStates = node.metadata.target_states;
      if (targetStates && Array.isArray(targetStates)) {
        return `${node.name} (${targetStates.join(", ")})`;
      }
      return node.name;
    }

    // For actions, create descriptive labels
    const details = formatActionDetails(node);
    const config = (node.metadata.config || {}) as ActionConfig;

    // Override default labels for clarity
    switch (node.name) {
      case "GO_TO_STATE":
        if (config.stateNames && Array.isArray(config.stateNames)) {
          return `ACTIVATE STATE ${config.stateNames.join(", ")}`;
        } else if (config.stateIds && Array.isArray(config.stateIds)) {
          return `ACTIVATE STATE ${config.stateIds.join(", ")}`;
        }
        return "ACTIVATE STATE" + details;

      case "IF": {
        // IF actions that check for images should show as FIND
        const target = config.target;
        if (config.imageName) {
          return `FIND "${config.imageName}"`;
        } else if (target?.imageName) {
          return `FIND "${target.imageName}"`;
        } else if (target?.imageId) {
          return `FIND "${target.imageId}"`;
        } else if (config.imageId) {
          return `FIND "${config.imageId}"`;
        }
        return "IF" + details;
      }

      default:
        return node.name + details;
    }
  };

  return (
    <div className="space-y-1">
      {/* Node Row */}
      <div
        className="flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-accent/5 transition-colors cursor-pointer group"
        style={{ paddingLeft: `${8 + indentation}px` }}
        onClick={() => isExpandable && onToggle(node.id)}
        onKeyDown={(e) => {
          if ((e.key === "Enter" || e.key === " ") && isExpandable) onToggle(node.id);
        }}
        role={isExpandable ? "button" : undefined}
        tabIndex={isExpandable ? 0 : undefined}
      >
        {/* Toggle Button */}
        {renderToggleButton()}

        {/* Icon */}
        {renderIcon()}

        {/* Label */}
        <span
          className={`flex-1 font-mono text-sm ${
            node.status === "failed" ? getStatusColors("error").text : "text-foreground"
          }`}
        >
          {getNodeLabel()}
        </span>

        {/* Status Indicator */}
        {renderStatusIndicator()}

        {/* Duration (always visible if available) */}
        {node.duration !== undefined && (
          <span className="text-xs text-muted-foreground font-mono">
            {formatDuration(node.duration)}
          </span>
        )}

        {/* Timestamp (show on hover) */}
        <span className="text-xs text-muted-foreground opacity-0 group-hover:opacity-100 transition-opacity">
          {formatTimestamp(node.timestamp)}
        </span>
      </div>

      {/* Error Message */}
      {node.error && (
        <div
          className={`text-xs ${getStatusColors("error").text} ml-7 px-2 py-1 ${getStatusColors("error").bg} rounded border-l-2 ${getStatusColors("error").border}`}
          style={{ marginLeft: `${16 + indentation}px` }}
        >
          {node.error}
        </div>
      )}

      {/* Children */}
      {isExpanded && node.children.length > 0 && (
        <div>
          {node.children.flatMap((child) => {
            // Transitions should always be rendered (they're the structural containers)
            if (child.node_type === "transition") {
              return [
                <TreeNode
                  key={child.id}
                  node={child}
                  isExpanded={expandedNodes.has(child.id)}
                  onToggle={onToggle}
                  expandedNodes={expandedNodes}
                  level={nestingLevel + 1}
                />,
              ];
            }

            // Filter out inline workflow wrappers recursively
            if (child.node_type === "workflow" && child.children.length > 0) {
              const isInline = child.metadata.is_inline === true;
              const isUserWorkflow = child.metadata.is_expandable === true;

              console.log(
                `[TreeNode] Child workflow ${child.name}: isInline=${isInline}, isUserWorkflow=${isUserWorkflow}`,
              );

              // Skip inline workflows (transition workflows) and render their children instead
              if (isInline && !isUserWorkflow) {
                console.log(
                  `[TreeNode] Skipping inline workflow wrapper: ${child.name}, rendering its children`,
                );
                return child.children.map((grandchild) => (
                  <TreeNode
                    key={grandchild.id}
                    node={grandchild}
                    isExpanded={expandedNodes.has(grandchild.id)}
                    onToggle={onToggle}
                    expandedNodes={expandedNodes}
                    level={nestingLevel + 1}
                  />
                ));
              }
            }

            // Render this child normally
            return [
              <TreeNode
                key={child.id}
                node={child}
                isExpanded={expandedNodes.has(child.id)}
                onToggle={onToggle}
                expandedNodes={expandedNodes}
                level={nestingLevel + 1}
              />,
            ];
          })}
        </div>
      )}
    </div>
  );
};
