/**
 * ActiveRunsBar Component
 *
 * Bottom bar showing compact cards for all non-primary active runs.
 * Enables quick switching between concurrent workflow runs.
 * Supports keyboard navigation with arrow keys and number shortcuts.
 */

import { useRef, useCallback, useEffect, useState } from "react";
import { Plus, Layers, Keyboard, Monitor, Cloud } from "lucide-react";
import { useActiveRuns } from "../../contexts/ActiveRunsContext";
import { CompactRunCard } from "./CompactRunCard";
import { NewRunDialog } from "./NewRunDialog";
import { Button } from "../ui";
import { getAccentColors } from "@/design-system";

/**
 * Props for ActiveRunsBar.
 */
export interface ActiveRunsBarProps {
  /** Callback when "New Run" button is clicked (deprecated - dialog handles this now) */
  onNewRun?: () => void;
  /** Callback after a new run/bridge is successfully created */
  onRunCreated?: (bridgeId: string) => void;
}

/**
 * Active runs bar component.
 *
 * Shows at the bottom of the dashboard when there are multiple active runs.
 * Displays compact cards for each non-selected run, allowing quick switching.
 *
 * Keyboard navigation:
 * - Left/Right arrows: Navigate between run cards
 * - Enter/Space: Select focused run
 * - 1-9: Quick select run by index
 * - Ctrl+Tab: Cycle to next run
 */
export function ActiveRunsBar({ onNewRun, onRunCreated }: ActiveRunsBarProps) {
  const { activeRuns, selectedRunId, selectRun, hasMultipleRuns, guiLockHolderId, bridgeCounts } =
    useActiveRuns();

  // Track focused run index for keyboard navigation (within the bar)
  const [focusedIndex, setFocusedIndex] = useState(-1);
  // Track dialog open state
  const [isNewRunDialogOpen, setIsNewRunDialogOpen] = useState(false);
  const cardRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const containerRef = useRef<HTMLDivElement>(null);

  // Get non-selected runs (these are the ones shown as cards)
  const otherRuns = activeRuns.filter((run) => run.runId !== selectedRunId);

  // Update refs array size
  useEffect(() => {
    cardRefs.current = cardRefs.current.slice(0, otherRuns.length);
  }, [otherRuns.length]);

  /**
   * Handle keyboard navigation within the bar.
   */
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      const maxIndex = otherRuns.length - 1;

      switch (e.key) {
        case "ArrowLeft":
          e.preventDefault();
          setFocusedIndex((prev) => (prev <= 0 ? maxIndex : prev - 1));
          break;

        case "ArrowRight":
          e.preventDefault();
          setFocusedIndex((prev) => (prev >= maxIndex ? 0 : prev + 1));
          break;

        case "Enter":
        case " ":
          e.preventDefault();
          if (focusedIndex >= 0 && focusedIndex < otherRuns.length) {
            selectRun(otherRuns[focusedIndex].runId);
            setFocusedIndex(-1); // Clear focus after selection
          }
          break;

        case "Escape":
          e.preventDefault();
          setFocusedIndex(-1);
          containerRef.current?.blur();
          break;

        case "Home":
          e.preventDefault();
          setFocusedIndex(0);
          break;

        case "End":
          e.preventDefault();
          setFocusedIndex(maxIndex);
          break;

        default:
          // Number keys 1-9 for quick selection
          if (e.key >= "1" && e.key <= "9") {
            const index = parseInt(e.key, 10) - 1;
            if (index < activeRuns.length) {
              e.preventDefault();
              selectRun(activeRuns[index].runId);
            }
          }
          break;
      }
    },
    [otherRuns, focusedIndex, selectRun, activeRuns],
  );

  /**
   * Global keyboard handler for Ctrl+Tab to cycle runs.
   */
  useEffect(() => {
    if (!hasMultipleRuns) return;

    const handleGlobalKeyDown = (e: KeyboardEvent) => {
      // Ctrl+Tab or Ctrl+` to cycle to next run
      if (e.ctrlKey && (e.key === "Tab" || e.key === "`")) {
        e.preventDefault();
        const currentIndex = activeRuns.findIndex((r) => r.runId === selectedRunId);
        const nextIndex = (currentIndex + 1) % activeRuns.length;
        selectRun(activeRuns[nextIndex].runId);
      }

      // Ctrl+Shift+Tab to cycle to previous run
      if (e.ctrlKey && e.shiftKey && e.key === "Tab") {
        e.preventDefault();
        const currentIndex = activeRuns.findIndex((r) => r.runId === selectedRunId);
        const prevIndex = currentIndex <= 0 ? activeRuns.length - 1 : currentIndex - 1;
        selectRun(activeRuns[prevIndex].runId);
      }
    };

    window.addEventListener("keydown", handleGlobalKeyDown);
    return () => window.removeEventListener("keydown", handleGlobalKeyDown);
  }, [hasMultipleRuns, activeRuns, selectedRunId, selectRun]);

  // Focus the card when focusedIndex changes
  useEffect(() => {
    if (focusedIndex >= 0 && cardRefs.current[focusedIndex]) {
      cardRefs.current[focusedIndex]?.focus();
    }
  }, [focusedIndex]);

  // Don't render if there's only one run (no need for the bar)
  if (!hasMultipleRuns && activeRuns.length <= 1) {
    return null;
  }

  return (
    <div
      ref={containerRef}
      role="listbox"
      aria-label="Active workflow runs"
      aria-activedescendant={
        focusedIndex >= 0 ? `run-${otherRuns[focusedIndex]?.runId}` : undefined
      }
      tabIndex={0}
      onKeyDown={handleKeyDown}
      onFocus={() => {
        if (focusedIndex < 0 && otherRuns.length > 0) {
          setFocusedIndex(0);
        }
      }}
      onBlur={(e) => {
        // Only clear focus if focus is leaving the container entirely
        if (!containerRef.current?.contains(e.relatedTarget as Node)) {
          setFocusedIndex(-1);
        }
      }}
      className="flex h-16 items-center gap-3 border-t border-border bg-card/50 px-4 focus:outline-hidden"
    >
      {/* Icon and label */}
      <div className="flex items-center gap-2 text-muted-foreground">
        <Layers className="h-4 w-4" />
        <span
          data-content-role="label"
          data-content-label="active run count"
          className="text-xs font-medium"
        >
          {activeRuns.length} active run{activeRuns.length !== 1 ? "s" : ""}
        </span>
      </div>

      {/* Separator */}
      <div className="h-8 w-px bg-border" />

      {/* Run cards */}
      <div className="flex items-center gap-2 overflow-x-auto flex-1 py-1">
        {/* Selected run indicator (brief) */}
        {selectedRunId && (
          <div
            data-content-role="label"
            data-content-label="primary run"
            className="flex items-center gap-1.5 px-2 py-1 rounded bg-primary/10 text-primary text-xs"
          >
            <span className="font-medium">Primary:</span>
            <span className="truncate max-w-[100px]">
              {activeRuns.find((r) => r.runId === selectedRunId)?.taskName || "Current"}
            </span>
          </div>
        )}

        {/* Other run cards */}
        {otherRuns.map((run, index) => (
          <CompactRunCard
            key={run.runId}
            ref={(el) => {
              cardRefs.current[index] = el;
            }}
            run={run}
            isSelected={false}
            isFocused={focusedIndex === index}
            index={index}
            onClick={() => {
              selectRun(run.runId);
              setFocusedIndex(-1);
            }}
          />
        ))}
      </div>

      {/* Bridge count summary */}
      {bridgeCounts.total > 0 && (
        <div className="flex items-center gap-2 text-xs">
          {bridgeCounts.gui > 0 && (
            <div
              className={`flex items-center gap-1 px-2 py-1 rounded ${getAccentColors("amber").bg} ${getAccentColors("amber").text}`}
              title={`${bridgeCounts.gui} GUI bridge${bridgeCounts.gui !== 1 ? "s" : ""}`}
            >
              <Monitor className="h-3 w-3" />
              <span
                data-content-role="metric"
                data-content-label="GUI bridge count"
                className="font-medium"
              >
                {bridgeCounts.gui}
              </span>
            </div>
          )}
          {bridgeCounts.headless > 0 && (
            <div
              className={`flex items-center gap-1 px-2 py-1 rounded ${getAccentColors("cyan").bg} ${getAccentColors("cyan").text}`}
              title={`${bridgeCounts.headless} Headless bridge${bridgeCounts.headless !== 1 ? "s" : ""}`}
            >
              <Cloud className="h-3 w-3" />
              <span
                data-content-role="metric"
                data-content-label="headless bridge count"
                className="font-medium"
              >
                {bridgeCounts.headless}
              </span>
            </div>
          )}
        </div>
      )}

      {/* Keyboard hint - shown on first focus */}
      <div
        className="flex items-center gap-1 text-muted-foreground/50 text-[10px]"
        title="Ctrl+Tab to cycle runs, Arrow keys to navigate"
      >
        <Keyboard className="h-3 w-3" />
        <span className="hidden sm:inline">Ctrl+Tab</span>
      </div>

      {/* GUI Lock indicator with enhanced styling */}
      {guiLockHolderId && (
        <div
          className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs relative ${
            getAccentColors("amber").bg
          } ${getAccentColors("amber").text}`}
          title="A run currently has exclusive GUI control"
        >
          <span
            data-content-role="status"
            data-content-label="GUI lock status"
            className="font-medium relative z-10"
          >
            GUI Lock
          </span>
          <div className="absolute inset-0 bg-amber-500/20 rounded animate-pulse" />
        </div>
      )}

      {/* New Run button */}
      <Button
        size="sm"
        variant="outline"
        onClick={() => setIsNewRunDialogOpen(true)}
        className="shrink-0 border-border bg-muted hover:bg-muted/80"
      >
        <Plus className="h-4 w-4 mr-1" />
        <span className="text-xs">New Run</span>
      </Button>

      {/* New Run Dialog */}
      <NewRunDialog
        open={isNewRunDialogOpen}
        onClose={() => setIsNewRunDialogOpen(false)}
        onSuccess={(bridgeId) => {
          // Call legacy onNewRun for backward compatibility if provided
          onNewRun?.();
          // Call new callback with bridge ID
          onRunCreated?.(bridgeId);
        }}
      />
    </div>
  );
}

export default ActiveRunsBar;
