/**
 * TreeNodeView.tsx
 *
 * Recursive tree node renderer for the accessibility tree.
 * Displays role badges, names, ref IDs, and expand/collapse controls.
 */

import { useState, useCallback, useMemo, memo } from "react";
import { ChevronRight, ChevronDown } from "lucide-react";
import { cn } from "../../lib/utils";
import { Badge } from "../ui";
import type { UnifiedNode } from "./types";

/** Color mapping for common accessibility roles. */
function getRoleBadgeVariant(role: string): "info" | "success" | "muted" | "default" | "purple" {
  const lower = role.toLowerCase();
  if (lower.includes("button") || lower === "link" || lower === "menuitem" || lower === "tab") {
    return "info";
  }
  if (
    lower.includes("edit") ||
    (lower.includes("text") && lower.includes("input")) ||
    lower === "combobox" ||
    lower === "spinbutton" ||
    lower === "slider" ||
    lower === "checkbox" ||
    lower === "radio" ||
    lower === "textbox"
  ) {
    return "success";
  }
  if (lower === "statictext" || lower === "text" || lower === "label" || lower === "heading") {
    return "muted";
  }
  if (
    lower === "window" ||
    lower === "pane" ||
    lower === "group" ||
    lower === "list" ||
    lower === "tree" ||
    lower === "table" ||
    lower === "toolbar" ||
    lower === "menubar" ||
    lower === "menu" ||
    lower === "dialog" ||
    lower === "tabpanel"
  ) {
    return "default";
  }
  return "purple";
}

interface TreeNodeViewProps {
  node: UnifiedNode;
  depth: number;
  selectedRef: string | null;
  onSelect: (node: UnifiedNode) => void;
  defaultExpanded?: boolean;
}

const TreeNodeView = memo(function TreeNodeView({
  node,
  depth,
  selectedRef,
  onSelect,
  defaultExpanded = false,
}: TreeNodeViewProps) {
  const [expanded, setExpanded] = useState(defaultExpanded || depth < 1);
  const hasChildren = node.children.length > 0;
  const isSelected = selectedRef === node.ref;

  const handleToggle = useCallback((e: React.MouseEvent) => {
    e.stopPropagation();
    setExpanded((prev) => !prev);
  }, []);

  const handleSelect = useCallback(() => {
    onSelect(node);
  }, [onSelect, node]);

  const displayName = useMemo(() => {
    if (node.name) return node.name;
    if (node.value) return `[${node.value}]`;
    return "";
  }, [node.name, node.value]);

  const roleBadgeVariant = useMemo(() => getRoleBadgeVariant(node.role), [node.role]);

  return (
    <div>
      <div
        role="treeitem"
        aria-expanded={hasChildren ? expanded : undefined}
        aria-selected={isSelected}
        tabIndex={0}
        onClick={handleSelect}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            handleSelect();
          }
          if (e.key === "ArrowRight" && hasChildren && !expanded) {
            e.preventDefault();
            setExpanded(true);
          }
          if (e.key === "ArrowLeft" && expanded) {
            e.preventDefault();
            setExpanded(false);
          }
        }}
        className={cn(
          "flex items-center gap-1.5 py-0.5 px-1 rounded cursor-pointer select-none",
          "hover:bg-muted/40 transition-colors text-sm",
          isSelected && "bg-primary/10 ring-1 ring-primary/30",
          node.is_interactive && !isSelected && "bg-blue-500/5",
        )}
        style={{ paddingLeft: `${depth * 16 + 4}px` }}
      >
        {/* Expand/Collapse toggle */}
        {hasChildren ? (
          <button
            type="button"
            onClick={handleToggle}
            className="flex-shrink-0 p-0.5 rounded hover:bg-muted/60"
            aria-label={expanded ? "Collapse" : "Expand"}
          >
            {expanded ? (
              <ChevronDown className="w-3.5 h-3.5 text-muted-foreground" />
            ) : (
              <ChevronRight className="w-3.5 h-3.5 text-muted-foreground" />
            )}
          </button>
        ) : (
          <span className="w-[18px] flex-shrink-0" />
        )}

        {/* Role badge */}
        <Badge variant={roleBadgeVariant} size="sm" className="flex-shrink-0">
          {node.role}
        </Badge>

        {/* Name */}
        {displayName && (
          <span className="truncate text-foreground" title={displayName}>
            {displayName}
          </span>
        )}

        {/* Ref ID */}
        <span className="ml-auto flex-shrink-0 font-mono text-[10px] text-muted-foreground/60">
          {node.ref}
        </span>

        {/* Interactive indicator */}
        {node.is_interactive && (
          <span
            className="flex-shrink-0 w-1.5 h-1.5 rounded-full bg-blue-400"
            title="Interactive"
          />
        )}
      </div>

      {/* Children */}
      {hasChildren && expanded && (
        <div role="group">
          {node.children.map((child) => (
            <TreeNodeView
              key={child.ref}
              node={child}
              depth={depth + 1}
              selectedRef={selectedRef}
              onSelect={onSelect}
            />
          ))}
        </div>
      )}
    </div>
  );
});

export default TreeNodeView;
