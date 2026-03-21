/**
 * VirtualList Component
 *
 * A virtualized list using react-window for efficient rendering of large lists.
 * Only renders items currently visible in the viewport.
 */

import { useCallback, type CSSProperties, type ReactElement } from "react";
import { List, useListRef, type RowComponentProps } from "react-window";
import { cn } from "../../lib/utils";

// ============================================================================
// Fixed Size Virtual List
// ============================================================================

interface FixedVirtualListProps<T> {
  /** Items to render */
  items: T[];
  /** Height of each item in pixels */
  itemHeight: number;
  /** Render function for each item */
  renderItem: (item: T, index: number, style: CSSProperties) => React.ReactNode;
  /** Optional className for the container */
  className?: string;
  /** Key extractor for items */
  getItemKey?: (item: T, index: number) => string | number;
  /** Whether to render in reverse order (newest first) */
  reverseOrder?: boolean;
  /** Overscan count (items to render outside viewport) */
  overscanCount?: number;
  /** Optional scroll event handler */
  onScroll?: (event: React.UIEvent<HTMLDivElement>) => void;
}

interface VirtualRowProps {
  displayItems: unknown[];
  renderItem: (item: unknown, index: number, style: CSSProperties) => React.ReactNode;
  getItemKey?: (item: unknown, index: number) => string | number;
}

function VirtualRow({
  index,
  style,
  displayItems,
  renderItem,
  getItemKey,
}: RowComponentProps<VirtualRowProps>): ReactElement | null {
  const item = displayItems[index];
  const key = getItemKey ? getItemKey(item, index) : index;
  return (
    <div key={key} style={style}>
      {renderItem(item, index, style)}
    </div>
  );
}

/**
 * Virtual list with fixed item heights.
 * Use when all items have the same height.
 */
export function FixedVirtualList<T>({
  items,
  itemHeight,
  renderItem,
  className,
  getItemKey,
  reverseOrder = false,
  overscanCount = 5,
}: FixedVirtualListProps<T>) {
  const listRef = useListRef(null);

  const displayItems = reverseOrder ? [...items].reverse() : items;

  if (items.length === 0) {
    return null;
  }

  return (
    <div className={cn("flex-1", className)}>
      <List
        listRef={listRef}
        rowCount={displayItems.length}
        rowHeight={itemHeight}
        rowComponent={VirtualRow}
        rowProps={{
          displayItems: displayItems as unknown[],
          renderItem: renderItem as VirtualRowProps["renderItem"],
          getItemKey: getItemKey as VirtualRowProps["getItemKey"],
        }}
        overscanCount={overscanCount}
        style={{ width: "100%", height: "100%" }}
      />
    </div>
  );
}

// ============================================================================
// Variable Size Virtual List
// ============================================================================

interface VariableVirtualListProps<T> {
  /** Items to render */
  items: T[];
  /** Function to get item height */
  getItemHeight: (item: T, index: number) => number;
  /** Render function for each item */
  renderItem: (item: T, index: number, style: CSSProperties) => React.ReactNode;
  /** Optional className for the container */
  className?: string;
  /** Key extractor for items */
  getItemKey?: (item: T, index: number) => string | number;
  /** Whether to render in reverse order (newest first) */
  reverseOrder?: boolean;
  /** Overscan count (items to render outside viewport) */
  overscanCount?: number;
}

/**
 * Virtual list with variable item heights.
 * Use when items have different heights.
 */
export function VariableVirtualList<T>({
  items,
  getItemHeight,
  renderItem,
  className,
  getItemKey,
  reverseOrder = false,
  overscanCount = 5,
}: VariableVirtualListProps<T>) {
  const listRef = useListRef(null);

  const displayItems = reverseOrder ? [...items].reverse() : items;

  const getSize = useCallback(
    (index: number) => getItemHeight(displayItems[index], index),
    [displayItems, getItemHeight],
  );

  if (items.length === 0) {
    return null;
  }

  return (
    <div className={cn("flex-1", className)}>
      <List
        listRef={listRef}
        rowCount={displayItems.length}
        rowHeight={getSize}
        rowComponent={VirtualRow}
        rowProps={{
          displayItems: displayItems as unknown[],
          renderItem: renderItem as VirtualRowProps["renderItem"],
          getItemKey: getItemKey as VirtualRowProps["getItemKey"],
        }}
        overscanCount={overscanCount}
        style={{ width: "100%", height: "100%" }}
      />
    </div>
  );
}

export default FixedVirtualList;
