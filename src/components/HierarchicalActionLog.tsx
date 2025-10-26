import { useEffect, useRef, useState, useMemo } from 'react';
import { TreeNode as TreeNodeComponent } from './TreeNode';
import { buildEventTree } from '../utils/eventTree';
import { HierarchicalEvent } from '../types/events';

interface HierarchicalActionLogProps {
  events: HierarchicalEvent[];
  autoScroll: boolean;
  onClear: () => void;
}

export default function HierarchicalActionLog({
  events,
  autoScroll,
}: HierarchicalActionLogProps) {
  const [expandedNodes, setExpandedNodes] = useState<Set<string>>(new Set());
  const scrollContainerRef = useRef<HTMLDivElement>(null);

  // Build tree structure from flat events array
  const treeNodes = useMemo(() => {
    console.log("=== HierarchicalActionLog: Building tree ===");
    console.log("Total events:", events.length);
    console.log("Sample event:", events[0]);
    if (events.length > 0) {
      console.log("First event parent_id:", events[0]?.hierarchy?.parent_id);
      console.log("First event nesting_level:", events[0]?.hierarchy?.nesting_level);
      console.log("All parent_ids:", events.map(e => e.hierarchy?.parent_id));
      console.log("All nesting_levels:", events.map(e => e.hierarchy?.nesting_level));
      console.log("All is_workflow flags:", events.map(e => e.is_workflow));
      console.log("Full events:", JSON.stringify(events, null, 2));
    }
    const tree = buildEventTree(events);
    console.log("Built tree with", tree.length, "root nodes");
    if (tree.length > 0) {
      console.log("First root node data:", tree[0].data);
      console.log("First root has", tree[0].children.length, "children");
      console.log("First root children:", tree[0].children.map(c => ({
        id: c.data.id,
        type: c.data.is_workflow ? 'workflow' : 'action',
        nesting_level: c.data.hierarchy?.nesting_level
      })));
    }

    // If there's only one root node and it's the top-level workflow (nesting_level === 1),
    // skip showing it and display its children directly
    // This avoids redundant nesting since only one workflow runs at a time
    if (tree.length === 1 &&
        tree[0] &&
        tree[0].data &&
        tree[0].data.hierarchy &&
        tree[0].data.hierarchy.nesting_level === 1 &&
        tree[0].data.is_workflow &&
        tree[0].children &&
        tree[0].children.length > 0) {
      console.log("Skipping top-level workflow node, showing children directly");
      console.log("Returning", tree[0].children.length, "children");
      return tree[0].children;
    }

    console.log("Returning full tree with", tree.length, "nodes");
    console.log("Final treeNodes that will be rendered:", tree.map(n => ({
      id: n.data.id,
      type: n.data.is_workflow ? 'workflow' : 'action',
      nesting_level: n.data.hierarchy?.nesting_level,
      children: n.children.length
    })));
    return tree;
  }, [events]);

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
  }, [events, autoScroll]);

  return (
    <div className="space-y-2">
      {treeNodes.length === 0 ? (
        <div className="text-center text-muted-foreground py-8">
          <p>No events to display</p>
        </div>
      ) : (
        <div className="space-y-1">
          {treeNodes.map((node) => (
            <TreeNodeComponent
              key={node.data.id}
              node={node}
              isExpanded={expandedNodes.has(node.data.id)}
              onToggle={handleToggleNode}
              expandedNodes={expandedNodes}
            />
          ))}
        </div>
      )}
    </div>
  );
}
