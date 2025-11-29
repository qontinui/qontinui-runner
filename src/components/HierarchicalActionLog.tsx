import { useEffect, useRef, useState } from "react";
import { TreeNode as TreeNodeComponent } from "./TreeNode";
import { DisplayNode } from "../types/treeEvents";

interface HierarchicalActionLogProps {
  treeRoots: DisplayNode[];
  autoScroll: boolean;
  onClear: () => void;
}

/**
 * Helper function to render a node, skipping workflow wrappers
 */
function renderNodeTree(
  node: DisplayNode,
  expandedNodes: Set<string>,
  onToggle: (id: string) => void,
  skipRootWorkflows: boolean = false,
): React.ReactNode[] {
  // Skip workflow nodes that are just wrappers
  if (node.type === "workflow") {
    console.log(`[HierarchicalActionLog] Checking workflow node: ${node.name}`, {
      isRoot: node.parent === null,
      skipRootWorkflows,
      metadata: node.metadata,
      childCount: node.children.length,
    });

    // If this is a root workflow, skip it and render children
    // (user already knows which workflow they ran)
    if (skipRootWorkflows && node.parent === null) {
      console.log(
        `[HierarchicalActionLog] Skipping root workflow: ${node.name}, children count: ${node.children.length}`,
      );
      const result = node.children.flatMap((child) =>
        renderNodeTree(child, expandedNodes, onToggle, false),
      );
      console.log(`[HierarchicalActionLog] Returning ${result.length} child nodes instead of root`);
      return result;
    }

    // If this is an inline workflow (transition workflow), skip it
    // Inline workflows are marked with is_inline=true in metadata
    // RUN_WORKFLOW actions create workflows with is_expandable=true and should be visible
    // Only check for inline workflows if they have children (otherwise we can't skip and render children)
    if (node.children.length > 0) {
      const isInline = node.metadata.is_inline === true;
      const isUserWorkflow = node.metadata.is_expandable === true;

      console.log(
        `[HierarchicalActionLog] Workflow ${node.name}: isInline=${isInline}, isUserWorkflow=${isUserWorkflow}`,
      );

      if (isInline && !isUserWorkflow) {
        console.log(`[HierarchicalActionLog] Skipping inline workflow: ${node.name}`);
        return node.children.flatMap((child) =>
          renderNodeTree(child, expandedNodes, onToggle, false),
        );
      }
    }
  }

  // Render this node normally
  return [
    <TreeNodeComponent
      key={node.id}
      node={node}
      isExpanded={expandedNodes.has(node.id)}
      onToggle={onToggle}
      expandedNodes={expandedNodes}
    />,
  ];
}

export default function HierarchicalActionLog({
  treeRoots,
  autoScroll,
}: HierarchicalActionLogProps) {
  const [expandedNodes, setExpandedNodes] = useState<Set<string>>(new Set());
  const scrollContainerRef = useRef<HTMLDivElement>(null);

  // Handle node expand/collapse toggle
  const handleToggleNode = (nodeId: string) => {
    setExpandedNodes((prev) => {
      const newSet = new Set(prev);
      if (newSet.has(nodeId)) {
        newSet.delete(nodeId);
      } else {
        newSet.add(nodeId);
      }
      return newSet;
    });
  };

  // Auto-scroll to bottom when new events arrive
  useEffect(() => {
    if (autoScroll && scrollContainerRef.current) {
      scrollContainerRef.current.scrollTop = scrollContainerRef.current.scrollHeight;
    }
  }, [treeRoots, autoScroll]);

  return (
    <div className="space-y-2">
      {treeRoots.length === 0 ? (
        <div className="text-center text-muted-foreground py-8">
          <p>No events to display</p>
        </div>
      ) : (
        <div className="space-y-1">
          {treeRoots.flatMap((node) => renderNodeTree(node, expandedNodes, handleToggleNode, true))}
        </div>
      )}
    </div>
  );
}
