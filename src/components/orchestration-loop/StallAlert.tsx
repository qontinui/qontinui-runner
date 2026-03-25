import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AlertTriangle, RefreshCw, Clock, Repeat, X } from "lucide-react";
import { cn } from "../../lib/utils";

// --- Types matching backend stall detection ---

interface StallEvent {
  pattern_type: string;
  action_count: number;
  intervention: string;
  detected_at: string;
  iteration: number;
}

const PATTERN_ICONS: Record<string, React.ReactNode> = {
  repeated_action: <Repeat className="w-3.5 h-3.5" />,
  oscillation: <RefreshCw className="w-3.5 h-3.5" />,
  timeout: <Clock className="w-3.5 h-3.5" />,
};

const PATTERN_COLORS: Record<string, { border: string; bg: string; text: string }> = {
  repeated_action: {
    border: "border-yellow-500",
    bg: "bg-yellow-500/10",
    text: "text-yellow-400",
  },
  oscillation: {
    border: "border-orange-500",
    bg: "bg-orange-500/10",
    text: "text-orange-400",
  },
  timeout: {
    border: "border-red-500",
    bg: "bg-red-500/10",
    text: "text-red-400",
  },
};

const DEFAULT_COLORS = {
  border: "border-yellow-500",
  bg: "bg-yellow-500/10",
  text: "text-yellow-400",
};

function formatPatternType(type: string): string {
  return type
    .replace(/_/g, " ")
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

interface StallAlertProps {
  /** Whether the orchestration loop is currently running */
  running: boolean;
}

export function StallAlert({ running }: StallAlertProps) {
  const [stall, setStall] = useState<StallEvent | null>(null);
  const [dismissed, setDismissed] = useState(false);

  // Listen for stall events from the Tauri backend
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    const setup = async () => {
      try {
        unlisten = await listen<StallEvent>("orchestration-stall-detected", (event) => {
          setStall(event.payload);
          setDismissed(false);
        });
      } catch {
        // Event system may not be available yet
      }
    };

    if (running) {
      setup();
    }

    return () => {
      unlisten?.();
    };
  }, [running]);

  // Poll for stall status as a fallback
  const checkStallStatus = useCallback(async () => {
    if (!running) return;
    try {
      const result = await invoke<StallEvent | null>("get_current_stall");
      if (result) {
        setStall(result);
        setDismissed(false);
      }
    } catch {
      // Command may not exist yet — that's fine
    }
  }, [running]);

  useEffect(() => {
    if (!running) {
      setStall(null);
      setDismissed(false);
      return;
    }

    // Check once on mount and periodically
    checkStallStatus();
    const interval = setInterval(checkStallStatus, 10000);
    return () => clearInterval(interval);
  }, [running, checkStallStatus]);

  if (!stall || dismissed || !running) {
    return null;
  }

  const colors = PATTERN_COLORS[stall.pattern_type] || DEFAULT_COLORS;
  const icon = PATTERN_ICONS[stall.pattern_type] || <AlertTriangle className="w-3.5 h-3.5" />;

  return (
    <div
      className={cn(
        "border-l-2 rounded-r px-3 py-2 flex items-start gap-2",
        colors.border,
        colors.bg,
      )}
    >
      <span className={cn("shrink-0 mt-0.5", colors.text)}>{icon}</span>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className={cn("text-xs font-semibold", colors.text)}>
            Stall Detected
          </span>
          <span
            className={cn(
              "inline-flex px-1.5 py-0.5 rounded-full text-[0.65rem] font-mono font-semibold",
              colors.bg,
              colors.text,
            )}
          >
            {formatPatternType(stall.pattern_type)}
          </span>
          {stall.action_count > 0 && (
            <span className="text-[0.7rem] text-muted-foreground font-mono">
              {stall.action_count} actions
            </span>
          )}
        </div>
        <p className="text-xs text-muted-foreground mt-0.5 leading-snug">
          {stall.intervention}
        </p>
      </div>
      <button
        onClick={() => setDismissed(true)}
        className="shrink-0 text-muted-foreground hover:text-foreground transition-colors"
        title="Dismiss"
      >
        <X className="w-3.5 h-3.5" />
      </button>
    </div>
  );
}
