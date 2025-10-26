/**
 * TreeNode Component
 *
 * A reusable component for rendering a single node in the hierarchical event tree.
 * Supports expand/collapse functionality, indentation based on nesting level,
 * and different visual representations for workflows and actions.
 */

import React from 'react';
import { ChevronRight, ChevronDown, CheckCircle2, XCircle, FolderOpen, Activity } from 'lucide-react';
import { TreeNode as TreeNodeType } from '../utils/eventTree';
import { HierarchicalEvent, ActionExecutionEvent, WorkflowEvent } from '../types/events';

interface TreeNodeProps {
  /**
   * The tree node to render
   */
  node: TreeNodeType<HierarchicalEvent>;

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
 * Type guard to check if an event is a workflow event
 */
function isWorkflowEvent(event: HierarchicalEvent): event is WorkflowEvent {
  return 'is_workflow' in event && event.is_workflow === true;
}

/**
 * Type guard to check if an event is an action execution event
 */
function isActionEvent(event: HierarchicalEvent): event is ActionExecutionEvent {
  return !isWorkflowEvent(event);
}

/**
 * Helper function to format action details for display
 */
function formatActionDetails(action: ActionExecutionEvent): string {
  const details: string[] = [];
  const config = action.config as any;

  // Action-specific details
  switch (action.action_type) {
    case 'GO_TO_STATE':
      // Show target state(s)
      if (action.target_states && action.target_states.length > 0) {
        details.push(`→ ${action.target_states.join(', ')}`);
      } else if (config?.stateNames && config.stateNames.length > 0) {
        details.push(`→ ${config.stateNames.join(', ')}`);
      }
      break;

    case 'TYPE':
      // Show typed text
      if (action.typed_text) {
        details.push(`"${action.typed_text}"`);
      } else if (action.text) {
        details.push(`"${action.text}"`);
      } else if (config?.text) {
        details.push(`"${config.text}"`);
      }
      break;

    case 'RUN_WORKFLOW':
      // Show workflow name (user noted it's redundant since next line shows workflow)
      // But useful if workflow hasn't expanded yet
      if (action.workflow_name) {
        details.push(action.workflow_name);
      }
      break;

    case 'MOUSE_MOVE':
      // Show move location
      if (action.x !== undefined && action.y !== undefined) {
        details.push(`→ (${action.x}, ${action.y})`);
      } else if (config?.x !== undefined && config?.y !== undefined) {
        details.push(`→ (${config.x}, ${config.y})`);
      }
      break;

    case 'CLICK':
      // Show click location
      if (action.x !== undefined && action.y !== undefined) {
        details.push(`at (${action.x}, ${action.y})`);
      } else if (config?.x !== undefined && config?.y !== undefined) {
        details.push(`at (${config.x}, ${config.y})`);
      }
      // Show image target if available
      if (action.image_id) {
        details.push(`[${action.image_id}]`);
      } else if (config?.target?.imageId) {
        details.push(`[${config.target.imageId}]`);
      }
      break;

    case 'FIND':
      // Show image being searched
      if (action.image_id) {
        details.push(`"${action.image_id}"`);
      } else if (config?.imageId) {
        details.push(`"${config.imageId}"`);
      }
      // Show if found/not found with match count
      if (action.found !== undefined) {
        if (action.found) {
          const matchCount = action.matches || 1;
          details.push(`✓ found (${matchCount} match${matchCount > 1 ? 'es' : ''})`);
          // Show location if available
          if (action.x !== undefined && action.y !== undefined) {
            details.push(`at (${action.x}, ${action.y})`);
          }
        } else {
          details.push('✗ not found');
        }
      }
      break;

    case 'WAIT':
      // Show wait duration
      if (action.duration !== undefined) {
        details.push(`${action.duration}ms`);
      } else if (config?.duration !== undefined) {
        details.push(`${config.duration}ms`);
      }
      break;

    case 'IF':
      // Show condition being checked (usually image-based)
      if (action.image_id) {
        details.push(`check "${action.image_id}"`);
      } else if (config?.imageId) {
        details.push(`check "${config.imageId}"`);
      } else if (config?.image) {
        details.push(`check "${config.image}"`);
      }
      // Show result if available
      if (action.condition_result !== undefined) {
        details.push(action.condition_result ? '✓ true' : '✗ false');
      }
      break;

    default:
      // Generic handling for other action types
      if (action.typed_text) {
        details.push(`"${action.typed_text}"`);
      }
      if (config?.target) {
        details.push(`target: ${config.target}`);
      }
      if (config?.url) {
        details.push(`url: ${config.url}`);
      }
  }

  // Add attempts if more than 1
  if (action.attempts > 1) {
    details.push(`(${action.attempts} attempts)`);
  }

  return details.length > 0 ? ` - ${details.join(', ')}` : '';
}

/**
 * Helper function to get the success status of an event
 */
function getEventSuccess(event: HierarchicalEvent): boolean | null {
  if (isActionEvent(event)) {
    return event.success;
  }
  if ('success' in event) {
    return event.success;
  }
  return null; // WorkflowStartedEvent doesn't have success
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
  const event = node.data;
  const nestingLevel = level !== undefined ? level : event.hierarchy.nesting_level;
  const indentation = nestingLevel * 20;
  const isExpandable = event.hierarchy.is_expandable || node.children.length > 0;
  const success = getEventSuccess(event);

  // Determine if this is a workflow event
  const isWorkflow = isWorkflowEvent(event);

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
        aria-label={isExpanded ? 'Collapse' : 'Expand'}
      >
        {isExpanded ? (
          <ChevronDown className="w-4 h-4" />
        ) : (
          <ChevronRight className="w-4 h-4" />
        )}
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
    return <Activity className="w-4 h-4 text-secondary" />;
  };

  /**
   * Renders the success/failure indicator
   */
  const renderStatusIndicator = () => {
    if (success === null) {
      return null; // No status for workflow started events
    }

    if (success) {
      return <CheckCircle2 className="w-4 h-4 text-accent" aria-label="Success" />;
    }
    return <XCircle className="w-4 h-4 text-destructive" aria-label="Failure" />;
  };

  /**
   * Renders the event content based on event type
   */
  const renderEventContent = () => {
    if (isWorkflow) {
      const workflow = event as WorkflowEvent;
      return (
        <span className="font-semibold text-primary">
          {workflow.workflow_name}
        </span>
      );
    }

    const action = event as ActionExecutionEvent;
    const details = formatActionDetails(action);

    return (
      <span className={success ? 'text-foreground' : 'text-destructive'}>
        <span className="font-medium">{action.action_type}</span>
        {details && <span className="text-sm text-muted-foreground">{details}</span>}
      </span>
    );
  };

  /**
   * Renders error/reason information if available
   */
  const renderAdditionalInfo = () => {
    if (isActionEvent(event)) {
      const action = event as ActionExecutionEvent;

      if (action.error) {
        return (
          <div
            className="text-xs text-destructive mt-1"
            style={{ paddingLeft: `${indentation + 28}px` }}
          >
            Error: {action.error}
          </div>
        );
      }

      if (action.reason && !action.success) {
        return (
          <div
            className="text-xs text-muted-foreground mt-1"
            style={{ paddingLeft: `${indentation + 28}px` }}
          >
            {action.reason}
          </div>
        );
      }
    }

    return null;
  };

  return (
    <div className="tree-node">
      {/* Main node row */}
      <div
        className="flex items-center gap-2 py-1.5 px-2 hover:bg-card/50 rounded transition-colors cursor-pointer group"
        style={{ paddingLeft: `${indentation + 8}px` }}
        onClick={() => isExpandable && onToggle(node.id)}
      >
        {renderToggleButton()}
        {renderIcon()}
        <div className="flex-1 flex items-center gap-2 min-w-0">
          {renderEventContent()}
        </div>
        {renderStatusIndicator()}
      </div>

      {/* Additional info row (error/reason) */}
      {renderAdditionalInfo()}

      {/* Recursively render children if expanded */}
      {isExpanded && node.children.length > 0 && (
        <div className="tree-children">
          {node.children.map((childNode) => (
            <TreeNode
              key={childNode.id}
              node={childNode}
              isExpanded={expandedNodes.has(childNode.id)}
              onToggle={onToggle}
              expandedNodes={expandedNodes}
            />
          ))}
        </div>
      )}
    </div>
  );
};
