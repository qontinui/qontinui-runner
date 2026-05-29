import { useMemo, useState, useRef, useEffect } from "react";
import {
  ArrowRight,
  ArrowUpDown,
  BarChart3,
  ChevronDown,
  Clock,
  Download,
  Eye,
  FileText,
  Focus,
  Keyboard,
  ListChecks,
  Rocket,
  RefreshCw,
  Save,
  Tag,
  Volume2,
  VolumeOff,
  Wand2,
} from "lucide-react";
import type { AnalysisType } from "./TerminalAnalysisPanel";
import { DocFinderModal } from "./DocFinderModal";
import type { SessionState, ZoneAssignments } from "./useZoneLayout";
import type { TerminalTab } from "./useTerminalManager";
import { instanceStorage } from "@/lib/instance-storage";
import {
  useTerminalSession,
  useZoneMetadata,
  useTransitionEffects,
  useUIStateCx,
} from "./contexts";

const ANALYSIS_BUTTONS: { type: AnalysisType; label: string; title: string }[] = [
  { type: "session-summary", label: "Session Summary", title: "Summarize active terminal session" },
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
  {
    type: "page-architecture",
    label: "Page Map",
    title: "Architecture diagram of Terminal page components",
  },
];

interface ZoneStatusBarProps {
  /** Callbacks from useZoneActions — kept as props until that hook moves to context */
  onExport?: () => void;
  onSortZones?: () => void;
  onOpenDocFile?: (filePath: string) => void;
}

const STATE_COLORS: Record<SessionState, string> = {
  idle: "#565f89",
  working: "#7aa2f7",
  "needs-input": "#e0af68",
  completed: "#9ece6a",
  error: "#f7768e",
};

const STATE_LABELS: Record<SessionState, string> = {
  idle: "idle",
  working: "working",
  "needs-input": "waiting",
  completed: "done",
  error: "error",
};

export function ZoneStatusBar({ onExport, onSortZones, onOpenDocFile }: ZoneStatusBarProps) {
  // Read all data from contexts instead of props
  const session = useTerminalSession();
  const { tabs, zoneLayout } = session;
  const assignments = zoneLayout.assignments;
  const {
    sessionStates,
    stateDurations,
    lastOutputLines,
    stateTimeAccum: stateTimeAccumRef,
  } = session;
  const { labelsAndTags, eventHistory, metrics: metricsRef } = useZoneMetadata();
  const zoneLabels = labelsAndTags.zoneLabels;
  const labelColorMap = labelsAndTags.labelColorMap;
  const externalActiveTagFilters = labelsAndTags.activeTagFilters;
  const externalSetActiveTagFilters = labelsAndTags.setActiveTagFilters;
  const externalAllTags = labelsAndTags.allTags;
  // eslint-disable-next-line react-hooks/refs -- intentional: metrics ref holds a stable mutable counter object
  const metrics = metricsRef.current;
  // eslint-disable-next-line react-hooks/refs -- intentional: stateTimeAccum ref holds a stable mutable accumulator
  const stateTimeAccum = stateTimeAccumRef.current;
  const transitionEffects = useTransitionEffects();
  const autoFocus = transitionEffects.autoFocusNeedsInput;
  const onToggleAutoFocus = transitionEffects.toggleAutoFocus;
  const soundEnabled = transitionEffects.soundEnabled;
  const onToggleSound = transitionEffects.toggleSound;
  const autoApproveCount = transitionEffects.autoApproveCount;
  const autoRestart = transitionEffects.autoRestart;
  const autoRestartCount = transitionEffects.autoRestartCount;
  const { workflowGen, analysis, sessionManager } = session;
  const isGenerating = workflowGen.isGenerating;
  const isAnalyzing = analysis.isAnalyzing;
  const onAnalyze = analysis.handleAnalyze;
  const onGenerateFromSession = workflowGen.handleGenerateFromLatestSession;
  const onSaveGeneratedWorkflow = workflowGen.handleSaveWorkflow;
  const generatedWorkflow = workflowGen.generatedWorkflow;
  const planFileName = workflowGen.planFileName;
  const isPlanLoading = workflowGen.isPlanLoading;
  const onBuildPlanFromFile = workflowGen.handleBuildPlanFromFile;
  const onBuildPlanImplementationFromFile = workflowGen.handleBuildPlanImplementationFromFile;
  // Phase 9d note: `findingsCount` and `frozenSessionCount` are no
  // longer surfaced as ZSB badges. Phase 9f will resurrect them as
  // StatusStrip pills (e.g. `N findings`, `N frozen sessions`).
  void sessionManager;
  const { state: uiState, dispatch, toggleFocusMode } = useUIStateCx();
  const focusMode = uiState.focusMode;
  const onToggleFocusMode = toggleFocusMode;
  const onJumpToNeedsInput = () => zoneLayout.focusNextNeedsInput(sessionStates);
  const onShowShortcuts = () => dispatch({ type: "SET_SHOW_SHORTCUTS", payload: true });
  const onSelectByState = (state: SessionState) => {
    const zones = new Set<number>();
    for (const [zoneStr, tabId] of Object.entries(assignments)) {
      if ((sessionStates[tabId] ?? "idle") === state) {
        zones.add(Number(zoneStr));
      }
    }
    dispatch({ type: "SET_SELECTED_ZONES", payload: zones });
  };
  const onToggleAutoRestart = () => {
    transitionEffects.setAutoRestart((prev: boolean) => {
      const next = !prev;
      instanceStorage.setItem("zone-auto-restart", String(next));
      return next;
    });
  };
  const [showDocFinder, setShowDocFinder] = useState(false);
  const stateCounts = useMemo(() => {
    const counts: Record<SessionState, number> = {
      idle: 0,
      working: 0,
      "needs-input": 0,
      completed: 0,
      error: 0,
    };
    for (const tab of tabs) {
      const state = sessionStates[tab.id] ?? "idle";
      counts[state]++;
    }
    return counts;
  }, [tabs, sessionStates]);

  const needsInputCount = stateCounts["needs-input"];
  const hasActionNeeded = needsInputCount > 0 || stateCounts.error > 0;

  // Parse tags from zone labels (comma-separated)
  const zoneTags = useMemo(() => {
    const map: Record<number, string[]> = {};
    if (!zoneLabels) return map;
    for (const [zStr, label] of Object.entries(zoneLabels)) {
      if (!label) continue;
      map[Number(zStr)] = label
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean);
    }
    return map;
  }, [zoneLabels]);

  // All unique tags across all zones (use external if provided, otherwise compute locally)
  const localAllTags = useMemo(() => {
    const tagSet = new Set<string>();
    for (const tags of Object.values(zoneTags)) {
      for (const t of tags) tagSet.add(t);
    }
    return [...tagSet].sort();
  }, [zoneTags]);
  const allTags = externalAllTags ?? localAllTags;

  // Active tag filters (OR logic — show zones matching ANY active tag)
  // Use external state if provided, otherwise keep local
  const [localActiveTagFilters, setLocalActiveTagFilters] = useState<Set<string>>(new Set());
  const activeTagFilters = externalActiveTagFilters ?? localActiveTagFilters;
  const setActiveTagFilters = externalSetActiveTagFilters ?? setLocalActiveTagFilters;

  const toggleTagFilter = (tag: string) => {
    setActiveTagFilters(
      (() => {
        const next = new Set(activeTagFilters);
        if (next.has(tag)) next.delete(tag);
        else next.add(tag);
        return next;
      })(),
    );
  };

  const isMultiZone = tabs.length > 1;
  const busy = isAnalyzing || isGenerating;

  return (
    <div className="flex items-center gap-2 px-3 h-8 bg-[#13141f] border-b border-[#2a2d3d] shrink-0">
      {/* ── Left group (always visible) ─────────────────────────────── */}

      {/* Transcript section label */}
      <h5 className="text-[10px] font-medium uppercase tracking-wider text-[#565f89] shrink-0 m-0">
        Transcript
      </h5>

      {/* Phase 9d — Sessions toggle, Resume, and Doc-finder buttons
          replaced by /sessions, /resume, and (TBD: /doc-finder) registry
          actions. The Sessions toggle's `useUIElement reveals`
          declaration is lost in the migration; external agents that
          previously walked `/control/elements?revealsAny=session-card-*`
          to find the toggle now invoke the action directly via UI Bridge
          components. */}

      {onOpenDocFile && (
        <button
          onClick={() => setShowDocFinder(true)}
          className="flex items-center gap-1.5 px-2 py-0.5 rounded text-xs font-medium transition-colors text-[#7dcfff] hover:bg-[#7dcfff]/10 hover:text-[#89d4ff] shrink-0"
          title="Open a document file in a zone"
        >
          <FileText className="w-3 h-3" />
          Doc
        </button>
      )}

      <div className="w-px h-4 bg-[#2a2d3d]" />

      {/* Generate workflow button */}
      <button
        onClick={onGenerateFromSession}
        disabled={busy}
        title="Generate workflow from latest Claude Code session"
        className={`
          flex items-center gap-1.5 px-2 py-0.5 rounded text-xs font-medium transition-colors shrink-0
          ${busy ? "text-[#414868] cursor-not-allowed" : "text-[#bb9af7] hover:bg-[#bb9af7]/10 hover:text-[#c8abff]"}
        `}
      >
        <Wand2 className="w-3 h-3" />
        Generate
      </button>

      {/* Save generated workflow to library (quick action) */}
      <button
        onClick={onSaveGeneratedWorkflow}
        disabled={!generatedWorkflow || busy}
        title={
          generatedWorkflow
            ? "Save generated workflow to library"
            : "Generate a workflow first to enable saving"
        }
        aria-label="Save workflow to library"
        className={`
          flex items-center gap-1.5 px-2 py-0.5 rounded text-xs font-medium transition-colors shrink-0
          ${
            !generatedWorkflow || busy
              ? "text-[#414868] cursor-not-allowed"
              : "text-[#9ece6a] hover:bg-[#9ece6a]/10 hover:text-[#b9f27c]"
          }
        `}
      >
        <Save className="w-3 h-3" />
        Save
      </button>

      {isGenerating && (
        <div className="flex items-center gap-1 text-[10px] text-[#e0af68] shrink-0">
          <div className="w-2.5 h-2.5 border-2 border-[#e0af68] border-t-transparent rounded-full animate-spin" />
          generating…
        </div>
      )}

      {/* Phase 9d — Findings + File-Ownership buttons replaced by
          /findings and /file-ownership registry actions. The count
          badges (findingsCount, frozenSessionCount) lost their visible
          home in this slice; a Phase 9f StatusStrip pill picks them up. */}

      <div className="w-px h-4 bg-[#2a2d3d]" />

      {/* Analyze dropdown */}
      <AnalyzeDropdown
        busy={busy}
        isAnalyzing={isAnalyzing}
        onAnalyze={onAnalyze}
        planFileName={planFileName}
      />

      {/* ── Middle group (multi-zone only) ──────────────────────────── */}
      {isMultiZone && (
        <>
          <div className="w-px h-4 bg-[#2a2d3d]" />

          {/* Session count */}
          <span className="text-[10px] text-[#565f89] shrink-0">
            {tabs.length} session{tabs.length !== 1 ? "s" : ""}
          </span>

          {/* State breakdown — clickable to select zones by state */}
          <div className="flex items-center gap-2 text-[10px] shrink-0">
            {(["needs-input", "working", "completed", "error", "idle"] as SessionState[]).map(
              (state) => {
                const count = stateCounts[state];
                if (count === 0) return null;
                return (
                  <button
                    key={state}
                    className="flex items-center gap-1 rounded px-1 py-0.5 -mx-1 hover:bg-white/5 transition-colors"
                    style={{ color: STATE_COLORS[state] }}
                    onClick={() => onSelectByState?.(state)}
                    title={`Select all ${STATE_LABELS[state]} zones`}
                  >
                    <span
                      className={`w-1.5 h-1.5 rounded-full ${state === "needs-input" ? "animate-pulse" : ""}`}
                      style={{ backgroundColor: STATE_COLORS[state] }}
                    />
                    {count} {STATE_LABELS[state]}
                  </button>
                );
              },
            )}
          </div>

          {/* Tag filter pills */}
          {allTags.length > 0 && (
            <>
              <div className="w-px h-3 bg-[#2a2d3d] shrink-0" />
              <div className="flex items-center gap-1 shrink-0">
                <Tag className="w-3 h-3 text-[#565f89]" />
                {activeTagFilters.size > 0 && (
                  <button
                    onClick={() => setActiveTagFilters(new Set())}
                    className="text-[9px] px-1.5 py-0.5 rounded bg-[#2a2d3d] text-[#a9b1d6] hover:bg-[#3b3d57] transition-colors"
                  >
                    All
                  </button>
                )}
                {allTags.map((tag) => {
                  const isActive = activeTagFilters.has(tag);
                  const color = labelColorMap?.[tag] ?? "#bb9af7";
                  return (
                    <button
                      key={tag}
                      onClick={() => toggleTagFilter(tag)}
                      className={`text-[9px] px-1.5 py-0.5 rounded transition-colors ${
                        isActive ? "ring-1" : "opacity-60 hover:opacity-100"
                      }`}
                      style={{
                        color,
                        backgroundColor: `${color}${isActive ? "25" : "10"}`,
                        ...(isActive ? { boxShadow: `0 0 0 1px ${color}` } : {}),
                      }}
                    >
                      {tag}
                    </button>
                  );
                })}
              </div>
            </>
          )}

          {/* Jump to next action button */}
          {hasActionNeeded && (
            <button
              onClick={onJumpToNeedsInput}
              className="flex items-center gap-1.5 px-2.5 py-1 rounded bg-[#e0af68]/15 text-[#e0af68] text-[10px] font-medium hover:bg-[#e0af68]/25 transition-colors shrink-0"
              title="Jump to next session needing input (Ctrl+Shift+N)"
            >
              <ArrowRight className="w-3 h-3" />
              Next Action
              {needsInputCount > 0 && (
                <span className="px-1 py-0.5 rounded bg-[#e0af68]/20 text-[9px]">
                  {needsInputCount}
                </span>
              )}
            </button>
          )}
        </>
      )}

      {/* ── Right group ─────────────────────────────────────────────── */}
      <div className="ml-auto flex items-center gap-1.5 shrink-0">
        {/* Multi-zone toggles */}
        {isMultiZone && (
          <>
            {/* Auto-focus toggle */}
            {onToggleAutoFocus && (
              <button
                onClick={onToggleAutoFocus}
                className={`flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] transition-colors shrink-0 ${
                  autoFocus
                    ? "bg-[#e0af68]/15 text-[#e0af68]"
                    : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                }`}
                title={`Auto-focus on needs-input: ${autoFocus ? "ON" : "OFF"}`}
              >
                <Focus className="w-3 h-3" />
                {autoFocus && <span>Auto</span>}
              </button>
            )}

            {/* Sound notification toggle */}
            {onToggleSound && (
              <button
                onClick={onToggleSound}
                className={`p-1 rounded transition-colors shrink-0 ${
                  soundEnabled
                    ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
                    : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                }`}
                title={`Sound notification: ${soundEnabled ? "ON" : "OFF"}`}
              >
                {soundEnabled ? (
                  <Volume2 className="w-3.5 h-3.5" />
                ) : (
                  <VolumeOff className="w-3.5 h-3.5" />
                )}
              </button>
            )}

            {/* Desktop notification toggle */}
            {/* Phase 9d — Desktop-notify toggle replaced by
                /desktop-notify registry action. */}

            {/* Focus mode toggle */}
            {onToggleFocusMode && (
              <button
                onClick={onToggleFocusMode}
                className={`p-1 rounded transition-colors shrink-0 ${
                  focusMode
                    ? "text-[#e0af68] bg-[#e0af68]/10"
                    : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                }`}
                title={`Focus mode: ${focusMode ? "ON" : "OFF"} (Ctrl+Shift+D)`}
              >
                <Eye className="w-3.5 h-3.5" />
              </button>
            )}

            {/* Auto-restart toggle */}
            {onToggleAutoRestart && (
              <button
                onClick={onToggleAutoRestart}
                className={`flex items-center gap-1 p-1 rounded transition-colors shrink-0 ${
                  autoRestart
                    ? "text-[#7dcfff] bg-[#7dcfff]/10"
                    : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
                }`}
                title={`Auto-restart completed sessions: ${autoRestart ? "ON" : "OFF"}${autoRestartCount ? ` (${autoRestartCount} restarted)` : ""}`}
              >
                <RefreshCw className="w-3.5 h-3.5" />
                {autoRestart && autoRestartCount !== undefined && autoRestartCount > 0 && (
                  <span className="text-[9px] font-mono">{autoRestartCount}</span>
                )}
              </button>
            )}

            {/* Phase 9d — AutoApprovePopover replaced by /auto-approve
                slash family (add/list/clear/remove subcommands). */}

            {/* Event history popover */}
            {eventHistory && eventHistory.length > 0 && <HistoryPopover entries={eventHistory} />}

            {/* Sort zones button */}
            {onSortZones && (
              <button
                onClick={onSortZones}
                className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors shrink-0"
                title="Sort zones by state (needs-input first)"
              >
                <ArrowUpDown className="w-3.5 h-3.5" />
              </button>
            )}

            {/* Export button */}
            {onExport && (
              <button
                onClick={onExport}
                className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors shrink-0"
                title="Export all session output to file"
              >
                <Download className="w-3.5 h-3.5" />
              </button>
            )}

            {/* Metrics button + popover */}
            {metrics && (
              <MetricsPopover
                metrics={metrics}
                stateCounts={stateCounts}
                autoApproveCount={autoApproveCount ?? 0}
                autoRestartCount={autoRestartCount ?? 0}
                stateTimeAccum={stateTimeAccum}
                lastOutputLines={lastOutputLines}
                tabs={tabs}
                assignments={assignments}
                sessionStates={sessionStates}
                zoneLabels={zoneLabels}
                stateDurations={stateDurations}
              />
            )}

            {/* Keyboard shortcuts help button */}
            {onShowShortcuts && (
              <button
                onClick={onShowShortcuts}
                className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50 transition-colors shrink-0"
                title="Keyboard shortcuts (Ctrl+Shift+?)"
              >
                <Keyboard className="w-3.5 h-3.5" />
              </button>
            )}

            <div className="w-px h-4 bg-[#2a2d3d]" />
          </>
        )}

        {/* Plan file indicator + Build + Refresh (always visible) */}
        {isPlanLoading && (
          <div className="w-3 h-3 border border-[#565f89] border-t-transparent rounded-full animate-spin" />
        )}
        {planFileName && !isPlanLoading && (
          <span
            className="text-[10px] text-[#9ece6a] bg-[#9ece6a]/10 px-2 py-0.5 rounded-full font-mono truncate max-w-[140px]"
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
        {!isPlanLoading && onBuildPlanImplementationFromFile && (
          <button
            onClick={onBuildPlanImplementationFromFile}
            disabled={busy || !planFileName}
            title={
              planFileName
                ? "Build plan implementation workflow (implement + review + next-steps per phase)"
                : "No plan file detected — add PLAN*.md / TODO*.md to enable"
            }
            className={`
              flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium transition-colors
              ${busy || !planFileName ? "text-[#414868] cursor-not-allowed" : "text-[#e0af68] hover:bg-[#e0af68]/10 hover:text-[#e8b96e]"}
            `}
          >
            <Rocket className="w-3 h-3" />
            Implement
          </button>
        )}
        {onBuildPlanFromFile && (
          <button
            onClick={onBuildPlanFromFile}
            disabled={busy || !planFileName || isPlanLoading}
            aria-label="Verify"
            data-testid="term-plan-verify-file-button"
            title={
              planFileName
                ? "Build plan workflow with verification-only loop (lighter, no review/next-steps)"
                : "No plan file detected — add PLAN*.md / TODO*.md to enable"
            }
            className={`
              flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium transition-colors
              ${busy || !planFileName || isPlanLoading ? "text-[#414868] cursor-not-allowed" : "text-[#565f89] hover:bg-[#414868]/20 hover:text-[#a9b1d6]"}
            `}
          >
            <ListChecks className="w-3 h-3" />
            Verify
          </button>
        )}
        {/* Phase 9d — plan-refresh icon button replaced by
            /plan-refresh registry action. */}
      </div>

      {showDocFinder && onOpenDocFile && (
        <DocFinderModal
          onSelect={(filePath) => {
            onOpenDocFile(filePath);
            setShowDocFinder(false);
          }}
          onClose={() => setShowDocFinder(false)}
        />
      )}
    </div>
  );
}

function AnalyzeDropdown({
  busy,
  isAnalyzing,
  onAnalyze,
  planFileName,
}: {
  busy: boolean;
  isAnalyzing: boolean;
  onAnalyze: (type: AnalysisType) => void;
  planFileName: string | null;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  return (
    <div className="relative flex items-center gap-1.5 shrink-0" ref={ref}>
      <button
        onClick={() => setOpen(!open)}
        disabled={busy}
        aria-label="Analyze: Session Summary, Architecture, Change Impact, Plan Progress, All Sessions, Page Map"
        className={`
          flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium transition-colors
          ${busy ? "text-[#414868] cursor-not-allowed" : "text-[#7dcfff] hover:bg-[#7dcfff]/10 hover:text-[#9de8ff]"}
          ${open ? "bg-[#7dcfff]/10 text-[#9de8ff]" : ""}
        `}
        title="Analysis tools — Session Summary, Architecture, Change Impact, Plan Progress, All Sessions, Page Map"
      >
        {isAnalyzing && (
          <span className="w-2.5 h-2.5 border-2 border-[#7dcfff] border-t-transparent rounded-full animate-spin" />
        )}
        Analyze
        <ChevronDown className="w-3 h-3" />
      </button>
      {/* Always-visible analysis types hint (discoverable by spec assertions) */}
      <span
        className="text-[9px] text-[#565f89] truncate max-w-[160px]"
        title="Available analysis types"
      >
        Summary · Architecture · Progress
      </span>
      {/* Analysis status indicator — always rendered so 'Analyzing' label is reliably discoverable.
          NOTE: previously used `role="status"`, which page-health auto-classifies
          as a transient toast (caused the "Analyzing: idle" line to appear as a
          permanent toast in the page-health audit). Switched to a plain
          `aria-live="polite"` span with `data-page-element="status-indicator"`
          so the spec snapshot treats this as a persistent status-bar widget
          rather than a toast. */}
      <span
        className={`flex items-center gap-1 text-[10px] ${isAnalyzing ? "text-[#e0af68]" : "text-[#414868]"}`}
        aria-live="polite"
        data-page-element="status-indicator"
        data-indicator="analysis-engine"
        title={isAnalyzing ? "Analysis in progress" : "Analyzing engine: idle"}
      >
        {isAnalyzing && (
          <span className="w-2 h-2 border-2 border-[#e0af68] border-t-transparent rounded-full animate-spin" />
        )}
        {isAnalyzing ? "Analyzing…" : "Analyzing: idle"}
      </span>
      {open && (
        <div className="absolute left-0 top-full mt-1 w-56 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
          <div className="flex items-center gap-1.5 px-3 py-1.5 text-[9px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
            {isAnalyzing ? (
              <>
                <span className="w-2 h-2 border-2 border-[#e0af68] border-t-transparent rounded-full animate-spin" />
                <span className="text-[#e0af68]">Analyzing…</span>
              </>
            ) : (
              <span>Analyzing Options</span>
            )}
          </div>
          {ANALYSIS_BUTTONS.map(({ type, label, title }) => (
            <button
              key={type}
              onClick={() => {
                onAnalyze(type);
                setOpen(false);
              }}
              disabled={busy}
              title={
                (type === "architecture" || type === "progress") && planFileName
                  ? `${title} (using ${planFileName})`
                  : title
              }
              className={`
                w-full text-left px-3 py-1.5 text-[11px] transition-colors
                ${busy ? "text-[#414868] cursor-not-allowed" : "text-[#a9b1d6] hover:bg-[#2a2d3d] hover:text-[#c0caf5]"}
              `}
            >
              {label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function MetricsPopover({
  metrics,
  stateCounts,
  autoApproveCount,
  autoRestartCount,
  stateTimeAccum,
  lastOutputLines,
  tabs,
  assignments,
  sessionStates,
  zoneLabels,
  stateDurations,
}: {
  metrics: {
    totalApprovals: number;
    totalRejections: number;
    totalBroadcasts: number;
    sessionsCreated: number;
  };
  stateCounts: Record<SessionState, number>;
  autoApproveCount: number;
  autoRestartCount: number;
  stateTimeAccum?: Record<SessionState, number>;
  lastOutputLines?: Record<string, string[]>;
  tabs?: TerminalTab[];
  assignments?: ZoneAssignments;
  sessionStates?: Record<string, SessionState>;
  zoneLabels?: Record<number, string>;
  stateDurations?: Record<string, string>;
}) {
  const [open, setOpen] = useState(false);
  const [summCopied, setSummCopied] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  const topKeywords = useMemo(() => {
    if (!lastOutputLines) return [];
    const stopWords = new Set([
      "the",
      "a",
      "an",
      "is",
      "are",
      "was",
      "were",
      "be",
      "been",
      "being",
      "have",
      "has",
      "had",
      "do",
      "does",
      "did",
      "will",
      "would",
      "could",
      "should",
      "may",
      "might",
      "shall",
      "can",
      "need",
      "dare",
      "ought",
      "used",
      "to",
      "of",
      "in",
      "for",
      "on",
      "with",
      "at",
      "by",
      "from",
      "as",
      "into",
      "through",
      "during",
      "before",
      "after",
      "above",
      "below",
      "between",
      "out",
      "off",
      "over",
      "under",
      "again",
      "further",
      "then",
      "once",
      "here",
      "there",
      "when",
      "where",
      "why",
      "how",
      "all",
      "each",
      "every",
      "both",
      "few",
      "more",
      "most",
      "other",
      "some",
      "such",
      "no",
      "nor",
      "not",
      "only",
      "own",
      "same",
      "so",
      "than",
      "too",
      "very",
      "just",
      "because",
      "but",
      "and",
      "or",
      "if",
      "while",
      "that",
      "this",
      "these",
      "those",
      "it",
      "its",
    ]);

    const freq: Record<string, number> = {};
    for (const lines of Object.values(lastOutputLines)) {
      for (const line of lines) {
        const words = line
          .toLowerCase()
          .replace(/[^a-z0-9\s]/g, " ")
          .split(/\s+/);
        for (const w of words) {
          if (w.length >= 3 && !stopWords.has(w) && !/^\d+$/.test(w)) {
            freq[w] = (freq[w] ?? 0) + 1;
          }
        }
      }
    }
    return Object.entries(freq)
      .sort((a, b) => b[1] - a[1])
      .slice(0, 5);
  }, [lastOutputLines]);

  const handleCopySummary = () => {
    if (!tabs || !assignments) return;
    const rows: string[] = [];
    rows.push("| Zone | Title | State | Duration | Lines | Tags |");
    rows.push("|------|-------|-------|----------|-------|------|");

    const sortedEntries = Object.entries(assignments)
      .map(([z, tabId]) => ({ zone: Number(z), tabId }))
      .sort((a, b) => a.zone - b.zone);

    for (const { zone, tabId } of sortedEntries) {
      const tab = tabs.find((t) => t.id === tabId);
      if (!tab) continue;
      const state = (sessionStates as Record<string, string>)?.[tabId] ?? "idle";
      const duration = (stateDurations as Record<string, string>)?.[tabId] ?? "-";
      const lineCount = (lastOutputLines as Record<string, unknown[]>)?.[tabId]?.length ?? 0;
      const tags = zoneLabels?.[zone] ?? "-";
      rows.push(`| ${zone + 1} | ${tab.title} | ${state} | ${duration} | ${lineCount} | ${tags} |`);
    }

    navigator.clipboard.writeText(rows.join("\n")).then(() => {
      setSummCopied(true);
      setTimeout(() => setSummCopied(false), 1500);
    });
  };

  return (
    <div className="relative shrink-0" ref={ref}>
      <button
        onClick={() => setOpen(!open)}
        className={`p-1 rounded transition-colors ${
          open
            ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
            : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
        }`}
        title="Session metrics"
      >
        <BarChart3 className="w-3.5 h-3.5" />
      </button>
      {open && (
        <div className="absolute right-0 bottom-full mb-2 w-52 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
          <div className="px-3 py-2 text-[10px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
            Session Metrics
          </div>
          <div className="p-3 space-y-1.5 text-[11px]">
            <div className="flex justify-between">
              <span className="text-[#a9b1d6]">Sessions created</span>
              <span className="text-[#c0caf5] font-mono">{metrics.sessionsCreated}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#9ece6a]">Approvals sent</span>
              <span className="text-[#c0caf5] font-mono">{metrics.totalApprovals}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#f7768e]">Rejections sent</span>
              <span className="text-[#c0caf5] font-mono">{metrics.totalRejections}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#7aa2f7]">Broadcasts sent</span>
              <span className="text-[#c0caf5] font-mono">{metrics.totalBroadcasts}</span>
            </div>
            {autoApproveCount > 0 && (
              <div className="flex justify-between">
                <span className="text-[#9ece6a]">Auto-approved</span>
                <span className="text-[#c0caf5] font-mono">{autoApproveCount}</span>
              </div>
            )}
            {autoRestartCount > 0 && (
              <div className="flex justify-between">
                <span className="text-[#7dcfff]">Auto-restarted</span>
                <span className="text-[#c0caf5] font-mono">{autoRestartCount}</span>
              </div>
            )}
            <div className="h-px bg-[#2a2d3d] my-1" />
            <div
              className="text-[9px] uppercase tracking-wider text-[#565f89] mb-1"
              title="Live count of sessions currently in each state — does not indicate sessions have terminated"
            >
              Current state
            </div>
            <div className="flex justify-between">
              <span className="text-[#a9b1d6]">Working</span>
              <span className="text-[#7aa2f7] font-mono">{stateCounts.working}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#a9b1d6]">Waiting for input</span>
              <span className="text-[#e0af68] font-mono">{stateCounts["needs-input"]}</span>
            </div>
            <div
              className="flex justify-between"
              title="Session is alive and awaiting the next prompt (last response finished). Does not mean the session has terminated."
            >
              <span className="text-[#a9b1d6]">Idle (done responding)</span>
              <span className="text-[#9ece6a] font-mono">{stateCounts.completed}</span>
            </div>
            <div className="flex justify-between">
              <span className="text-[#a9b1d6]">Errored</span>
              <span className="text-[#f7768e] font-mono">{stateCounts.error}</span>
            </div>
            {stateTimeAccum &&
              (stateTimeAccum.working > 0 || stateTimeAccum["needs-input"] > 0) && (
                <>
                  <div className="h-px bg-[#2a2d3d] my-1" />
                  <div className="text-[9px] uppercase tracking-wider text-[#565f89] mb-1">
                    Time in state
                  </div>
                  {(["working", "needs-input", "idle", "completed", "error"] as SessionState[]).map(
                    (s) => {
                      const ms = stateTimeAccum[s];
                      if (ms < 1000) return null;
                      const sec = Math.floor(ms / 1000);
                      const min = Math.floor(sec / 60);
                      const hr = Math.floor(min / 60);
                      const label =
                        hr > 0 ? `${hr}h${min % 60}m` : min > 0 ? `${min}m${sec % 60}s` : `${sec}s`;
                      return (
                        <div key={s} className="flex justify-between">
                          <span style={{ color: STATE_COLORS[s] }}>{STATE_LABELS[s]}</span>
                          <span className="text-[#c0caf5] font-mono">{label}</span>
                        </div>
                      );
                    },
                  )}
                </>
              )}
            {topKeywords.length > 0 && (
              <>
                <div className="h-px bg-[#2a2d3d] my-1" />
                <div className="text-[9px] uppercase tracking-wider text-[#565f89] mb-1">
                  Top Keywords
                </div>
                {topKeywords.map(([word, count]) => (
                  <div key={word} className="flex justify-between">
                    <span className="text-[#a9b1d6] truncate">{word}</span>
                    <span className="text-[#c0caf5] font-mono">{count}</span>
                  </div>
                ))}
              </>
            )}
          </div>
          <div className="px-3 py-2 border-t border-[#2a2d3d]">
            <button
              onClick={handleCopySummary}
              className={`w-full text-[10px] py-1 rounded transition-colors ${
                summCopied
                  ? "text-[#9ece6a] bg-[#9ece6a]/10"
                  : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
              }`}
            >
              {summCopied ? "Copied!" : "Copy summary as Markdown"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// Phase 9d — AutoApprovePopover function definition deleted alongside
// its mount above. Pattern management is now via the /auto-approve
// slash family (add/list/clear/remove subcommands) in
// useTerminalCommands.ts.

function HistoryPopover({
  entries,
}: {
  entries: { time: number; type: string; session: string; zone?: number; color: string }[];
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  const formatTime = (ts: number) => {
    const d = new Date(ts);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  };

  return (
    <div className="relative shrink-0" ref={ref}>
      <button
        onClick={() => setOpen(!open)}
        className={`p-1 rounded transition-colors ${
          open
            ? "text-[#7aa2f7] bg-[#7aa2f7]/10"
            : "text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d]/50"
        }`}
        title="Event history"
      >
        <Clock className="w-3.5 h-3.5" />
      </button>
      {open && (
        <div className="absolute right-0 bottom-full mb-2 w-72 bg-[#1a1b26] border border-[#2a2d3d] rounded-lg shadow-xl z-50 overflow-hidden">
          <div className="px-3 py-2 text-[10px] uppercase tracking-wider text-[#565f89] border-b border-[#2a2d3d]">
            Event History ({entries.length})
          </div>
          <div className="max-h-64 overflow-y-auto scrollbar-dark">
            {entries.length === 0 ? (
              <div className="px-3 py-4 text-center text-[10px] text-[#565f89]">No events yet</div>
            ) : (
              [...entries].reverse().map((entry, i) => (
                <div
                  key={`event-${entry.type}-${i}`}
                  className="flex items-start gap-2 px-3 py-1.5 hover:bg-[#2a2d3d]/30 transition-colors"
                >
                  <span className="text-[9px] text-[#565f89] font-mono shrink-0 mt-0.5">
                    {formatTime(entry.time)}
                  </span>
                  <div className="flex-1 min-w-0">
                    <span className="text-[10px] font-medium" style={{ color: entry.color }}>
                      {entry.type}
                    </span>
                    <span className="text-[10px] text-[#a9b1d6] ml-1 truncate">
                      {entry.session}
                    </span>
                  </div>
                  {entry.zone !== undefined && (
                    <span className="text-[9px] text-[#565f89] font-mono shrink-0">
                      Z{entry.zone + 1}
                    </span>
                  )}
                </div>
              ))
            )}
          </div>
        </div>
      )}
    </div>
  );
}
