/**
 * SpecTree — Left sidebar showing hierarchical spec navigation
 */

import {
  ChevronDown,
  ChevronRight,
  Shield,
  Palette,
  Server,
  Globe,
  FileJson,
  Database,
  GitBranch,
  Gauge,
} from "lucide-react";
import type { SpecTreeNode, SpecSelection } from "./types";

// ============================================================================
// Severity badge colors
// ============================================================================

const _CATEGORY_COLORS: Record<string, string> = {
  semantic: "text-purple-400",
  "element-presence": "text-blue-400",
  "state-consistency": "text-amber-400",
  "form-validation": "text-green-400",
  accessibility: "text-cyan-400",
  navigation: "text-indigo-400",
  "cross-page-consistency": "text-pink-400",
  "modal-dialog": "text-orange-400",
  design: "text-rose-400",
  custom: "text-gray-400",
};

function getTypeIcon(type: SpecTreeNode["type"]) {
  switch (type) {
    case "page-spec":
      return Shield;
    case "style-guide":
      return Palette;
    case "architecture":
      return Server;
    case "api":
      return Globe;
    case "data":
      return Database;
    case "dependency":
      return GitBranch;
    case "constraint":
      return Gauge;
    default:
      return FileJson;
  }
}

// ============================================================================
// Tree Node Component
// ============================================================================

interface TreeNodeProps {
  node: SpecTreeNode;
  depth: number;
  expanded: boolean;
  selected: boolean;
  onToggle: (nodeId: string) => void;
  onSelect: (node: SpecTreeNode) => void;
  expandedNodes: Set<string>;
  selection: SpecSelection;
}

function TreeNode({
  node,
  depth,
  expanded,
  selected,
  onToggle,
  onSelect,
  expandedNodes,
  selection,
}: TreeNodeProps) {
  const hasChildren = node.children && node.children.length > 0;
  const Icon = getTypeIcon(node.type);
  const isGroup = node.type === "group";
  const isUnspecced = node.unspecced === true;

  return (
    <div>
      <button
        onClick={() => {
          if (hasChildren) onToggle(node.id);
          onSelect(node);
        }}
        className={`w-full flex items-center gap-1.5 px-2 py-1 text-left text-xs rounded
          hover:bg-white/5 transition-colors
          ${selected ? "bg-white/10 text-white" : isUnspecced ? "text-muted-foreground/40 italic" : "text-muted-foreground"}`}
        style={{ paddingLeft: `${depth * 12 + 8}px` }}
      >
        {hasChildren ? (
          expanded ? (
            <ChevronDown className="w-3 h-3 shrink-0 opacity-50" />
          ) : (
            <ChevronRight className="w-3 h-3 shrink-0 opacity-50" />
          )
        ) : (
          <span className="w-3 shrink-0" />
        )}

        {!isGroup && (
          <Icon className={`w-3 h-3 shrink-0 ${isUnspecced ? "opacity-30" : "opacity-60"}`} />
        )}

        <span className="truncate flex-1">{node.label}</span>

        {isUnspecced && (
          <span className="text-[9px] px-1 py-0.5 rounded bg-amber-500/10 text-amber-500/60 border border-amber-500/20 shrink-0 not-italic">
            no spec
          </span>
        )}

        {!isUnspecced && node.childCount !== undefined && node.childCount > 0 && (
          <span className="text-[10px] opacity-40 tabular-nums shrink-0">{node.childCount}</span>
        )}
      </button>

      {hasChildren && expanded && (
        <div>
          {node.children!.map((child) => {
            const childExpanded = expandedNodes.has(child.id);
            const childSelected =
              (selection.type === "group" &&
                child.specId === selection.specId &&
                child.groupId === selection.groupId) ||
              (selection.type === "spec" && child.specId === selection.specId && !child.groupId);

            return (
              <TreeNode
                key={child.id}
                node={child}
                depth={depth + 1}
                expanded={childExpanded}
                selected={childSelected}
                onToggle={onToggle}
                onSelect={onSelect}
                expandedNodes={expandedNodes}
                selection={selection}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// SpecTree Component
// ============================================================================

interface SpecTreeProps {
  tree: SpecTreeNode[];
  expandedNodes: Set<string>;
  selection: SpecSelection;
  onToggle: (nodeId: string) => void;
  onSelect: (node: SpecTreeNode) => void;
}

export function SpecTree({ tree, expandedNodes, selection, onToggle, onSelect }: SpecTreeProps) {
  if (tree.length === 0) {
    return (
      <div className="px-3 py-6 text-center text-xs text-muted-foreground">
        No specs loaded.
        <br />
        Load bundled specs or connect to an app.
      </div>
    );
  }

  // Show app name header when all specs are from the same app (no app grouping in tree)
  const firstAppName = tree[0]?.appName;
  const singleApp =
    firstAppName && tree.every((n) => n.appName === firstAppName) ? firstAppName : null;

  return (
    <div className="py-1">
      {singleApp && (
        <div className="px-3 py-1 text-[10px] font-medium text-cyan-400/70 uppercase tracking-wider">
          {singleApp}
        </div>
      )}
      {tree.map((node) => {
        const expanded = expandedNodes.has(node.id);
        const selected =
          selection.type === "spec" && node.specId === selection.specId && !node.groupId;

        return (
          <TreeNode
            key={node.id}
            node={node}
            depth={0}
            expanded={expanded}
            selected={selected}
            onToggle={onToggle}
            onSelect={onSelect}
            expandedNodes={expandedNodes}
            selection={selection}
          />
        );
      })}
    </div>
  );
}
