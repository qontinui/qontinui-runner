/**
 * Element Tree View
 *
 * Displays a hierarchical tree of UI Bridge registered elements.
 * Supports selection, filtering, and state indication.
 */

import { useState, useMemo, useCallback } from "react";
import { cn } from "../../lib/utils";
import { Badge } from "../ui/Badge";
import {
  ChevronRight,
  ChevronDown,
  Box,
  Type,
  ToggleLeft,
  Link,
  Square,
  FormInput,
  MousePointer,
  List,
  Circle,
  EyeOff,
  Lock,
  Search,
} from "lucide-react";
import type { UIBridgeElement, UIBridgeState } from "./UIBridgeInspectorPanel";

interface ElementTreeViewProps {
  elements: UIBridgeElement[];
  states: UIBridgeState[];
  activeStates: string[];
  selectedElementId?: string | null;
  onSelectElement?: (elementId: string | null) => void;
  loading?: boolean;
}

interface TreeNode {
  element: UIBridgeElement;
  children: TreeNode[];
  depth: number;
}

const ELEMENT_TYPE_ICONS: Record<string, React.ReactNode> = {
  button: <MousePointer className="w-3.5 h-3.5" />,
  input: <Type className="w-3.5 h-3.5" />,
  textarea: <FormInput className="w-3.5 h-3.5" />,
  select: <List className="w-3.5 h-3.5" />,
  checkbox: <ToggleLeft className="w-3.5 h-3.5" />,
  radio: <Circle className="w-3.5 h-3.5" />,
  link: <Link className="w-3.5 h-3.5" />,
  form: <Square className="w-3.5 h-3.5" />,
  custom: <Box className="w-3.5 h-3.5" />,
};

export function ElementTreeView({
  elements,
  states,
  activeStates,
  selectedElementId,
  onSelectElement,
  loading = false,
}: ElementTreeViewProps) {
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState("");

  // Build tree structure from flat elements
  const tree = useMemo(() => {
    const nodeMap = new Map<string, TreeNode>();
    const roots: TreeNode[] = [];

    // First pass: create nodes
    elements.forEach((element) => {
      nodeMap.set(element.id, {
        element,
        children: [],
        depth: 0,
      });
    });

    // Second pass: build hierarchy
    elements.forEach((element) => {
      const node = nodeMap.get(element.id)!;
      if (element.parent && nodeMap.has(element.parent)) {
        const parentNode = nodeMap.get(element.parent)!;
        parentNode.children.push(node);
        node.depth = parentNode.depth + 1;
      } else {
        roots.push(node);
      }
    });

    // Sort children by ID
    const sortChildren = (node: TreeNode) => {
      node.children.sort((a, b) => a.element.id.localeCompare(b.element.id));
      node.children.forEach(sortChildren);
    };
    roots.forEach(sortChildren);
    roots.sort((a, b) => a.element.id.localeCompare(b.element.id));

    return roots;
  }, [elements]);

  // Filter tree
  const filteredTree = useMemo(() => {
    if (!filter.trim()) return tree;

    const lowerFilter = filter.toLowerCase();

    const matchesFilter = (node: TreeNode): boolean => {
      const el = node.element;
      return (
        el.id.toLowerCase().includes(lowerFilter) ||
        el.type.toLowerCase().includes(lowerFilter) ||
        el.tagName.toLowerCase().includes(lowerFilter) ||
        el.label?.toLowerCase().includes(lowerFilter) ||
        el.text?.toLowerCase().includes(lowerFilter) ||
        false
      );
    };

    const filterNode = (node: TreeNode): TreeNode | null => {
      const filteredChildren = node.children
        .map(filterNode)
        .filter((n): n is TreeNode => n !== null);

      if (matchesFilter(node) || filteredChildren.length > 0) {
        return {
          ...node,
          children: filteredChildren,
        };
      }
      return null;
    };

    return tree.map(filterNode).filter((n): n is TreeNode => n !== null);
  }, [tree, filter]);

  // Get states for an element
  const getElementStates = useCallback(
    (elementId: string): UIBridgeState[] => {
      return states.filter((state) => state.elements.includes(elementId));
    },
    [states]
  );

  // Toggle expand/collapse
  const toggleExpand = useCallback((id: string) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  // Expand all nodes containing the filter match
  const expandAll = useCallback(() => {
    const allIds = new Set<string>();
    const collectIds = (node: TreeNode) => {
      if (node.children.length > 0) {
        allIds.add(node.element.id);
        node.children.forEach(collectIds);
      }
    };
    filteredTree.forEach(collectIds);
    setExpandedIds(allIds);
  }, [filteredTree]);

  // Collapse all
  const collapseAll = useCallback(() => {
    setExpandedIds(new Set());
  }, []);

  // Render a single tree node
  const renderNode = (node: TreeNode) => {
    const { element, children, depth } = node;
    const isExpanded = expandedIds.has(element.id);
    const isSelected = selectedElementId === element.id;
    const hasChildren = children.length > 0;
    const elementStates = getElementStates(element.id);
    const isInActiveState = elementStates.some((s) =>
      activeStates.includes(s.id)
    );

    return (
      <div key={element.id}>
        <div
          className={cn(
            "flex items-center gap-1 py-1 px-2 rounded-md cursor-pointer transition-colors",
            "hover:bg-muted/50",
            isSelected && "bg-primary/10 ring-1 ring-primary/30",
            !element.visible && "opacity-50"
          )}
          style={{ paddingLeft: `${depth * 16 + 8}px` }}
          onClick={() => onSelectElement?.(isSelected ? null : element.id)}
        >
          {/* Expand/Collapse toggle */}
          <button
            className={cn(
              "w-4 h-4 flex items-center justify-center rounded hover:bg-muted",
              !hasChildren && "invisible"
            )}
            onClick={(e) => {
              e.stopPropagation();
              toggleExpand(element.id);
            }}
          >
            {isExpanded ? (
              <ChevronDown className="w-3 h-3" />
            ) : (
              <ChevronRight className="w-3 h-3" />
            )}
          </button>

          {/* Element type icon */}
          <span className="text-muted-foreground">
            {ELEMENT_TYPE_ICONS[element.type] || ELEMENT_TYPE_ICONS.custom}
          </span>

          {/* Element ID */}
          <span
            className={cn(
              "flex-1 text-xs font-mono truncate",
              isInActiveState && "text-accent"
            )}
            title={element.id}
          >
            {element.id}
          </span>

          {/* State indicators */}
          {!element.visible && (
            <span className="inline-flex" title="Hidden">
              <EyeOff className="w-3 h-3 text-muted-foreground" />
            </span>
          )}
          {!element.enabled && (
            <span className="inline-flex" title="Disabled">
              <Lock className="w-3 h-3 text-muted-foreground" />
            </span>
          )}
          {element.focused && (
            <div
              className="w-2 h-2 rounded-full bg-accent"
              title="Focused"
            />
          )}

          {/* Type badge */}
          <Badge variant="muted" className="text-[10px] px-1 py-0">
            {element.type}
          </Badge>
        </div>

        {/* Children */}
        {hasChildren && isExpanded && (
          <div>{children.map(renderNode)}</div>
        )}
      </div>
    );
  };

  if (loading && elements.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
        Loading elements...
      </div>
    );
  }

  if (elements.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground text-sm gap-2">
        <Box className="w-8 h-8 opacity-50" />
        <p>No UI Bridge elements found</p>
        <p className="text-xs">
          Make sure the page uses UI Bridge with data-ui-id attributes
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Search and controls */}
      <div className="flex items-center gap-2 mb-2">
        <div className="relative flex-1">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
          <input
            type="text"
            placeholder="Filter elements..."
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="w-full pl-7 pr-2 py-1.5 text-xs bg-muted/30 border border-border/50 rounded-md focus:outline-none focus:ring-1 focus:ring-primary"
          />
        </div>
        <button
          className="px-2 py-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
          onClick={expandAll}
          title="Expand all"
        >
          Expand
        </button>
        <button
          className="px-2 py-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
          onClick={collapseAll}
          title="Collapse all"
        >
          Collapse
        </button>
      </div>

      {/* Tree */}
      <div className="flex-1 overflow-auto">
        {filteredTree.length === 0 ? (
          <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
            No elements match "{filter}"
          </div>
        ) : (
          filteredTree.map(renderNode)
        )}
      </div>

      {/* Selected element details */}
      {selectedElementId && (
        <SelectedElementDetails
          element={elements.find((el) => el.id === selectedElementId)}
          states={getElementStates(selectedElementId)}
          activeStates={activeStates}
        />
      )}
    </div>
  );
}

interface SelectedElementDetailsProps {
  element?: UIBridgeElement;
  states: UIBridgeState[];
  activeStates: string[];
}

function SelectedElementDetails({
  element,
  states,
  activeStates,
}: SelectedElementDetailsProps) {
  if (!element) return null;

  return (
    <div className="mt-2 p-2 bg-muted/30 rounded-md border border-border/50">
      <div className="text-xs font-semibold mb-1">Selected: {element.id}</div>
      <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
        <div className="text-muted-foreground">Type:</div>
        <div>{element.type}</div>
        <div className="text-muted-foreground">Tag:</div>
        <div>{element.tagName}</div>
        {element.label && (
          <>
            <div className="text-muted-foreground">Label:</div>
            <div className="truncate">{element.label}</div>
          </>
        )}
        {element.value !== undefined && (
          <>
            <div className="text-muted-foreground">Value:</div>
            <div className="truncate font-mono">{element.value || "(empty)"}</div>
          </>
        )}
        <div className="text-muted-foreground">Visible:</div>
        <div>{element.visible ? "Yes" : "No"}</div>
        <div className="text-muted-foreground">Enabled:</div>
        <div>{element.enabled ? "Yes" : "No"}</div>
        {states.length > 0 && (
          <>
            <div className="text-muted-foreground">States:</div>
            <div className="flex flex-wrap gap-1">
              {states.map((state) => (
                <Badge
                  key={state.id}
                  variant={activeStates.includes(state.id) ? "default" : "muted"}
                  className="text-[10px]"
                >
                  {state.name || state.id}
                </Badge>
              ))}
            </div>
          </>
        )}
        {element.actions && element.actions.length > 0 && (
          <>
            <div className="text-muted-foreground">Actions:</div>
            <div className="flex flex-wrap gap-1">
              {element.actions.map((action) => (
                <Badge key={action} variant="muted" className="text-[10px]">
                  {action}
                </Badge>
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export default ElementTreeView;
