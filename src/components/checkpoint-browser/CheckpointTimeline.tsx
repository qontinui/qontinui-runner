/**
 * CheckpointTimeline.tsx
 *
 * Displays checkpoints as a vertical timeline with trigger indicators.
 * Uses virtual scrolling for large checkpoint lists (50+ items) to maintain performance.
 */

import { useCallback, type CSSProperties } from "react";
import {
  Clock,
  User,
  AlertTriangle,
  CheckCircle,
  XCircle,
  Shield,
  RotateCcw,
  Zap,
} from "lucide-react";
import type { CheckpointSummary } from "../../types/checkpoint";
import { TRIGGER_TYPE_LABELS, TRIGGER_TYPE_COLORS, formatTimestamp } from "../../types/checkpoint";
import { FixedVirtualList } from "../ui";

/** Threshold for switching to virtual scrolling */
const VIRTUAL_SCROLL_THRESHOLD = 50;
/** Height of each checkpoint item in pixels */
const CHECKPOINT_ITEM_HEIGHT = 120;

interface CheckpointTimelineProps {
  checkpoints: CheckpointSummary[];
  selectedId: string | null;
  compareId: string | null;
  onSelect: (id: string) => void;
  onCompare: (id: string) => void;
  compareMode: boolean;
  /** Currently active checkpoint ID (just created via realtime) for highlighting */
  activeId?: string | null;
}

function TriggerIcon({ type }: { type: string }) {
  switch (type) {
    case "manual":
      return <User className="w-4 h-4" />;
    case "automatic":
      return <Zap className="w-4 h-4" />;
    case "before_operation":
      return <Clock className="w-4 h-4" />;
    case "after_success":
      return <CheckCircle className="w-4 h-4" />;
    case "after_failure":
      return <XCircle className="w-4 h-4" />;
    case "verification":
      return <Shield className="w-4 h-4" />;
    case "iteration":
      return <RotateCcw className="w-4 h-4" />;
    default:
      return <AlertTriangle className="w-4 h-4" />;
  }
}

interface CheckpointItemProps {
  checkpoint: CheckpointSummary;
  isSelected: boolean;
  isCompare: boolean;
  isActive: boolean;
  compareMode: boolean;
  onSelect: () => void;
  onCompare: () => void;
  isFirst: boolean;
  isLast: boolean;
}

function CheckpointItem({
  checkpoint,
  isSelected,
  isCompare,
  isActive,
  compareMode,
  onSelect,
  onCompare,
  isFirst,
  isLast,
}: CheckpointItemProps) {
  const triggerColor = TRIGGER_TYPE_COLORS[checkpoint.trigger_type] || "#6b7280";
  const triggerLabel = TRIGGER_TYPE_LABELS[checkpoint.trigger_type] || checkpoint.trigger_type;

  return (
    <div className={`relative flex gap-3 ${isActive ? "animate-pulse" : ""}`}>
      {/* Timeline line */}
      <div className="flex flex-col items-center">
        {/* Top line */}
        {!isFirst && <div className="w-0.5 h-4 bg-gray-700" />}

        {/* Dot */}
        <button
          onClick={compareMode ? onCompare : onSelect}
          className={`relative w-8 h-8 rounded-full flex items-center justify-center transition-all ${
            isSelected
              ? "bg-purple-600 ring-2 ring-purple-400"
              : isCompare
                ? "bg-orange-600 ring-2 ring-orange-400"
                : isActive
                  ? "bg-green-600 ring-2 ring-green-400"
                  : "bg-gray-700 hover:bg-gray-600"
          }`}
          style={{
            backgroundColor: isSelected || isCompare || isActive ? undefined : triggerColor,
          }}
        >
          <TriggerIcon type={checkpoint.trigger_type} />
        </button>

        {/* Bottom line */}
        {!isLast && <div className="flex-1 w-0.5 bg-gray-700" />}
      </div>

      {/* Content */}
      <div
        onClick={onSelect}
        className={`flex-1 pb-4 cursor-pointer group ${isFirst ? "" : "pt-1"}`}
      >
        <div
          className={`p-3 rounded-lg transition-colors ${
            isSelected
              ? "bg-purple-900/30 border border-purple-500"
              : isCompare
                ? "bg-orange-900/30 border border-orange-500"
                : isActive
                  ? "bg-green-900/30 border border-green-500"
                  : "bg-gray-800 hover:bg-gray-750 border border-transparent"
          }`}
        >
          {/* Header */}
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <span className="text-xs px-2 py-0.5 rounded-full bg-gray-700 text-gray-300">
                {triggerLabel}
              </span>
              <span className="text-xs text-gray-400">Iter {checkpoint.iteration}</span>
            </div>
            <span className="text-xs text-gray-500">{formatTimestamp(checkpoint.created_at)}</span>
          </div>

          {/* Name */}
          {checkpoint.name && (
            <div className="mt-2 text-sm font-medium text-white">{checkpoint.name}</div>
          )}

          {/* State */}
          <div className="mt-1 text-xs text-gray-400">
            State: <span className="text-gray-300">{checkpoint.state}</span>
          </div>

          {/* Tags */}
          {checkpoint.tags.length > 0 && (
            <div className="mt-2 flex flex-wrap gap-1">
              {checkpoint.tags.map((tag, idx) => (
                <span key={idx} className="text-xs bg-gray-700 text-gray-300 px-2 py-0.5 rounded">
                  {tag}
                </span>
              ))}
            </div>
          )}

          {/* Compare mode indicator */}
          {compareMode && !isSelected && !isCompare && (
            <div className="mt-2 text-xs text-gray-500 opacity-0 group-hover:opacity-100 transition-opacity">
              Click to compare
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export function CheckpointTimeline({
  checkpoints,
  selectedId,
  compareId,
  onSelect,
  onCompare,
  compareMode,
  activeId,
}: CheckpointTimelineProps) {
  // Sort by creation time (newest first)
  const sortedCheckpoints = [...checkpoints].sort(
    (a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime(),
  );

  // Use virtual scrolling for large lists
  const useVirtualScroll = sortedCheckpoints.length > VIRTUAL_SCROLL_THRESHOLD;

  // Render function for virtualized items
  const renderVirtualItem = useCallback(
    (cp: CheckpointSummary, index: number, style: CSSProperties) => (
      <div style={style}>
        <CheckpointItem
          checkpoint={cp}
          isSelected={cp.id === selectedId}
          isCompare={cp.id === compareId}
          isActive={cp.id === activeId}
          compareMode={compareMode}
          onSelect={() => onSelect(cp.id)}
          onCompare={() => onCompare(cp.id)}
          isFirst={index === 0}
          isLast={index === sortedCheckpoints.length - 1}
        />
      </div>
    ),
    [selectedId, compareId, activeId, compareMode, onSelect, onCompare, sortedCheckpoints.length],
  );

  // Key extractor for virtual list
  const getItemKey = useCallback((cp: CheckpointSummary) => cp.id, []);

  if (checkpoints.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-gray-500 p-4">
        <Clock className="w-8 h-8 mb-2" />
        <p>No checkpoints found</p>
        <p className="text-xs mt-1">Run tasks with checkpointing enabled</p>
      </div>
    );
  }

  return (
    <div className="h-full p-4">
      {useVirtualScroll ? (
        /* Virtual scrolling for large lists */
        <FixedVirtualList
          items={sortedCheckpoints}
          itemHeight={CHECKPOINT_ITEM_HEIGHT}
          renderItem={renderVirtualItem}
          getItemKey={getItemKey}
          overscanCount={5}
        />
      ) : (
        /* Regular scrolling for small lists */
        <div className="overflow-auto h-full">
          <div className="space-y-0">
            {sortedCheckpoints.map((cp, idx) => (
              <CheckpointItem
                key={cp.id}
                checkpoint={cp}
                isSelected={cp.id === selectedId}
                isCompare={cp.id === compareId}
                isActive={cp.id === activeId}
                compareMode={compareMode}
                onSelect={() => onSelect(cp.id)}
                onCompare={() => onCompare(cp.id)}
                isFirst={idx === 0}
                isLast={idx === sortedCheckpoints.length - 1}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
