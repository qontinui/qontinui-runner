/**
 * MonitorSelector Component
 *
 * Visual monitor selection component showing monitor layout with size and position info.
 * Supports both single-select and multi-select modes.
 * When readOnly is true, monitors are displayed as read-only (configured in qontinui-web).
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Monitor as MonitorIcon, Check, Info } from "lucide-react";
import type { Monitor } from "../types/geometry";
import { getAccentColors } from "@/design-system";

/**
 * Extended monitor info from the Rust backend.
 * Adds scale field that may be present in backend responses.
 */
interface BackendMonitor extends Monitor {
  /** Legacy scale field (use scale_factor from Monitor) */
  scale?: number;
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
  /** Whether the selector is read-only (monitors configured in qontinui-web) */
  readOnly?: boolean;
}

/**
 * Get position label based on monitor X coordinate relative to others
 */
function getPositionLabel(monitor: BackendMonitor, allMonitors: BackendMonitor[]): string {
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
}: MonitorSelectorProps) {
  const [monitors, setMonitors] = useState<BackendMonitor[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/immutability
    loadMonitors();
  }, []);

  const loadMonitors = async () => {
    try {
      setLoading(true);
      const result = await invoke<{ success: boolean; data?: { monitors: BackendMonitor[] } }>(
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
    <div className={`space-y-2 ${className}`}>
      {/* Info message when read-only */}
      {readOnly && (
        <div
          className={`flex items-start gap-2 p-2 ${getAccentColors("blue").bg} border ${getAccentColors("blue").border} rounded-lg text-xs`}
        >
          <Info className={`w-4 h-4 shrink-0 ${getAccentColors("blue").text} mt-0.5`} />
          <div className={getAccentColors("blue").text}>
            Monitors are configured per element in qontinui-web. To change monitors, update your
            workflow configuration.
          </div>
        </div>
      )}

      <div className="flex flex-wrap items-center gap-2">
        {monitors.map((monitor, spatialIndex) => {
          const isSelected = selectedMonitors.includes(monitor.index);
          const position = getPositionLabel(monitor, monitors);
          // Display spatial position (1-indexed, left to right) instead of Windows enumeration index
          const displayNumber = spatialIndex + 1;

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
                <MonitorIcon className={iconSize} />
                <span className="font-medium">#{displayNumber}</span>
                {multiSelect && isSelected && <Check className={`${iconSize} text-primary`} />}
              </div>
              <div className={`${textSize} text-muted-foreground space-y-0.5`}>
                {monitor.isPrimary && <div className="text-primary font-medium">Primary</div>}
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
              <MonitorIcon className={iconSize} />
              <span className="font-medium">All</span>
              {selectedMonitors.length === monitors.length && (
                <Check className={`${iconSize} text-primary`} />
              )}
            </div>
            <div className={`${textSize} text-muted-foreground`}>{monitors.length} monitors</div>
          </button>
        )}
      </div>
    </div>
  );
}
