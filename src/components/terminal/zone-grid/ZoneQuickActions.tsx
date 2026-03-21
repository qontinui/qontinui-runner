import { useState, useCallback, useRef, useEffect } from "react";
import { Maximize2, Pin, Copy, ArrowDownToLine, RotateCcw, Download } from "lucide-react";
import type { SessionState } from "../useZoneLayout";

export function ZoneQuickActions({
  zoneIndex: _zoneIndex,
  isPinned,
  onTogglePin,
  onMaximize,
  onCopyOutput,
  onScrollToBottom,
  lastLines,
  state,
  onRestart,
  onExportZone,
}: {
  zoneIndex: number;
  isPinned?: boolean;
  onTogglePin?: () => void;
  onMaximize: () => void;
  onCopyOutput: () => void;
  onScrollToBottom?: () => void;
  lastLines: string[];
  state?: SessionState;
  onRestart?: () => void;
  onExportZone?: (format: "text" | "markdown" | "json") => void;
}) {
  const [visible, setVisible] = useState(false);
  const [copied, setCopied] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const exportRef = useRef<HTMLDivElement>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (!exportOpen) return;
    const handler = (e: MouseEvent) => {
      if (exportRef.current && !exportRef.current.contains(e.target as Node)) {
        setExportOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [exportOpen]);

  const show = useCallback(() => {
    timerRef.current = setTimeout(() => setVisible(true), 500);
  }, []);
  const hide = useCallback(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    setVisible(false);
    setExportOpen(false);
  }, []);

  if (!visible) {
    return (
      <div
        className="absolute inset-0 z-[5]"
        onMouseEnter={show}
        onMouseLeave={hide}
        style={{ pointerEvents: "none" }}
      />
    );
  }

  return (
    <>
      <div
        className="absolute inset-0 z-[5]"
        onMouseLeave={hide}
        style={{ pointerEvents: "none" }}
      />
      <div
        className="absolute top-1 right-1 z-20 flex items-center gap-0.5 px-1 py-0.5 bg-[#1a1b26]/90 border border-[#2a2d3d] rounded-md shadow-lg backdrop-blur-sm"
        onMouseEnter={() => setVisible(true)}
        onMouseLeave={hide}
      >
        {onTogglePin && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onTogglePin();
            }}
            onMouseDown={(e) => e.stopPropagation()}
            className={`p-1 rounded transition-colors ${isPinned ? "text-[#7aa2f7]" : "text-[#565f89] hover:text-[#a9b1d6]"}`}
            title={isPinned ? "Unpin" : "Pin"}
          >
            <Pin className="w-3 h-3" />
          </button>
        )}
        <button
          onClick={(e) => {
            e.stopPropagation();
            onMaximize();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] transition-colors"
          title="Maximize"
        >
          <Maximize2 className="w-3 h-3" />
        </button>
        <button
          onClick={(e) => {
            e.stopPropagation();
            onCopyOutput();
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
          onMouseDown={(e) => e.stopPropagation()}
          className={`p-1 rounded transition-colors ${copied ? "text-[#9ece6a]" : "text-[#565f89] hover:text-[#a9b1d6]"}`}
          title={copied ? "Copied!" : "Copy output"}
          disabled={lastLines.length === 0}
        >
          <Copy className="w-3 h-3" />
        </button>
        {onScrollToBottom && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onScrollToBottom();
            }}
            onMouseDown={(e) => e.stopPropagation()}
            className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] transition-colors"
            title="Scroll to bottom"
          >
            <ArrowDownToLine className="w-3 h-3" />
          </button>
        )}
        {(state === "completed" || state === "error") && onRestart && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onRestart();
            }}
            onMouseDown={(e) => e.stopPropagation()}
            className="p-1 rounded text-[#7aa2f7] hover:text-[#89b4fa] transition-colors"
            title="Restart session"
          >
            <RotateCcw className="w-3 h-3" />
          </button>
        )}
        {onExportZone && (
          <div ref={exportRef} className="relative">
            <button
              onClick={(e) => {
                e.stopPropagation();
                setExportOpen((v) => !v);
              }}
              onMouseDown={(e) => e.stopPropagation()}
              className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] transition-colors"
              title="Export output"
              disabled={lastLines.length === 0}
            >
              <Download className="w-3 h-3" />
            </button>
            {exportOpen && (
              <div className="absolute top-full right-0 mt-1 z-50 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl min-w-[120px] py-1">
                {(
                  [
                    ["text", "Plain text"],
                    ["markdown", "Markdown"],
                    ["json", "JSON"],
                  ] as const
                ).map(([fmt, label]) => (
                  <button
                    key={fmt}
                    onClick={(e) => {
                      e.stopPropagation();
                      onExportZone(fmt);
                      setExportOpen(false);
                    }}
                    onMouseDown={(e) => e.stopPropagation()}
                    className="w-full text-left px-3 py-1.5 text-xs text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
                  >
                    {label}
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    </>
  );
}
