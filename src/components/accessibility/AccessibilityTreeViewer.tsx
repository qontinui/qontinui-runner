/**
 * AccessibilityTreeViewer.tsx
 *
 * Displays the accessibility tree in a hierarchical, collapsible format.
 * Shows node properties: role, name, value, ref (@e1, @e2), and bounds.
 * Highlights interactive elements and allows clicking on nodes to see details.
 */

import { useState, useCallback, useMemo } from "react";
import {
  ChevronRight,
  ChevronDown,
  MousePointer,
  Type,
  Focus,
  Copy,
  Check,
  Search,
} from "lucide-react";
import type { AccessibilityNode, AccessibilityRole } from "../../types/accessibility";
import { getAccentColors, getStatusColors } from "@/design-system";

/**
 * Get color for accessibility role.
 */
function getRoleColor(role: AccessibilityRole): string {
  const interactiveRoles: AccessibilityRole[] = [
    "button",
    "link",
    "checkbox",
    "radio",
    "textbox",
    "combobox",
    "searchbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "tab",
    "switch",
    "slider",
    "spinbutton",
  ];

  const containerRoles: AccessibilityRole[] = [
    "dialog",
    "form",
    "group",
    "list",
    "listbox",
    "menu",
    "menubar",
    "navigation",
    "region",
    "table",
    "tablist",
    "toolbar",
    "tree",
  ];

  if (interactiveRoles.includes(role)) {
    return getAccentColors("blue").text;
  }
  if (containerRoles.includes(role)) {
    return getAccentColors("purple").text;
  }
  return "text-muted-foreground";
}

/**
 * Get icon for accessibility role.
 */
function getRoleIcon(role: AccessibilityRole): string {
  const iconMap: Partial<Record<AccessibilityRole, string>> = {
    button: "btn",
    link: "a",
    textbox: "input",
    checkbox: "cb",
    radio: "rad",
    combobox: "sel",
    searchbox: "search",
    img: "img",
    heading: "h",
    list: "ul",
    listitem: "li",
    table: "tbl",
    row: "tr",
    navigation: "nav",
    dialog: "dlg",
    form: "form",
    menu: "menu",
    tab: "tab",
    tree: "tree",
    treeitem: "item",
  };
  return iconMap[role] || role.slice(0, 3);
}

interface TreeNodeItemProps {
  node: AccessibilityNode;
  depth: number;
  expandedNodes: Set<string>;
  selectedRef: string | null;
  searchQuery: string;
  onToggle: (ref: string) => void;
  onSelect: (node: AccessibilityNode) => void;
  onCopyRef: (ref: string) => void;
  onClickAction?: (ref: string) => void;
  onFillAction?: (ref: string) => void;
  onFocusAction?: (ref: string) => void;
}

/**
 * Single tree node item.
 */
function TreeNodeItem({
  node,
  depth,
  expandedNodes,
  selectedRef,
  searchQuery,
  onToggle,
  onSelect,
  onCopyRef,
  onClickAction,
  onFillAction,
  onFocusAction,
}: TreeNodeItemProps) {
  const [copySuccess, setCopySuccess] = useState(false);
  const hasChildren = node.children && node.children.length > 0;
  const isExpanded = expandedNodes.has(node.ref);
  const isSelected = selectedRef === node.ref;
  const isInteractive = node.is_interactive;

  // Check if this node matches the search query
  const matchesSearch = useMemo(() => {
    if (!searchQuery) return true;
    const query = searchQuery.toLowerCase();
    return (
      node.ref.toLowerCase().includes(query) ||
      node.role.toLowerCase().includes(query) ||
      (node.name && node.name.toLowerCase().includes(query)) ||
      (node.value && node.value.toLowerCase().includes(query))
    );
  }, [node, searchQuery]);

  // Check if any descendant matches search
  const hasMatchingDescendant = useMemo(() => {
    if (!searchQuery) return false;
    const checkDescendants = (n: AccessibilityNode): boolean => {
      const query = searchQuery.toLowerCase();
      if (
        n.ref.toLowerCase().includes(query) ||
        n.role.toLowerCase().includes(query) ||
        (n.name && n.name.toLowerCase().includes(query)) ||
        (n.value && n.value.toLowerCase().includes(query))
      ) {
        return true;
      }
      return n.children?.some(checkDescendants) ?? false;
    };
    return node.children?.some(checkDescendants) ?? false;
  }, [node, searchQuery]);

  // Hide if doesn't match and no descendants match
  if (searchQuery && !matchesSearch && !hasMatchingDescendant) {
    return null;
  }

  const handleCopyRef = (e: React.MouseEvent) => {
    e.stopPropagation();
    onCopyRef(node.ref);
    setCopySuccess(true);
    setTimeout(() => setCopySuccess(false), 1500);
  };

  const roleColor = getRoleColor(node.role);
  const roleIcon = getRoleIcon(node.role);

  return (
    <div className="select-none">
      {/* Node row */}
      <div
        className={`flex items-center gap-1 py-1 px-2 rounded cursor-pointer transition-colors group
          ${isSelected ? "bg-primary/20 ring-1 ring-primary/30" : "hover:bg-muted/50"}
          ${matchesSearch && searchQuery ? "bg-yellow-500/10" : ""}
        `}
        style={{ paddingLeft: `${depth * 16 + 8}px` }}
        onClick={() => onSelect(node)}
      >
        {/* Expand/collapse toggle */}
        <button
          className="w-4 h-4 flex items-center justify-center text-muted-foreground hover:text-foreground"
          onClick={(e) => {
            e.stopPropagation();
            if (hasChildren) onToggle(node.ref);
          }}
        >
          {hasChildren ? (
            isExpanded ? (
              <ChevronDown className="w-3 h-3" />
            ) : (
              <ChevronRight className="w-3 h-3" />
            )
          ) : (
            <span className="w-3 h-3" />
          )}
        </button>

        {/* Role badge */}
        <span
          className={`px-1.5 py-0.5 text-[10px] font-mono rounded ${roleColor} ${
            isInteractive ? "bg-primary/10 font-semibold" : "bg-muted/50"
          }`}
          title={node.role}
        >
          {roleIcon}
        </span>

        {/* Ref */}
        <span
          className={`text-xs font-mono px-1 rounded ${
            isInteractive ? getAccentColors("green").text : "text-muted-foreground"
          }`}
        >
          {node.ref}
        </span>

        {/* Name/Value */}
        <span className="flex-1 text-xs truncate text-foreground">
          {node.name && (
            <span className="font-medium" title={node.name}>
              {node.name.length > 40 ? node.name.slice(0, 40) + "..." : node.name}
            </span>
          )}
          {node.value && (
            <span className="ml-1 text-muted-foreground" title={node.value}>
              = "{node.value.length > 20 ? node.value.slice(0, 20) + "..." : node.value}"
            </span>
          )}
        </span>

        {/* Interactive indicator */}
        {isInteractive && (
          <span
            className={`w-1.5 h-1.5 rounded-full ${getAccentColors("green").bgSolid}`}
            title="Interactive"
          />
        )}

        {/* Action buttons (visible on hover or when selected) */}
        <div
          className={`flex items-center gap-0.5 ${
            isSelected ? "opacity-100" : "opacity-0 group-hover:opacity-100"
          } transition-opacity`}
        >
          {/* Copy ref button */}
          <button
            onClick={handleCopyRef}
            className="p-1 rounded hover:bg-muted/80 transition-colors"
            title="Copy ref"
          >
            {copySuccess ? (
              <Check className={`w-3 h-3 ${getStatusColors("success").icon}`} />
            ) : (
              <Copy className="w-3 h-3 text-muted-foreground" />
            )}
          </button>

          {/* Click action */}
          {isInteractive && onClickAction && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                onClickAction(node.ref);
              }}
              className="p-1 rounded hover:bg-muted/80 transition-colors"
              title="Click element"
            >
              <MousePointer className="w-3 h-3 text-muted-foreground" />
            </button>
          )}

          {/* Fill action (for text inputs) */}
          {isInteractive &&
            ["textbox", "searchbox", "combobox"].includes(node.role) &&
            onFillAction && (
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  onFillAction(node.ref);
                }}
                className="p-1 rounded hover:bg-muted/80 transition-colors"
                title="Fill element"
              >
                <Type className="w-3 h-3 text-muted-foreground" />
              </button>
            )}

          {/* Focus action */}
          {isInteractive && onFocusAction && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                onFocusAction(node.ref);
              }}
              className="p-1 rounded hover:bg-muted/80 transition-colors"
              title="Focus element"
            >
              <Focus className="w-3 h-3 text-muted-foreground" />
            </button>
          )}
        </div>
      </div>

      {/* Children */}
      {hasChildren && isExpanded && (
        <div>
          {node.children.map((child) => (
            <TreeNodeItem
              key={child.ref}
              node={child}
              depth={depth + 1}
              expandedNodes={expandedNodes}
              selectedRef={selectedRef}
              searchQuery={searchQuery}
              onToggle={onToggle}
              onSelect={onSelect}
              onCopyRef={onCopyRef}
              onClickAction={onClickAction}
              onFillAction={onFillAction}
              onFocusAction={onFocusAction}
            />
          ))}
        </div>
      )}
    </div>
  );
}

interface AccessibilityTreeViewerProps {
  /** Root node of the accessibility tree */
  root: AccessibilityNode;
  /** Currently selected node ref */
  selectedRef?: string | null;
  /** Callback when a node is selected */
  onSelectNode?: (node: AccessibilityNode) => void;
  /** Callback when a ref is copied */
  onCopyRef?: (ref: string) => void;
  /** Callback for click action */
  onClickAction?: (ref: string) => void;
  /** Callback for fill action */
  onFillAction?: (ref: string) => void;
  /** Callback for focus action */
  onFocusAction?: (ref: string) => void;
  /** Whether to auto-expand interactive elements */
  autoExpandInteractive?: boolean;
  /** Maximum initial depth to expand */
  initialExpandDepth?: number;
}

/**
 * Accessibility tree viewer component.
 */
export function AccessibilityTreeViewer({
  root,
  selectedRef = null,
  onSelectNode,
  onCopyRef,
  onClickAction,
  onFillAction,
  onFocusAction,
  autoExpandInteractive = true,
  initialExpandDepth = 2,
}: AccessibilityTreeViewerProps) {
  const [searchQuery, setSearchQuery] = useState("");

  // Calculate initially expanded nodes
  const initialExpanded = useMemo(() => {
    const expanded = new Set<string>();

    const expandNode = (node: AccessibilityNode, depth: number) => {
      if (depth <= initialExpandDepth) {
        expanded.add(node.ref);
      }
      // Always expand path to interactive elements if enabled
      if (autoExpandInteractive && node.is_interactive) {
        let current = node.parent_ref;
        while (current) {
          expanded.add(current);
          // Find parent node to continue up the tree
          const findParent = (n: AccessibilityNode): AccessibilityNode | null => {
            if (n.ref === current) return n;
            for (const child of n.children || []) {
              const found = findParent(child);
              if (found) return found;
            }
            return null;
          };
          const parent = findParent(root);
          current = parent?.parent_ref;
        }
      }
      node.children?.forEach((child) => expandNode(child, depth + 1));
    };

    expandNode(root, 0);
    return expanded;
  }, [root, initialExpandDepth, autoExpandInteractive]);

  const [expandedNodes, setExpandedNodes] = useState<Set<string>>(initialExpanded);

  const handleToggle = useCallback((ref: string) => {
    setExpandedNodes((prev) => {
      const next = new Set(prev);
      if (next.has(ref)) {
        next.delete(ref);
      } else {
        next.add(ref);
      }
      return next;
    });
  }, []);

  const handleSelect = useCallback(
    (node: AccessibilityNode) => {
      onSelectNode?.(node);
    },
    [onSelectNode],
  );

  const handleCopyRef = useCallback(
    (ref: string) => {
      navigator.clipboard.writeText(ref);
      onCopyRef?.(ref);
    },
    [onCopyRef],
  );

  // Expand all / collapse all
  const expandAll = useCallback(() => {
    const allRefs = new Set<string>();
    const collectRefs = (node: AccessibilityNode) => {
      allRefs.add(node.ref);
      node.children?.forEach(collectRefs);
    };
    collectRefs(root);
    setExpandedNodes(allRefs);
  }, [root]);

  const collapseAll = useCallback(() => {
    setExpandedNodes(new Set([root.ref]));
  }, [root.ref]);

  // Count nodes
  const nodeCount = useMemo(() => {
    let total = 0;
    let interactive = 0;
    const count = (node: AccessibilityNode) => {
      total++;
      if (node.is_interactive) interactive++;
      node.children?.forEach(count);
    };
    count(root);
    return { total, interactive };
  }, [root]);

  return (
    <div data-ui-id="a11y-tree-viewer" className="flex flex-col h-full">
      {/* Toolbar */}
      <div className="flex items-center gap-2 px-3 py-2 border-b border-border flex-shrink-0">
        {/* Search */}
        <div className="relative flex-1">
          <Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
          <input
            data-ui-id="a11y-tree-search-input"
            type="text"
            placeholder="Search nodes..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-7 pr-3 py-1.5 text-xs bg-muted/50 border border-border/50 rounded-md focus:outline-none focus:ring-1 focus:ring-primary/50"
          />
        </div>

        {/* Stats */}
        <span className="text-xs text-muted-foreground whitespace-nowrap">
          {nodeCount.total} nodes ({nodeCount.interactive} interactive)
        </span>

        {/* Expand/Collapse buttons */}
        <button
          data-ui-id="a11y-expand-all-btn"
          onClick={expandAll}
          className="px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors"
        >
          Expand All
        </button>
        <button
          data-ui-id="a11y-collapse-all-btn"
          onClick={collapseAll}
          className="px-2 py-1 text-xs text-muted-foreground hover:text-foreground hover:bg-muted/50 rounded transition-colors"
        >
          Collapse
        </button>
      </div>

      {/* Tree */}
      <div data-ui-id="a11y-tree-list" className="flex-1 overflow-auto">
        <TreeNodeItem
          node={root}
          depth={0}
          expandedNodes={expandedNodes}
          selectedRef={selectedRef}
          searchQuery={searchQuery}
          onToggle={handleToggle}
          onSelect={handleSelect}
          onCopyRef={handleCopyRef}
          onClickAction={onClickAction}
          onFillAction={onFillAction}
          onFocusAction={onFocusAction}
        />
      </div>
    </div>
  );
}

export default AccessibilityTreeViewer;
