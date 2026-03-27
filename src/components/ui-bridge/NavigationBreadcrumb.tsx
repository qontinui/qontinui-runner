/**
 * NavigationBreadcrumb Component
 *
 * Displays a clickable breadcrumb trail showing navigation history.
 * Format: Graph > State: Login Form > Fingerprint: fp_abc123
 */

import { memo, useState } from "react";
import { ChevronRight, Share2, Layers, GitBranch, Fingerprint, X } from "lucide-react";
import type { NavigationHistory, NavigationItem, NavigationItemType } from "./navigation";
import { truncateLabel } from "./navigation";

// =============================================================================
// Types
// =============================================================================

interface NavigationBreadcrumbProps {
  /** Navigation history to display */
  history: NavigationHistory;
  /** Callback when a breadcrumb item is clicked */
  onNavigate: (item: NavigationItem) => void;
  /** Callback to clear navigation */
  onClear?: () => void;
  /** Current selection info for displaying even without history */
  currentSelection?: {
    type: NavigationItemType;
    id: string;
    label: string;
  } | null;
}

// =============================================================================
// Helper Components
// =============================================================================

/** Get icon component for navigation item type */
function NavigationIcon({
  type,
  className = "w-3 h-3",
}: {
  type: NavigationItemType;
  className?: string;
}) {
  switch (type) {
    case "graph":
      return <Share2 className={className} />;
    case "state":
      return <Layers className={className} />;
    case "transition":
      return <GitBranch className={className} />;
    case "fingerprint":
      return <Fingerprint className={className} />;
    default:
      return null;
  }
}

/** Get type label for navigation item */
function getTypeLabel(type: NavigationItemType): string {
  switch (type) {
    case "graph":
      return "Graph";
    case "state":
      return "State";
    case "transition":
      return "Transition";
    case "fingerprint":
      return "Fingerprint";
    default:
      return "";
  }
}

/** Single breadcrumb item */
function BreadcrumbItem({
  item,
  isLast,
  onClick,
}: {
  item: NavigationItem;
  isLast: boolean;
  onClick: () => void;
}) {
  const typeLabel = getTypeLabel(item.type);
  const displayLabel = truncateLabel(item.label, 20);

  return (
    <button
      onClick={onClick}
      className={`
        flex items-center gap-1.5 px-2 py-1 rounded text-xs
        transition-colors
        ${isLast ? "bg-primary/20 text-primary font-medium" : "text-muted-foreground hover:text-foreground hover:bg-muted/50"}
      `}
      title={`${typeLabel}: ${item.label}`}
    >
      <NavigationIcon type={item.type} className="w-3 h-3 shrink-0" />
      {item.type !== "graph" && <span className="text-muted-foreground">{typeLabel}:</span>}
      <span className="truncate max-w-[120px]">{displayLabel}</span>
    </button>
  );
}

// =============================================================================
// Main Component
// =============================================================================

function NavigationBreadcrumbComponent({
  history,
  onNavigate,
  onClear,
  currentSelection,
}: NavigationBreadcrumbProps) {
  const [now] = useState(() => Date.now());

  // Combine history items with current selection if not already in history
  const displayItems = [...history.items];

  // Add current selection if it's not the last history item
  if (currentSelection) {
    const lastItem = displayItems[displayItems.length - 1];
    const isSameAsLast =
      lastItem && lastItem.type === currentSelection.type && lastItem.id === currentSelection.id;

    if (!isSameAsLast) {
      displayItems.push({
        type: currentSelection.type,
        id: currentSelection.id,
        label: currentSelection.label,
        timestamp: now,
      });
    }
  }

  // Don't render if no items
  if (displayItems.length === 0) {
    return null;
  }

  return (
    <div className="flex items-center gap-1 px-1 py-1 bg-muted/20 rounded-lg border border-border/30">
      {displayItems.map((item, index) => (
        <div key={`${item.type}-${item.id}-${index}`} className="flex items-center">
          {index > 0 && <ChevronRight className="w-3 h-3 text-muted-foreground/50 mx-0.5" />}
          <BreadcrumbItem
            item={item}
            isLast={index === displayItems.length - 1}
            onClick={() => onNavigate(item)}
          />
        </div>
      ))}

      {/* Clear button */}
      {onClear && displayItems.length > 0 && (
        <button
          onClick={onClear}
          className="ml-1 p-1 rounded hover:bg-muted/50 text-muted-foreground hover:text-foreground transition-colors"
          title="Clear navigation"
        >
          <X className="w-3 h-3" />
        </button>
      )}
    </div>
  );
}

export const NavigationBreadcrumb = memo(NavigationBreadcrumbComponent);

export default NavigationBreadcrumb;
