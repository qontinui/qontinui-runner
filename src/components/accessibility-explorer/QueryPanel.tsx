/**
 * QueryPanel.tsx
 *
 * Query bar for filtering accessibility tree nodes by role/label,
 * and AI Context tab for generating LLM-friendly tree summaries.
 */

import { Search, RefreshCw, Copy } from "lucide-react";
import { cn } from "../../lib/utils";
import { Button, Badge, ScrollArea } from "../ui";
import type { UnifiedNode } from "./types";

async function copyToClipboard(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // fallback: no-op
  }
}

const COMMON_ROLES = [
  "",
  "button",
  "checkbox",
  "combobox",
  "dialog",
  "edit",
  "group",
  "heading",
  "link",
  "list",
  "listitem",
  "menu",
  "menuitem",
  "pane",
  "radio",
  "slider",
  "spinbutton",
  "tab",
  "tabpanel",
  "text",
  "textbox",
  "toolbar",
  "tree",
  "treeitem",
  "window",
];

// ---------------------------------------------------------------------------
// Query Panel
// ---------------------------------------------------------------------------

export interface QueryPanelProps {
  queryRole: string;
  queryLabel: string;
  queryInteractiveOnly: boolean;
  queryResults: UnifiedNode[];
  queryLoading: boolean;
  onRoleChange: (v: string) => void;
  onLabelChange: (v: string) => void;
  onInteractiveOnlyChange: (v: boolean) => void;
  onSearch: () => void;
  onSelectResult: (node: UnifiedNode) => void;
}

export function QueryPanel({
  queryRole,
  queryLabel,
  queryInteractiveOnly,
  queryResults,
  queryLoading,
  onRoleChange,
  onLabelChange,
  onInteractiveOnlyChange,
  onSearch,
  onSelectResult,
}: QueryPanelProps) {
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <select
          value={queryRole}
          onChange={(e) => onRoleChange(e.target.value)}
          className="px-2 py-1 text-xs rounded bg-muted/30 border border-border text-foreground focus:outline-none focus:ring-1 focus:ring-primary/50"
        >
          {COMMON_ROLES.map((r) => (
            <option key={r} value={r}>
              {r || "Any role"}
            </option>
          ))}
        </select>
        <input
          type="text"
          value={queryLabel}
          onChange={(e) => onLabelChange(e.target.value)}
          placeholder="Label contains..."
          className="flex-1 px-2 py-1 text-xs rounded bg-muted/30 border border-border text-foreground placeholder:text-muted-foreground/50 focus:outline-none focus:ring-1 focus:ring-primary/50"
        />
        <label className="flex items-center gap-1 text-xs text-muted-foreground whitespace-nowrap cursor-pointer">
          <input
            type="checkbox"
            checked={queryInteractiveOnly}
            onChange={(e) => onInteractiveOnlyChange(e.target.checked)}
            className="rounded"
          />
          Interactive only
        </label>
        <Button variant="primary" size="sm" onClick={onSearch} disabled={queryLoading}>
          <Search className="w-3 h-3" />
          {queryLoading ? "..." : "Search"}
        </Button>
      </div>
      {queryResults.length > 0 && (
        <ScrollArea className="max-h-40">
          <div className="space-y-0.5">
            {queryResults.map((node) => (
              <button
                key={node.ref}
                type="button"
                onClick={() => onSelectResult(node)}
                className="w-full flex items-center gap-2 px-2 py-1 text-xs rounded hover:bg-muted/40 text-left"
              >
                <Badge variant="info" size="sm">
                  {node.role}
                </Badge>
                <span className="truncate">{node.name ?? "(no name)"}</span>
                <span className="ml-auto font-mono text-muted-foreground/60">{node.ref}</span>
              </button>
            ))}
          </div>
        </ScrollArea>
      )}
      {queryResults.length === 0 && !queryLoading && (
        <div className="text-xs text-muted-foreground text-center py-1">
          No results. Run a query above.
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// AI Context Panel
// ---------------------------------------------------------------------------

export interface AiContextPanelProps {
  aiContext: string;
  loading: boolean;
  onFetch: () => void;
}

export function AiContextPanel({ aiContext, loading, onFetch }: AiContextPanelProps) {
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <Button variant="outline" size="sm" onClick={onFetch} disabled={loading}>
          <RefreshCw className={cn("w-3 h-3", loading && "animate-spin")} />
          {loading ? "Loading..." : "Generate AI Context"}
        </Button>
        {aiContext && (
          <Button variant="ghost" size="sm" onClick={() => copyToClipboard(aiContext)}>
            <Copy className="w-3 h-3" /> Copy
          </Button>
        )}
      </div>
      {aiContext && (
        <pre className="text-xs font-mono bg-muted/20 border border-border/30 rounded-md p-3 max-h-48 overflow-auto whitespace-pre-wrap">
          {aiContext}
        </pre>
      )}
    </div>
  );
}
