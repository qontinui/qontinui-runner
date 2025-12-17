/**
 * MonitorSelector Component
 *
 * Visual monitor selection component showing monitor layout with size and position info.
 * Supports both single-select and multi-select modes.
 * Supports read-only mode for calculated monitors with override capability.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Monitor, Check, Pencil, RotateCcw, AlertTriangle } from "lucide-react";

export interface MonitorInfo {
  index: number;
  x: number;
  y: number;
  width: number;
  height: number;
  scale: number;
  is_primary: boolean;
  name?: string;
}

interface MonitorSelectorProps {
  /** Selected monitor indices */
  selectedMonitors: number[];
  /** Called when selection changes */
  onSelectionChange: (indices: number[]) => void;
  /** Enable multi-select mode (default: true) */
  multiSelect?: boolean;
  /** Compact mode for smaller spaces */
  compact?: boolean;
  /** Optional class name */
  className?: string;
  /** Whether the selector is read-only (calculated mode, not editable) */
  readOnly?: boolean;
  /** Whether override mode is active (user is manually editing) */
  isOverrideActive?: boolean;
  /** Called to toggle override mode */
  onOverrideToggle?: () => void;
}

/**
 * Get position label based on monitor X coordinate relative to others
 */
function getPositionLabel(monitor: MonitorInfo, allMonitors: MonitorInfo[]): string {
  if (allMonitors.length === 1) return "";

  const sorted = [...allMonitors].sort((a, b) => a.x - b.x);
  const idx = sorted.findIndex((m) => m.index === monitor.index);

  if (sorted.length === 2) {
    return idx === 0 ? "Left" : "Right";
  }

  if (idx === 0) return "Left";
  if (idx === sorted.length - 1) return "Right";
  return "Center";
}

export function MonitorSelector({
  selectedMonitors,
  onSelectionChange,
  multiSelect = true,
  compact = false,
  className = "",
  readOnly = false,
  isOverrideActive = false,
  onOverrideToggle,
}: MonitorSelectorProps) {
  const [monitors, setMonitors] = useState<MonitorInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [showWarning, setShowWarning] = useState(false);

  useEffect(() => {
    loadMonitors();
  }, []);

  const loadMonitors = async () => {
    try {
      setLoading(true);
      const result = await invoke<{ success: boolean; data?: { monitors: MonitorInfo[] } }>(
        "get_monitors",
      );
      if (result?.success && result.data?.monitors) {
        // Sort monitors by x position (left to right)
        const sortedMonitors = [...result.data.monitors].sort((a, b) => a.x - b.x);
        setMonitors(sortedMonitors);
      }
    } catch (err) {
      console.error("Failed to load monitors:", err);
    } finally {
      setLoading(false);
    }
  };

  const handleMonitorClick = (index: number) => {
    // Don't allow changes in read-only mode
    if (readOnly) {
      return;
    }

    if (multiSelect) {
      // Toggle selection
      if (selectedMonitors.includes(index)) {
        // Don't allow deselecting if it's the only one selected
        if (selectedMonitors.length > 1) {
          onSelectionChange(selectedMonitors.filter((i) => i !== index));
        }
      } else {
        onSelectionChange([...selectedMonitors, index].sort((a, b) => a - b));
      }
    } else {
      // Single select
      onSelectionChange([index]);
    }
  };

  const handleOverrideToggle = () => {
    if (!onOverrideToggle) return;

    // If enabling override mode (Edit button), show warning
    if (!isOverrideActive) {
      setShowWarning(true);
      // Auto-hide warning after 5 seconds
      setTimeout(() => setShowWarning(false), 5000);
    }

    onOverrideToggle();
  };

  if (loading) {
    return <div className={`text-sm text-muted-foreground ${className}`}>Loading monitors...</div>;
  }

  if (monitors.length === 0) {
    return <div className={`text-sm text-muted-foreground ${className}`}>No monitors detected</div>;
  }

  const buttonSize = compact ? "p-2 min-w-[80px]" : "p-3 min-w-[100px]";
  const iconSize = compact ? "w-4 h-4" : "w-5 h-5";
  const textSize = compact ? "text-xs" : "text-sm";

  return (
    <div className={`flex flex-wrap items-center gap-2 ${className}`}>
      {monitors.map((monitor) => {
        const isSelected = selectedMonitors.includes(monitor.index);
        const position = getPositionLabel(monitor, monitors);

        return (
          <button
            key={monitor.index}
            onClick={() => handleMonitorClick(monitor.index)}
            disabled={readOnly}
            className={`flex flex-col items-center gap-1 rounded-lg border transition-all ${buttonSize} ${
              isSelected
                ? "bg-primary/20 border-primary text-primary ring-2 ring-primary/30"
                : "bg-input border-border/50 hover:border-primary/50 hover:bg-accent/50"
            } ${readOnly ? "cursor-default opacity-80" : "cursor-pointer"}`}
          >
            <div className="flex items-center gap-1.5">
              <Monitor className={iconSize} />
              <span className="font-medium">#{monitor.index}</span>
              {multiSelect && isSelected && <Check className={`${iconSize} text-primary`} />}
            </div>
            <div className={`${textSize} text-muted-foreground space-y-0.5`}>
              {monitor.is_primary && <div className="text-primary font-medium">Primary</div>}
              {position && !compact && <div>{position}</div>}
              <div>
                {monitor.width}x{monitor.height}
              </div>
            </div>
          </button>
        );
      })}

      {/* "All" option for multi-select (only when not read-only) */}
      {multiSelect && monitors.length > 1 && !readOnly && (
        <button
          onClick={() => onSelectionChange(monitors.map((m) => m.index))}
          className={`flex flex-col items-center justify-center gap-1 rounded-lg border transition-all ${buttonSize} ${
            selectedMonitors.length === monitors.length
              ? "bg-primary/20 border-primary text-primary ring-2 ring-primary/30"
              : "bg-input border-border/50 hover:border-primary/50 hover:bg-accent/50"
          }`}
        >
          <div className="flex items-center gap-1.5">
            <Monitor className={iconSize} />
            <span className="font-medium">All</span>
            {selectedMonitors.length === monitors.length && (
              <Check className={`${iconSize} text-primary`} />
            )}
          </div>
          <div className={`${textSize} text-muted-foreground`}>{monitors.length} monitors</div>
        </button>
      )}

      {/* Override toggle button */}
      {onOverrideToggle && (
        <button
          onClick={handleOverrideToggle}
          className={`flex items-center gap-1.5 px-2 py-1.5 rounded-md text-xs transition-colors ${
            isOverrideActive
              ? "bg-yellow-500/20 text-yellow-600 hover:bg-yellow-500/30"
              : "bg-muted/50 text-muted-foreground hover:bg-muted hover:text-foreground"
          }`}
          title={isOverrideActive ? "Use calculated monitors" : "Override automatic selection"}
        >
          {isOverrideActive ? (
            <>
              <RotateCcw className="w-3.5 h-3.5" />
              <span>Auto</span>
            </>
          ) : (
            <>
              <Pencil className="w-3.5 h-3.5" />
              <span>Edit</span>
            </>
          )}
        </button>
      )}

      {/* Warning when override is active */}
      {showWarning && isOverrideActive && (
        <div className="flex items-start gap-2 p-3 bg-yellow-500/10 border border-yellow-500/30 rounded-lg text-xs">
          <AlertTriangle className="w-4 h-4 flex-shrink-0 text-yellow-600 mt-0.5" />
          <div className="text-yellow-600">
            <strong>Efficiency Warning:</strong> Manual monitor selection will reduce efficiency by
            searching all selected monitors for images. By default, StateImages are only searched on
            their associated monitor(s).
          </div>
        </div>
      )}
    </div>
  );
}
