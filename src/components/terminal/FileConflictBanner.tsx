import { AlertTriangle, X, FileWarning, ChevronDown, ChevronUp } from "lucide-react";
import { useState } from "react";
import type { FileConflict } from "./useFileConflicts";

interface FileConflictBannerProps {
  /** Files with active conflicts (multiple sessions) */
  conflicts: FileConflict[];
  /** Recent alert from real-time conflict detection */
  recentAlert: { file_path: string; holder_name: string } | null;
  /** Dismiss the recent alert */
  onDismissAlert: () => void;
}

/**
 * Banner component that shows file conflict warnings in the Terminal page.
 *
 * Displays two types of warnings:
 * 1. Toast-style alert when a conflict is detected in real-time
 * 2. Persistent summary of all files with multiple active editors
 */
export function FileConflictBanner({
  conflicts,
  recentAlert,
  onDismissAlert,
}: FileConflictBannerProps) {
  const [expanded, setExpanded] = useState(false);

  if (conflicts.length === 0 && !recentAlert) return null;

  return (
    <div className="shrink-0">
      {/* Real-time conflict alert (toast-style) */}
      {recentAlert && (
        <div className="flex items-center gap-2 px-3 py-1.5 text-xs font-medium bg-[#e0af68]/15 text-[#e0af68] border-b border-[#e0af68]/20 animate-in slide-in-from-top-1">
          <AlertTriangle className="w-3.5 h-3.5 shrink-0" />
          <span className="flex-1 truncate">
            Conflict: <strong>{recentAlert.file_path}</strong> is also being edited by{" "}
            {recentAlert.holder_name}
          </span>
          <button
            onClick={onDismissAlert}
            className="p-0.5 rounded hover:bg-white/10 transition-colors"
          >
            <X className="w-3 h-3" />
          </button>
        </div>
      )}

      {/* Persistent conflict summary */}
      {conflicts.length > 0 && (
        <div className="border-b border-[#e0af68]/20">
          <button
            onClick={() => setExpanded(!expanded)}
            className="flex items-center gap-2 px-3 py-1 text-xs text-[#e0af68]/80 hover:text-[#e0af68] w-full transition-colors"
          >
            <FileWarning className="w-3 h-3 shrink-0" />
            <span>
              {conflicts.length} file{conflicts.length !== 1 ? "s" : ""} under concurrent
              development
            </span>
            {expanded ? (
              <ChevronUp className="w-3 h-3 ml-auto" />
            ) : (
              <ChevronDown className="w-3 h-3 ml-auto" />
            )}
          </button>

          {expanded && (
            <div className="px-3 pb-2 text-xs text-[#a9b1d6]/70 space-y-1">
              {conflicts.map((c) => (
                <div key={c.file_path} className="flex items-start gap-1.5">
                  <span className="text-[#e0af68]/60 mt-px">-</span>
                  <div>
                    <span className="text-[#a9b1d6]/90 font-mono">{c.file_path}</span>
                    <span className="text-[#a9b1d6]/50 ml-1.5">
                      ({c.other_holders.map((h) => h.holder_name).join(", ")})
                    </span>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
