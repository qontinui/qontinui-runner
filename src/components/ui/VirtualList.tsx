/**
 * VirtualList Component
 *
 * A virtualized list using react-window for efficient rendering of large lists.
 * Only renders items currently visible in the viewport.
 */

import { useRef, useCallback, type CSSProperties } from "react";
import {
  FixedSizeList,
  VariableSizeList,
  type ListChildComponentProps,
  type ListOnScrollProps,
} from "react-window";
import AutoSizer from "react-virtualized-auto-sizer";
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
  onScroll,
}: FixedVirtualListProps<T>) {
  const listRef = useRef<FixedSizeList>(null);

  const displayItems = reverseOrder ? [...items].reverse() : items;

  const handleScroll = useCallback(
    (_props: ListOnScrollProps) => {
      if (onScroll) {
        onScroll({} as React.UIEvent<HTMLDivElement>);
      }
    },
    [onScroll],
  );

  const Row = useCallback(
    ({ index, style }: ListChildComponentProps) => {
      const item = displayItems[index];
      const key = getItemKey ? getItemKey(item, index) : index;
      return (
        <div key={key} style={style}>
          {renderItem(item, index, style)}
        </div>
      );
    },
    [displayItems, renderItem, getItemKey],
  );

  if (items.length === 0) {
    return null;
  }

  return (
    <div className={cn("flex-1", className)}>
      <AutoSizer>
        {({ height, width }) => (
          <FixedSizeList
            ref={listRef}
            height={height}
            width={width}
            itemCount={displayItems.length}
            itemSize={itemHeight}
            overscanCount={overscanCount}
            onScroll={handleScroll}
          >
            {Row}
          </FixedSizeList>
        )}
      </AutoSizer>
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
  const listRef = useRef<VariableSizeList>(null);

  const displayItems = reverseOrder ? [...items].reverse() : items;

  const Row = useCallback(
    ({ index, style }: ListChildComponentProps) => {
      const item = displayItems[index];
      const key = getItemKey ? getItemKey(item, index) : index;
      return (
        <div key={key} style={style}>
          {renderItem(item, index, style)}
        </div>
      );
    },
    [displayItems, renderItem, getItemKey],
  );

  const getSize = useCallback(
    (index: number) => getItemHeight(displayItems[index], index),
    [displayItems, getItemHeight],
  );

  if (items.length === 0) {
    return null;
  }

  return (
    <div className={cn("flex-1", className)}>
      <AutoSizer>
        {({ height, width }) => (
          <VariableSizeList
            ref={listRef}
            height={height}
            width={width}
            itemCount={displayItems.length}
            itemSize={getSize}
            overscanCount={overscanCount}
          >
            {Row}
          </VariableSizeList>
        )}
      </AutoSizer>
    </div>
  );
}

export default FixedVirtualList;
