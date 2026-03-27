/**
 * PromptVersionHistory — Lists all versions of a prompt template with selection,
 * comparison, and navigation to detail/diff views.
 */

import { useState, useMemo } from "react";
import { History, GitCompare, ChevronRight, Loader2 } from "lucide-react";
import { usePromptVersions } from "@/hooks/usePromptVersions";
import { PromptVersionDetail } from "./PromptVersionDetail";
import { PromptVersionDiff } from "./PromptVersionDiff";

interface PromptVersionHistoryProps {
  templateId: string;
}

type View = "list" | "detail" | "diff";

export function PromptVersionHistory({ templateId }: PromptVersionHistoryProps) {
  const { data: versions, isLoading, error, refetch } = usePromptVersions(templateId);

  const [view, setView] = useState<View>("list");
  const [selectedVersion, setSelectedVersion] = useState<number | null>(null);
  const [compareA, setCompareA] = useState<number | null>(null);
  const [compareB, setCompareB] = useState<number | null>(null);
  const [compareMode, setCompareMode] = useState(false);

  const sorted = useMemo(
    () => [...(versions ?? [])].sort((a, b) => b.version_number - a.version_number),
    [versions],
  );

  // --- handlers ---

  const handleVersionClick = (vn: number) => {
    if (compareMode) {
      if (compareA == null) {
        setCompareA(vn);
      } else if (compareB == null && vn !== compareA) {
        setCompareB(vn);
      } else {
        // reset and start over
        setCompareA(vn);
        setCompareB(null);
      }
    } else {
      setSelectedVersion(vn);
      setView("detail");
    }
  };

  const handleStartCompare = () => {
    setCompareMode(true);
    setCompareA(null);
    setCompareB(null);
  };

  const handleCancelCompare = () => {
    setCompareMode(false);
    setCompareA(null);
    setCompareB(null);
  };

  const handleViewDiff = () => {
    if (compareA != null && compareB != null) {
      setView("diff");
      setCompareMode(false);
    }
  };

  const handleBack = () => {
    setView("list");
    setSelectedVersion(null);
    setCompareA(null);
    setCompareB(null);
  };

  // --- detail / diff sub-views ---

  if (view === "detail" && selectedVersion != null) {
    return (
      <PromptVersionDetail
        templateId={templateId}
        versionNumber={selectedVersion}
        onBack={handleBack}
        onRestored={() => {
          refetch();
          handleBack();
        }}
      />
    );
  }

  if (view === "diff" && compareA != null && compareB != null) {
    return (
      <PromptVersionDiff
        templateId={templateId}
        versionA={compareA}
        versionB={compareB}
        onBack={handleBack}
      />
    );
  }

  // --- list view ---

  return (
    <div className="p-4 space-y-4" data-tutorial-id="prompt-version-history">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <History className="w-4 h-4 text-primary" />
          <h3 className="text-sm font-medium text-foreground">Version History</h3>
          <span className="text-xs text-muted-foreground">{sorted.length} versions</span>
        </div>

        <div className="flex items-center gap-2">
          {compareMode ? (
            <>
              <span className="text-xs text-muted-foreground">
                {compareA == null
                  ? "Select first version"
                  : compareB == null
                    ? "Select second version"
                    : "Ready to compare"}
              </span>
              {compareA != null && compareB != null && (
                <button
                  onClick={handleViewDiff}
                  className="px-3 py-1 text-xs bg-blue-600 text-white rounded hover:bg-blue-500"
                >
                  View Diff
                </button>
              )}
              <button
                onClick={handleCancelCompare}
                className="px-3 py-1 text-xs bg-muted/60 text-muted-foreground rounded hover:bg-muted"
              >
                Cancel
              </button>
            </>
          ) : (
            <>
              <button
                onClick={handleStartCompare}
                disabled={sorted.length < 2}
                className="flex items-center gap-1 px-3 py-1 text-xs bg-muted/60 text-muted-foreground rounded hover:bg-muted disabled:opacity-40 disabled:cursor-not-allowed"
                data-tutorial-id="prompt-version-compare"
              >
                <GitCompare className="w-3.5 h-3.5" />
                Compare
              </button>
              <button
                onClick={() => refetch()}
                className="text-xs text-muted-foreground hover:text-foreground px-2 py-1"
              >
                Refresh
              </button>
            </>
          )}
        </div>
      </div>

      {/* Content */}
      {isLoading ? (
        <div className="flex items-center justify-center h-40">
          <Loader2 className="w-5 h-5 animate-spin text-muted-foreground" />
        </div>
      ) : error ? (
        <div className="text-red-400 text-sm py-4 text-center">
          Failed to load version history.{" "}
          <button onClick={() => refetch()} className="underline hover:text-red-300">
            Retry
          </button>
        </div>
      ) : sorted.length === 0 ? (
        <div className="text-muted-foreground text-sm py-8 text-center">
          No versions recorded for this template yet.
        </div>
      ) : (
        <div className="space-y-1">
          {sorted.map((v) => {
            const isSelectedA = compareA === v.version_number;
            const isSelectedB = compareB === v.version_number;
            const isCompareSelected = isSelectedA || isSelectedB;

            return (
              <div
                key={v.version_number}
                role="button"
                tabIndex={0}
                onClick={() => handleVersionClick(v.version_number)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    handleVersionClick(v.version_number);
                  }
                }}
                className={`flex items-center gap-3 px-4 py-2.5 rounded-lg cursor-pointer transition-colors ${
                  isCompareSelected
                    ? "bg-blue-900/30 border border-blue-700"
                    : "bg-card border border-border hover:bg-muted"
                }`}
              >
                {/* Version badge */}
                <span className="text-xs font-mono px-2 py-0.5 rounded bg-muted text-muted-foreground min-w-[3rem] text-center">
                  v{v.version_number}
                </span>

                {/* Compare indicator */}
                {compareMode && isCompareSelected && (
                  <span className="text-xs px-1.5 py-0.5 rounded bg-blue-800 text-blue-300">
                    {isSelectedA ? "A" : "B"}
                  </span>
                )}

                {/* Description & hash */}
                <div className="flex-1 min-w-0">
                  <div className="text-sm text-foreground truncate">
                    {v.change_description || "No description"}
                  </div>
                  <div className="text-xs text-muted-foreground font-mono">
                    {v.content_hash?.substring(0, 12)}
                  </div>
                </div>

                {/* Date */}
                <span className="text-xs text-muted-foreground whitespace-nowrap">
                  {new Date(v.created_at).toLocaleDateString()}
                </span>

                {!compareMode && (
                  <ChevronRight className="w-4 h-4 text-muted-foreground shrink-0" />
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

export default PromptVersionHistory;
