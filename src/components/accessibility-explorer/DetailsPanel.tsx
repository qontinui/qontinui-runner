/**
 * DetailsPanel.tsx
 *
 * Shows all properties of the selected accessibility node
 * and provides action buttons (Click, Focus, Type).
 */

import { MousePointer, Type, Focus, Copy } from "lucide-react";
import { cn } from "../../lib/utils";
import { Button, Badge } from "../ui";
import type { UnifiedNode } from "./types";

async function copyToClipboard(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // fallback: no-op
  }
}

export interface DetailsPanelProps {
  node: UnifiedNode;
  stateBadges: Array<{ label: string; active: boolean }>;
  actionMessage: string | null;
  showTypeInput: boolean;
  typeText: string;
  onTypeTextChange: (v: string) => void;
  onShowTypeInput: () => void;
  onClickAction: () => void;
  onFocusAction: () => void;
  onTypeAction: () => void;
}

export function DetailsPanel({
  node,
  stateBadges,
  actionMessage,
  showTypeInput,
  typeText,
  onTypeTextChange,
  onShowTypeInput,
  onClickAction,
  onFocusAction,
  onTypeAction,
}: DetailsPanelProps) {
  return (
    <div className="p-3 space-y-3 text-sm">
      {/* Identity */}
      <div className="space-y-1.5">
        <DetailRow label="Role" value={node.role} />
        {node.name && <DetailRow label="Name" value={node.name} />}
        {node.value && <DetailRow label="Value" value={node.value} />}
        {node.description && <DetailRow label="Description" value={node.description} />}
        <div className="flex items-center justify-between">
          <span className="text-muted-foreground text-xs">Ref ID</span>
          <div className="flex items-center gap-1">
            <code className="font-mono text-xs bg-muted/30 px-1.5 py-0.5 rounded">
              {node.ref}
            </code>
            <button
              type="button"
              onClick={() => copyToClipboard(node.ref)}
              className="p-0.5 rounded hover:bg-muted/40"
              title="Copy ref ID"
            >
              <Copy className="w-3 h-3 text-muted-foreground" />
            </button>
          </div>
        </div>
        <DetailRow label="Source" value={node.source.toUpperCase()} />
      </div>

      {/* Bounds */}
      {node.bounds && (
        <div>
          <span className="text-xs text-muted-foreground">Bounds</span>
          <div className="grid grid-cols-4 gap-1 mt-1">
            {(["x", "y", "width", "height"] as const).map((k) => (
              <div key={k} className="text-center">
                <div className="text-[10px] text-muted-foreground/60">{k}</div>
                <div className="text-xs font-mono">{Math.round(node.bounds![k])}</div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* State flags */}
      <div>
        <span className="text-xs text-muted-foreground">State</span>
        <div className="flex flex-wrap gap-1 mt-1">
          {stateBadges.map(({ label, active }) => (
            <Badge key={label} variant={active ? "success" : "outline"} size="sm">
              {label}
            </Badge>
          ))}
        </div>
      </div>

      {/* Patterns */}
      {node.supported_patterns.length > 0 && (
        <div>
          <span className="text-xs text-muted-foreground">Patterns</span>
          <div className="flex flex-wrap gap-1 mt-1">
            {node.supported_patterns.map((p) => (
              <Badge key={p} variant="purple" size="sm">
                {p}
              </Badge>
            ))}
          </div>
        </div>
      )}

      {/* Actions */}
      <div className="pt-2 border-t border-border/30 space-y-2">
        <div className="flex items-center gap-1.5">
          <Button variant="outline" size="sm" onClick={onClickAction}>
            <MousePointer className="w-3 h-3" /> Click
          </Button>
          <Button variant="outline" size="sm" onClick={onFocusAction}>
            <Focus className="w-3 h-3" /> Focus
          </Button>
          <Button variant="outline" size="sm" onClick={onShowTypeInput}>
            <Type className="w-3 h-3" /> Type...
          </Button>
        </div>
        {showTypeInput && (
          <div className="flex items-center gap-1.5">
            <input
              type="text"
              value={typeText}
              onChange={(e) => onTypeTextChange(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && onTypeAction()}
              placeholder="Text to type..."
              className="flex-1 px-2 py-1 text-xs rounded bg-muted/30 border border-border text-foreground placeholder:text-muted-foreground/50 focus:outline-none focus:ring-1 focus:ring-primary/50"
            />
            <Button variant="primary" size="sm" onClick={onTypeAction} disabled={!typeText}>
              Send
            </Button>
          </div>
        )}
        {actionMessage && (
          <div
            className={cn(
              "text-xs px-2 py-1 rounded",
              actionMessage.startsWith("Error") || actionMessage.startsWith("Failed")
                ? "bg-red-500/10 text-red-400"
                : "bg-green-500/10 text-green-400",
            )}
          >
            {actionMessage}
          </div>
        )}
      </div>
    </div>
  );
}

function DetailRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-muted-foreground text-xs">{label}</span>
      <span className="text-xs font-medium truncate ml-2">{value}</span>
    </div>
  );
}
