import { useState, useRef, useEffect } from "react";
import { ChevronDown, GitPullRequest, Pin, Filter, ArrowDownToLine, Zap } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useUIElement } from "@qontinui/ui-bridge";
import type { TerminalTab } from "../useTerminalManager";
import type { ZoneAssignments, SessionState } from "../useZoneLayout";
import { STATE_BORDER_COLORS } from "./constants";
import { isNonDurablePty, NON_DURABLE_TOOLTIP, NON_DURABLE_LABEL } from "../sessionDurability";
import { useTerminalWindowActions } from "../useTerminalWindowActions";
import { useSessionPrs, summarizePrs, prGithubUrl, prLabel } from "../useSessionPrs";

/** Tokyo-Night verdict colors shared by the summary symbol and per-PR dots
 * (same green/red the state dots use — `constants.ts`). */
const PR_MERGED_COLOR = "#9ece6a";
const PR_UNMERGED_COLOR = "#f7768e";

/**
 * Per-session PR status dropdown for the zone header: the trigger shows a
 * pull-request symbol that is green when EVERY PR the session authored has
 * merged and red otherwise; the panel lists each PR (unmerged first) with
 * its own red/green mark. Data comes from coord via `useSessionPrs`
 * (fail-soft: unpaired/unreachable/pre-endpoint coord ⇒ renders nothing).
 * Hidden for tabs with no Claude session id and sessions with no PRs.
 */
export function SessionPrDropdown({ claudeSessionId }: { claudeSessionId?: string }) {
  const [open, setOpen] = useState(false);
  const panelRef = useRef<HTMLDivElement>(null);
  const { available, prs, allMerged, refresh } = useSessionPrs(claudeSessionId);

  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (panelRef.current && !panelRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  if (!claudeSessionId || !available || prs.length === 0) return null;

  const { total, unmerged } = summarizePrs(prs);
  const summaryColor = allMerged ? PR_MERGED_COLOR : PR_UNMERGED_COLOR;
  const summaryText = allMerged ? `all ${total} merged` : `${unmerged} of ${total} unmerged`;

  return (
    <div className="relative shrink-0" ref={panelRef}>
      <button
        onClick={(e) => {
          e.stopPropagation();
          if (!open) refresh();
          setOpen(!open);
        }}
        className="flex items-center gap-0.5 p-0.5 rounded hover:bg-[#2a2d3d] transition-colors"
        style={{ color: summaryColor }}
        title={`Session PRs: ${summaryText}`}
        aria-label={`Session PRs: ${summaryText}`}
        data-pr-summary={allMerged ? "all-merged" : "unmerged"}
      >
        <GitPullRequest className="w-2.5 h-2.5" />
        <span className="text-[9px] font-mono leading-none">{allMerged ? total : unmerged}</span>
        <ChevronDown className="w-2 h-2" />
      </button>

      {open && (
        <div className="absolute left-0 top-full mt-0.5 w-56 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
          <div className="flex items-center gap-1.5 px-2 py-1 border-b border-[#2a2d3d]">
            <div
              className="w-2 h-2 rounded-full shrink-0"
              style={{ backgroundColor: summaryColor }}
            />
            <span className="text-[9px] text-[#a9b1d6]">{summaryText}</span>
          </div>
          <div className="max-h-48 overflow-y-auto scrollbar-dark">
            {prs.map((pr) => {
              const url = prGithubUrl(pr);
              return (
                <button
                  key={`${pr.repo}#${pr.pr_number}`}
                  onClick={(e) => {
                    e.stopPropagation();
                    if (url) {
                      void openUrl(url).catch((err) =>
                        console.error("Failed to open PR url:", err),
                      );
                      setOpen(false);
                    }
                  }}
                  className={`w-full flex items-center gap-1.5 px-2 py-1 text-left transition-colors text-[#a9b1d6] ${
                    url ? "hover:bg-[#2a2d3d]/50 cursor-pointer" : "cursor-default"
                  }`}
                  title={url ?? `${pr.repo}#${pr.pr_number} (no GitHub link — bare repo name)`}
                >
                  <div
                    className="w-1.5 h-1.5 rounded-full shrink-0"
                    style={{
                      backgroundColor: pr.merged ? PR_MERGED_COLOR : PR_UNMERGED_COLOR,
                    }}
                  />
                  <span className="text-[10px] font-mono truncate flex-1">{prLabel(pr)}</span>
                  <span
                    className="text-[8px] shrink-0"
                    style={{ color: pr.merged ? PR_MERGED_COLOR : PR_UNMERGED_COLOR }}
                  >
                    {pr.merged ? "merged" : (pr.pr_state ?? "open")}
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * UI Bridge registration spec for a zone header (boot-restore remediation
 * item 8): session/zone identity was previously only observable through a
 * debug DOM text node — `ai/find "<session title>"` resolved nothing. The
 * header element registers with the session title as its label and carries
 * the binding as data attributes (`data-claude-session-id`,
 * `data-zone-index`, `data-resume-failed`) so UI Bridge clients can verify
 * session↔zone placement without scraping internals.
 *
 * Pure + exported for unit tests (the hook call itself needs a DOM).
 */
export function zoneHeaderElementSpec(
  zoneIndex: number,
  title: string,
): { id: string; label: string } {
  return {
    id: `terminal-zone-header-${zoneIndex}`,
    label: `Zone ${zoneIndex + 1}: ${title}`,
  };
}

export function ZoneLabel({
  tab,
  state,
  zoneIndex,
  allTabs,
  assignments,
  sessionStates,
  onAssignTab,
  isPinned,
  onTogglePin,
  zoneLabel,
  onSetZoneLabel,
  onScrollToBottom,
  outputLineCount,
  outputByteSize,
  onToggleFilter,
  filterActive,
}: {
  tab: TerminalTab;
  state: SessionState;
  zoneIndex: number;
  allTabs: TerminalTab[];
  assignments: ZoneAssignments;
  sessionStates: Record<string, SessionState>;
  onAssignTab?: (zoneIndex: number, tabId: string) => void;
  isPinned?: boolean;
  onTogglePin?: () => void;
  zoneLabel?: string;
  onSetZoneLabel?: (label: string) => void;
  onScrollToBottom?: () => void;
  outputLineCount?: number;
  outputByteSize?: number;
  onToggleFilter?: () => void;
  filterActive?: boolean;
}) {
  const [showSelector, setShowSelector] = useState(false);
  const [editingLabel, setEditingLabel] = useState(false);
  const [labelValue, setLabelValue] = useState(zoneLabel ?? "");
  const selectorRef = useRef<HTMLDivElement>(null);
  const { popOutTab } = useTerminalWindowActions();

  // Item 8: expose session/zone identity to UI Bridge clients. The label is
  // the session title so `ai/find` by title resolves to this header; the
  // session binding rides as data attributes on the registered node.
  const headerSpec = zoneHeaderElementSpec(zoneIndex, tab.title);
  const { ref: headerRef } = useUIElement({
    id: headerSpec.id,
    type: "generic",
    label: headerSpec.label,
  });

  useEffect(() => {
    if (!showSelector) return;
    const handleClick = (e: MouseEvent) => {
      if (selectorRef.current && !selectorRef.current.contains(e.target as Node)) {
        setShowSelector(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [showSelector]);

  return (
    <div
      ref={headerRef}
      data-claude-session-id={tab.claudeSessionId ?? undefined}
      data-zone-index={zoneIndex}
      data-resume-failed={tab.resumeFailed ? "true" : undefined}
      className="absolute top-0 left-0 right-0 flex items-center gap-1.5 px-2 py-0.5 bg-[#13141f]/80 backdrop-blur-sm z-10 cursor-grab active:cursor-grabbing"
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData("text/tab-id", tab.id);
        e.dataTransfer.effectAllowed = "move";
      }}
      onDragEnd={(e) => {
        // Drag-tab-out: if the header was dropped OUTSIDE this window's
        // viewport and no in-window zone accepted it (dropEffect "none"),
        // pop the terminal into its own new window. HTML5 DnD can't cross
        // OS windows, so a release over another runner window also lands
        // here as "none" with an out-of-bounds pointer — treated the same
        // (new window). A drop onto a zone sets dropEffect "move" → skip.
        if (e.dataTransfer.dropEffect !== "none") return;
        const out =
          e.clientX <= 0 ||
          e.clientY <= 0 ||
          e.clientX >= window.innerWidth ||
          e.clientY >= window.innerHeight;
        if (!out) return;
        void popOutTab(tab.id).catch((err) =>
          console.error("Failed to pop out terminal (drag-out):", err),
        );
      }}
    >
      <div
        className={`w-1.5 h-1.5 rounded-full shrink-0 ${
          state === "needs-input" ? "animate-pulse" : ""
        }`}
        style={{ backgroundColor: STATE_BORDER_COLORS[state] }}
      />
      <span className="text-[10px] text-[#a9b1d6] truncate font-medium">{tab.title}</span>

      {/* Honesty label (Phase 5): interactive PTY shells are non-durable — a
          runner restart ends them. Mirrors the compact-card marker so the
          operator sees it in the full-terminal view too, not only in the
          compact multi-zone overview. */}
      {isNonDurablePty(tab) && (
        <span
          className="flex items-center gap-0.5 shrink-0 text-[8px] text-[#565f89] bg-[#565f89]/15 px-1 py-0 rounded"
          title={NON_DURABLE_TOOLTIP}
          aria-label={`Ephemeral session: ${NON_DURABLE_TOOLTIP}`}
        >
          <Zap className="w-2.5 h-2.5" />
          {NON_DURABLE_LABEL}
        </span>
      )}

      {/* Per-session PR status: green = every authored PR merged, red = at
          least one unmerged; the panel lists PR numbers unmerged-first so a
          stuck/red-CI PR is visible without asking the session. */}
      <SessionPrDropdown claudeSessionId={tab.claudeSessionId} />

      {outputLineCount !== undefined && outputLineCount > 0 && (
        <span className="text-[9px] text-[#565f89] font-mono ml-1 shrink-0">
          {outputLineCount > 999 ? `${(outputLineCount / 1000).toFixed(1)}k` : outputLineCount}{" "}
          lines
          {outputByteSize !== undefined && outputByteSize > 1024 && (
            <span className="ml-1">
              (
              {outputByteSize > 1048576
                ? `${(outputByteSize / 1048576).toFixed(1)}MB`
                : `${(outputByteSize / 1024).toFixed(0)}KB`}
              )
            </span>
          )}
        </span>
      )}

      {onSetZoneLabel &&
        (editingLabel ? (
          <input
            autoFocus
            value={labelValue}
            onChange={(e) => setLabelValue(e.target.value)}
            onBlur={() => {
              setEditingLabel(false);
              onSetZoneLabel(labelValue.trim());
            }}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") {
                setEditingLabel(false);
                onSetZoneLabel(labelValue.trim());
              }
              if (e.key === "Escape") {
                setEditingLabel(false);
                setLabelValue(zoneLabel ?? "");
              }
            }}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
            className="bg-transparent border-b border-[#565f89] text-[9px] text-[#bb9af7] outline-hidden w-16 shrink-0"
            placeholder="Label..."
          />
        ) : zoneLabel ? (
          <span
            className="text-[9px] text-[#bb9af7] bg-[#bb9af7]/10 px-1 py-0 rounded cursor-pointer hover:bg-[#bb9af7]/20 transition-colors shrink-0"
            onDoubleClick={(e) => {
              e.stopPropagation();
              setLabelValue(zoneLabel);
              setEditingLabel(true);
            }}
            title="Double-click to edit"
          >
            {zoneLabel}
          </span>
        ) : (
          <span
            className="text-[9px] text-[#565f89]/30 cursor-pointer hover:text-[#565f89] transition-colors shrink-0"
            onDoubleClick={(e) => {
              e.stopPropagation();
              setLabelValue("");
              setEditingLabel(true);
            }}
            title="Double-click to add label"
          >
            +
          </span>
        ))}

      {onAssignTab && allTabs.length > 1 && (
        <div className="relative shrink-0" ref={selectorRef}>
          <button
            onClick={(e) => {
              e.stopPropagation();
              setShowSelector(!showSelector);
            }}
            className="p-0.5 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors"
            title="Switch session in this zone"
          >
            <ChevronDown className="w-2.5 h-2.5" />
          </button>

          {showSelector && (
            <div className="absolute left-0 top-full mt-0.5 w-48 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
              <div className="max-h-48 overflow-y-auto scrollbar-dark">
                {allTabs.map((t) => {
                  const isCurrent = t.id === tab.id;
                  const assignedZone = Object.entries(assignments).find(([, id]) => id === t.id);
                  const tabState = sessionStates[t.id] ?? "idle";

                  return (
                    <button
                      key={t.id}
                      onClick={(e) => {
                        e.stopPropagation();
                        onAssignTab(zoneIndex, t.id);
                        setShowSelector(false);
                      }}
                      className={`w-full flex items-center gap-1.5 px-2 py-1 text-left transition-colors ${
                        isCurrent
                          ? "bg-[#7aa2f7]/10 text-[#7aa2f7]"
                          : "text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                      }`}
                    >
                      <div
                        className={`w-1.5 h-1.5 rounded-full shrink-0 ${tabState === "needs-input" ? "animate-pulse" : ""}`}
                        style={{ backgroundColor: STATE_BORDER_COLORS[tabState] }}
                      />
                      <span className="text-[10px] truncate flex-1">{t.title}</span>
                      {assignedZone && !isCurrent && (
                        <span className="text-[8px] text-[#565f89]">
                          Z{Number(assignedZone[0]) + 1}
                        </span>
                      )}
                      {!assignedZone && (
                        <span className="text-[8px] text-[#565f89] italic">hidden</span>
                      )}
                    </button>
                  );
                })}
              </div>
            </div>
          )}
        </div>
      )}

      {onTogglePin && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onTogglePin();
          }}
          className={`p-0.5 rounded transition-colors shrink-0 ${
            isPinned
              ? "text-[#7aa2f7] bg-[#7aa2f7]/15"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]"
          }`}
          title={
            isPinned
              ? "Unpin zone (show compact in auto mode)"
              : "Pin zone (keep full terminal in auto mode)"
          }
        >
          <Pin className="w-2.5 h-2.5" />
        </button>
      )}

      {onToggleFilter && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onToggleFilter();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          className={`p-0.5 rounded transition-colors shrink-0 ${
            filterActive
              ? "text-[#e0af68] bg-[#e0af68]/10"
              : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
          }`}
          title="Filter output"
        >
          <Filter className="w-2.5 h-2.5" />
        </button>
      )}

      {onScrollToBottom && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onScrollToBottom();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          className="p-0.5 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors shrink-0"
          title="Scroll to bottom"
        >
          <ArrowDownToLine className="w-2.5 h-2.5" />
        </button>
      )}

      {tab.workingDir && (
        <span className="text-[9px] text-[#565f89] truncate ml-auto pointer-events-none">
          {tab.workingDir.split(/[/\\]/).pop()}
        </span>
      )}
    </div>
  );
}
