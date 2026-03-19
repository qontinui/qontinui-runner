/**
 * ChecklistPanel Component
 *
 * Renders a checklist with progress tracking.
 * When not read-only and a panelId is provided, items are interactive —
 * clicking toggles the checked state and persists via PATCH API.
 */

import { useState, useCallback } from "react";
import { CheckSquare, Square } from "lucide-react";
import { cn } from "@/lib/utils";
import type { CanvasPanelComponentProps } from "./types";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface ChecklistItem {
  id: string;
  label: string;
  checked: boolean;
  description?: string;
}

export function ChecklistPanel({ data, size, panelId, readOnly }: CanvasPanelComponentProps) {
  const initialItems = (data.items as ChecklistItem[]) ?? [];
  const [localItems, setLocalItems] = useState<ChecklistItem[] | null>(null);
  const isCompact = size === "compact";

  // Use local state if we've made changes, otherwise use incoming data
  const items = localItems ?? initialItems;

  // Reset local state when upstream data changes (new items from data prop)
  // We detect this by comparing the items reference
  if (localItems && initialItems !== localItems) {
    // Check if upstream data is different from our local state
    const upstreamJson = JSON.stringify(initialItems);
    const localJson = JSON.stringify(localItems);
    if (upstreamJson !== localJson) {
      // Upstream changed (likely from a Tauri event reconciliation) — adopt upstream
      setLocalItems(null);
    }
  }

  const isInteractive = !readOnly && !!panelId;

  const handleToggle = useCallback(
    async (itemId: string) => {
      if (!isInteractive) return;

      // Optimistic local update
      const updatedItems = items.map((item) =>
        item.id === itemId ? { ...item, checked: !item.checked } : item,
      );
      setLocalItems(updatedItems);

      // Persist to backend
      try {
        await tracedFetch(`${getApiBase()}/canvas/panels/${panelId}`, {
          method: "PATCH",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ data: { items: updatedItems } }),
        });
      } catch {
        // Revert on failure
        setLocalItems(null);
      }
    },
    [isInteractive, items, panelId],
  );

  if (items.length === 0) {
    return <p className="text-sm text-muted-foreground italic">No items</p>;
  }

  const checkedCount = items.filter((i) => i.checked).length;

  return (
    <div className="space-y-1.5">
      {/* Summary with progress bar */}
      <div className="space-y-1">
        <div className="flex items-center justify-between text-xs">
          <span className="text-muted-foreground">
            {checkedCount}/{items.length} completed
          </span>
          {items.length > 0 && (
            <span
              className={cn(
                "font-mono",
                checkedCount === items.length ? "text-green-500" : "text-muted-foreground",
              )}
            >
              {Math.round((checkedCount / items.length) * 100)}%
            </span>
          )}
        </div>
        <div className="h-1.5 bg-muted rounded-full overflow-hidden">
          <div
            className={cn(
              "h-full rounded-full transition-all",
              checkedCount === items.length ? "bg-green-500" : "bg-blue-500",
            )}
            style={{
              width: `${items.length > 0 ? (checkedCount / items.length) * 100 : 0}%`,
            }}
          />
        </div>
      </div>

      {/* Items */}
      {items.map((item) => {
        const Icon = item.checked ? CheckSquare : Square;

        return (
          <div
            key={item.id}
            className={cn(
              "flex gap-2 items-start",
              isInteractive && "cursor-pointer hover:bg-muted/30 rounded px-1 -mx-1 py-0.5",
            )}
            onClick={isInteractive ? () => handleToggle(item.id) : undefined}
            role={isInteractive ? "button" : undefined}
            tabIndex={isInteractive ? 0 : undefined}
            onKeyDown={
              isInteractive
                ? (e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      handleToggle(item.id);
                    }
                  }
                : undefined
            }
          >
            <Icon
              className={cn(
                "h-4 w-4 flex-shrink-0 mt-0.5",
                item.checked ? "text-green-500" : "text-muted-foreground",
              )}
            />
            <div className="min-w-0">
              <span
                className={cn(
                  isCompact ? "text-xs" : "text-sm",
                  item.checked && "line-through text-muted-foreground",
                )}
              >
                {item.label}
              </span>
              {item.description && (
                <p
                  className={cn(
                    "text-muted-foreground mt-0.5",
                    isCompact ? "text-[10px]" : "text-xs",
                  )}
                >
                  {item.description}
                </p>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
