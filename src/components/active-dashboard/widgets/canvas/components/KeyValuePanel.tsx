/**
 * KeyValuePanel Component
 *
 * Renders key-value pairs with optional colored status badges.
 */

import { cn } from "../../../../../lib/utils";
import { Badge } from "../../../../ui";
import type { CanvasPanelComponentProps } from "./CanvasComponentRegistry";

interface KVPair {
  key: string;
  value: string | number | boolean;
  style?: "default" | "success" | "warning" | "error";
}

const STYLE_CLASSES: Record<string, string> = {
  default: "text-foreground",
  success: "text-green-500",
  warning: "text-yellow-500",
  error: "text-red-500",
};

export function KeyValuePanel({ data, size }: CanvasPanelComponentProps) {
  const pairs = (data.pairs as KVPair[]) ?? [];
  const isCompact = size === "compact";

  if (pairs.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No data</p>;
  }

  return (
    <div className={cn("space-y-1", isCompact ? "text-xs" : "text-sm")}>
      {pairs.map((pair, idx) => (
        <div key={idx} className="flex items-center gap-3">
          <span className="text-muted-foreground font-medium min-w-[100px] flex-shrink-0">
            {pair.key}
          </span>
          <span className={cn("font-mono", STYLE_CLASSES[pair.style ?? "default"])}>
            {typeof pair.value === "boolean" ? (pair.value ? "true" : "false") : String(pair.value)}
          </span>
        </div>
      ))}
    </div>
  );
}
