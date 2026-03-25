import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Shrink } from "lucide-react";
import { cn } from "../../lib/utils";

// --- Types matching backend context summarization ---

interface SummarizationInfo {
  active: boolean;
  iterations_summarized: [number, number] | null;
  original_tokens: number;
  compressed_tokens: number;
  summarized_at: string | null;
}

function formatTokens(count: number): string {
  if (count >= 1000) {
    return `${(count / 1000).toFixed(count >= 10000 ? 0 : 1)}K`;
  }
  return String(count);
}

interface SummarizationBadgeProps {
  /** Whether the orchestration loop is running */
  running: boolean;
}

export function SummarizationBadge({ running }: SummarizationBadgeProps) {
  const [info, setInfo] = useState<SummarizationInfo | null>(null);
  const [showTooltip, setShowTooltip] = useState(false);

  const fetchInfo = useCallback(async () => {
    try {
      const result = await invoke<SummarizationInfo | null>(
        "get_summarization_info",
      );
      setInfo(result);
    } catch {
      // Command may not exist yet
    }
  }, []);

  useEffect(() => {
    if (!running) {
      setInfo(null);
      return;
    }

    fetchInfo();
    const interval = setInterval(fetchInfo, 5000);
    return () => clearInterval(interval);
  }, [running, fetchInfo]);

  if (!info || !info.active || !running) {
    return null;
  }

  const savingsPercent =
    info.original_tokens > 0
      ? Math.round(
          ((info.original_tokens - info.compressed_tokens) /
            info.original_tokens) *
            100,
        )
      : 0;

  const iterRange = info.iterations_summarized
    ? `${info.iterations_summarized[0]}-${info.iterations_summarized[1]}`
    : null;

  return (
    <div className="relative inline-flex">
      <button
        onMouseEnter={() => setShowTooltip(true)}
        onMouseLeave={() => setShowTooltip(false)}
        className={cn(
          "inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full",
          "bg-purple-500/10 text-purple-400",
          "text-[0.7rem] font-medium",
          "hover:bg-purple-500/20 transition-colors",
          "cursor-default",
        )}
      >
        <Shrink className="w-3 h-3" />
        <span>Context compressed</span>
        {iterRange && (
          <span className="font-mono text-purple-400/70">({iterRange})</span>
        )}
        <span className="font-mono">
          {formatTokens(info.original_tokens)} &rarr;{" "}
          {formatTokens(info.compressed_tokens)}
        </span>
      </button>

      {/* Tooltip */}
      {showTooltip && (
        <div
          className={cn(
            "absolute z-50 top-full left-1/2 -translate-x-1/2 mt-1.5",
            "bg-popover border border-border rounded-md shadow-lg",
            "px-3 py-2 min-w-[200px]",
          )}
        >
          <div className="space-y-1.5 text-xs">
            <div className="flex justify-between">
              <span className="text-muted-foreground">Original tokens</span>
              <span className="font-mono">
                {info.original_tokens.toLocaleString()}
              </span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">Compressed tokens</span>
              <span className="font-mono">
                {info.compressed_tokens.toLocaleString()}
              </span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">Savings</span>
              <span className="font-mono text-green-400">
                {savingsPercent}%
              </span>
            </div>
            {iterRange && (
              <div className="flex justify-between">
                <span className="text-muted-foreground">Iterations</span>
                <span className="font-mono">{iterRange}</span>
              </div>
            )}
            {info.summarized_at && (
              <div className="flex justify-between">
                <span className="text-muted-foreground">Summarized at</span>
                <span className="font-mono text-[0.65rem]">
                  {new Date(info.summarized_at).toLocaleTimeString()}
                </span>
              </div>
            )}
          </div>
          {/* Tooltip arrow */}
          <div className="absolute -top-1 left-1/2 -translate-x-1/2 w-2 h-2 bg-popover border-l border-t border-border rotate-45" />
        </div>
      )}
    </div>
  );
}
