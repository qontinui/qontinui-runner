/**
 * Example: Integrating ActionDetailModal with TreeNode
 *
 * This example shows how to add click-to-view-details functionality
 * to the hierarchical action log.
 */

import { useState } from "react";
import { DisplayNode } from "../types/treeEvents";
import ActionDetailModal from "./ActionDetailModal";

/**
 * Example 1: Simple usage in a component
 */
export function SimpleExample() {
  const [selectedAction, setSelectedAction] = useState<DisplayNode | null>(null);
  const [isModalOpen, setIsModalOpen] = useState(false);

  // Example action data
  const exampleAction: DisplayNode = {
    id: "action-123",
    type: "action",
    name: "FIND",
    timestamp: Date.now() / 1000,
    end_timestamp: Date.now() / 1000 + 2,
    duration: 2,
    status: "success",
    metadata: {
      config: {
        target: {
          type: "image",
          imageId: "login_button",
        },
        timeout: 5000,
      },
      runtime: {
        found: true,
        confidence: 0.987,
        location: { x: 100, y: 200 },
        dimensions: { w: 120, h: 40 },
        top_matches: [
          {
            confidence: 0.987,
            location: { x: 100, y: 200 },
            dimensions: { w: 120, h: 40 },
          },
          {
            confidence: 0.952,
            location: { x: 105, y: 205 },
            dimensions: { w: 120, h: 40 },
          },
        ],
      },
      state_context: {
        active_before: ["login_screen"],
        active_after: ["login_screen"],
        changed: false,
        activated: [],
        deactivated: [],
      },
      timing: {
        start_time: new Date().toISOString(),
        end_time: new Date().toISOString(),
        duration_ms: 2000,
      },
    },
    children: [],
    parent: null,
    isExpanded: true,
  };

  const handleActionClick = () => {
    setSelectedAction(exampleAction);
    setIsModalOpen(true);
  };

  const handleClose = () => {
    setIsModalOpen(false);
    setTimeout(() => setSelectedAction(null), 200);
  };

  return (
    <div>
      <button
        onClick={handleActionClick}
        className="px-4 py-2 bg-primary text-primary-foreground rounded-md"
      >
        View Action Details
      </button>

      <ActionDetailModal
        action={selectedAction}
        isOpen={isModalOpen}
        onClose={handleClose}
      />
    </div>
  );
}

/**
 * Example 2: Integration with TreeNode
 *
 * To integrate with the existing TreeNode component:
 *
 * 1. Add onNodeClick prop to TreeNodeProps:
 *
 * interface TreeNodeProps {
 *   node: DisplayNode;
 *   isExpanded: boolean;
 *   onToggle: (id: string) => void;
 *   expandedNodes: Set<string>;
 *   level?: number;
 *   onNodeClick?: (node: DisplayNode) => void; // Add this
 * }
 */

/**
 * 2. Modify the TreeNode component's click handler:
 */

function TreeNodeExample({
  node,
  isExpanded,
  onToggle,
  onNodeClick,
}: {
  node: DisplayNode;
  isExpanded: boolean;
  onToggle: (id: string) => void;
  onNodeClick?: (node: DisplayNode) => void;
}) {
  const isExpandable = node.metadata.is_expandable || node.children.length > 0;

  const handleRowClick = (e: React.MouseEvent) => {
    // If clicking on the row (not toggle button), show details
    if (e.currentTarget === e.target) {
      onNodeClick?.(node);
    }
  };

  return (
    <div
      className="flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-accent/5 transition-colors cursor-pointer"
      onClick={handleRowClick}
    >
      {/* Toggle button */}
      {isExpandable && (
        <button
          onClick={(e) => {
            e.stopPropagation(); // Prevent showing details when toggling
            onToggle(node.id);
          }}
        >
          {isExpanded ? "▼" : "▶"}
        </button>
      )}

      {/* Action name */}
      <span>{node.name}</span>
    </div>
  );
}

/**
 * 3. Use in HierarchicalActionLog:
 */

export function HierarchicalActionLogExample({
  treeRoots,
}: {
  treeRoots: DisplayNode[];
}) {
  const [selectedAction, setSelectedAction] = useState<DisplayNode | null>(null);
  const [isDetailModalOpen, setIsDetailModalOpen] = useState(false);
  const [expandedNodes] = useState<Set<string>>(new Set());

  const handleNodeClick = (node: DisplayNode) => {
    // Only show details for action nodes, not workflow nodes
    if (node.type === "action") {
      setSelectedAction(node);
      setIsDetailModalOpen(true);
    }
  };

  const handleCloseModal = () => {
    setIsDetailModalOpen(false);
    setTimeout(() => setSelectedAction(null), 200);
  };

  const handleToggle = (nodeId: string) => {
    // Toggle logic here
    console.log("Toggle node:", nodeId);
  };

  return (
    <>
      <div className="space-y-2">
        {treeRoots.map((node) => (
          <TreeNodeExample
            key={node.id}
            node={node}
            isExpanded={expandedNodes.has(node.id)}
            onToggle={handleToggle}
            onNodeClick={handleNodeClick} // Pass click handler
          />
        ))}
      </div>

      {/* Detail Modal */}
      <ActionDetailModal
        action={selectedAction}
        isOpen={isDetailModalOpen}
        onClose={handleCloseModal}
      />
    </>
  );
}

/**
 * Example 3: With keyboard navigation
 */

export function KeyboardNavigationExample({ actions }: { actions: DisplayNode[] }) {
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [isModalOpen, setIsModalOpen] = useState(false);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        setSelectedIndex((prev) => Math.min(prev + 1, actions.length - 1));
        break;
      case "ArrowUp":
        e.preventDefault();
        setSelectedIndex((prev) => Math.max(prev - 1, 0));
        break;
      case "Enter":
      case " ":
        e.preventDefault();
        setIsModalOpen(true);
        break;
      case "Escape":
        setIsModalOpen(false);
        break;
    }
  };

  return (
    <div onKeyDown={handleKeyDown} tabIndex={0} className="outline-none">
      {actions.map((action, index) => (
        <div
          key={action.id}
          className={`p-2 cursor-pointer ${index === selectedIndex ? "bg-accent" : ""}`}
          onClick={() => {
            setSelectedIndex(index);
            setIsModalOpen(true);
          }}
        >
          {action.name}
        </div>
      ))}

      <ActionDetailModal
        action={actions[selectedIndex] || null}
        isOpen={isModalOpen}
        onClose={() => setIsModalOpen(false)}
      />
    </div>
  );
}

/**
 * Example 4: With context menu (right-click)
 */

export function ContextMenuExample({ node }: { node: DisplayNode }) {
  const [isModalOpen, setIsModalOpen] = useState(false);

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    setIsModalOpen(true);
  };

  return (
    <>
      <div
        onContextMenu={handleContextMenu}
        className="p-2 hover:bg-accent cursor-pointer"
      >
        {node.name}
      </div>

      <ActionDetailModal
        action={node}
        isOpen={isModalOpen}
        onClose={() => setIsModalOpen(false)}
      />
    </>
  );
}
