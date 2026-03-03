import { PanelLeft, FileText, RefreshCw } from "lucide-react";
import type { AnalysisType } from "./TerminalAnalysisPanel";

interface TerminalActionBarProps {
  showSidebar: boolean;
  onToggleSidebar: () => void;
  isGenerating: boolean;
  isAnalyzing: boolean;
  onAnalyze: (type: AnalysisType) => void;
  /** Filename of the loaded plan, or null if none detected. */
  planFileName: string | null;
  /** True while the plan is being loaded from disk. */
  isPlanLoading: boolean;
  /** Reload the plan file from disk. */
  onRefreshPlan: () => void;
}

const ANALYSIS_BUTTONS: { type: AnalysisType; label: string; title: string }[] = [
  { type: "session-summary", label: "Summarize", title: "Summarize active terminal session" },
  {
    type: "architecture",
    label: "Architecture",
    title: "Generate architecture diagram from plan or selection",
  },
  {
    type: "change-impact",
    label: "Change Impact",
    title: "Analyze git diff / file changes (select in terminal)",
  },
  { type: "progress", label: "Plan Progress", title: "Assess progress against current plan" },
  { type: "cross-tab", label: "All Sessions", title: "Summarize all open terminal sessions" },
];

export function TerminalActionBar({
  showSidebar,
  onToggleSidebar,
  isGenerating,
  isAnalyzing,
  onAnalyze,
  planFileName,
  isPlanLoading,
  onRefreshPlan,
}: TerminalActionBarProps) {
  const busy = isAnalyzing || isGenerating;

  return (
    <div className="bg-[#13141f] border-b border-[#2a2d3d] shrink-0">
      {/* Row 1: Session browser + generation status */}
      <div className="flex items-center gap-2 px-3 h-8">
        <button
          onClick={onToggleSidebar}
          className={`
            flex items-center gap-1.5 px-2 py-0.5 rounded text-xs font-medium transition-colors
            ${
              showSidebar
                ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
                : "text-[#9ece6a] hover:bg-[#9ece6a]/10 hover:text-[#a6da7a]"
            }
          `}
          title={showSidebar ? "Hide session browser" : "Browse Claude Code sessions"}
        >
          {showSidebar ? <PanelLeft className="w-3 h-3" /> : <FileText className="w-3 h-3" />}
          {showSidebar ? "Hide Sessions" : "Browse Sessions"}
        </button>

        {isGenerating && (
          <>
            <div className="w-px h-4 bg-[#2a2d3d]" />
            <div className="flex items-center gap-1.5 text-xs text-[#e0af68]">
              <div className="w-3 h-3 border-2 border-[#e0af68] border-t-transparent rounded-full animate-spin" />
              Generating workflow...
            </div>
          </>
        )}

        {/* Plan file indicator (right-aligned) */}
        <div className="ml-auto flex items-center gap-1.5">
          {isPlanLoading && (
            <div className="w-3 h-3 border border-[#565f89] border-t-transparent rounded-full animate-spin" />
          )}
          {planFileName && !isPlanLoading && (
            <span
              className="text-[10px] text-[#9ece6a] bg-[#9ece6a]/10 px-2 py-0.5 rounded-full font-mono truncate max-w-[180px]"
              title={`Plan loaded: ${planFileName} — used by Architecture and Plan Progress analyses`}
            >
              {planFileName}
            </span>
          )}
          {!planFileName && !isPlanLoading && (
            <span
              className="text-[10px] text-[#414868]"
              title="No PLAN*.md / TODO*.md file found in workspace"
            >
              no plan
            </span>
          )}
          <button
            onClick={onRefreshPlan}
            disabled={isPlanLoading}
            title="Reload plan file from disk"
            className="p-0.5 rounded text-[#414868] hover:text-[#7dcfff] transition-colors disabled:opacity-40"
          >
            <RefreshCw className="w-3 h-3" />
          </button>
        </div>
      </div>

      {/* Row 2: Analysis buttons */}
      <div className="flex items-center gap-1.5 px-3 h-7 border-t border-[#2a2d3d]/60">
        <span className="text-[10px] text-[#414868] font-medium uppercase tracking-wider mr-0.5 shrink-0">
          Analyze:
        </span>
        {ANALYSIS_BUTTONS.map(({ type, label, title }) => (
          <button
            key={type}
            onClick={() => onAnalyze(type)}
            disabled={busy}
            title={
              (type === "architecture" || type === "progress") && planFileName
                ? `${title} (using ${planFileName})`
                : title
            }
            className={`
              px-2 py-0.5 rounded text-[10px] font-medium transition-colors shrink-0
              ${busy ? "text-[#414868] cursor-not-allowed" : "text-[#7dcfff] hover:bg-[#7dcfff]/10 hover:text-[#9de8ff]"}
            `}
          >
            {isAnalyzing ? (
              <span className="flex items-center gap-1">
                <span className="w-2 h-2 border border-[#7dcfff] border-t-transparent rounded-full animate-spin inline-block" />
                {label}
              </span>
            ) : (
              label
            )}
          </button>
        ))}
      </div>
    </div>
  );
}
