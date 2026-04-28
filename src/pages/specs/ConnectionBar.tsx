/**
 * ConnectionBar — Connect to apps via UI Bridge, load bundled/file specs, build workflows
 */

import { useEffect, useState } from "react";
import { useUIElement } from "@qontinui/ui-bridge";
import {
  Loader2,
  BookOpen,
  Search,
  AlertCircle,
  CheckCircle2,
  FolderOpen,
  Save,
  Hammer,
  Pencil,
  PencilOff,
  Brain,
  Bug,
  Network,
  RefreshCw,
  Sparkles,
  ShieldAlert,
} from "lucide-react";
import type { ConnectionState } from "./types";
import { SpecDriftButton } from "./SpecDriftButton";
import { SyncConfirmModal } from "./SyncConfirmModal";
import type { AnalyzeResult, RunSyncOptions } from "@/hooks/useSpecSync";

interface ConnectionBarProps {
  connection: ConnectionState;
  isLoading: boolean;
  stats: { totalSpecs: number; totalGroups: number; totalAssertions: number };
  editMode: boolean;
  hasSelectedSpec: boolean;
  selectedSpecKind: string | null;
  forcePromptOnly: boolean;
  onToggleForcePromptOnly: () => void;
  includeRegressionChecks: boolean;
  onToggleRegressionChecks: () => void;
  regressionIssueCount: number;
  useAiSpecGeneration: boolean;
  onToggleUseAiSpecGeneration: () => void;
  isGeneratingWithAi: boolean;
  onLoadBundled: () => void;
  onDiscover: (url: string) => void;
  onLoadFromFile: () => void;
  onSaveToFile: () => void;
  onBuildWorkflow: () => void;
  onCompileStateMachine: () => void;
  /**
   * Analyze without starting the sync loop. Used to populate the
   * pre-flight confirmation modal.
   */
  onAnalyzeSpecs: (options: RunSyncOptions) => Promise<AnalyzeResult>;
  /**
   * Start the sync loop. The pre-flight modal calls this with the
   * AnalyzeResult it already computed so the hook does not re-analyze.
   */
  onRunSync: (options: RunSyncOptions, precomputed?: AnalyzeResult) => Promise<void>;
  isSyncing: boolean;
  syncProgress?: { current: number; total: number };
  onCancelSync?: () => void;
  onToggleEditMode: () => void;
}

export function ConnectionBar({
  connection,
  isLoading,
  stats,
  editMode,
  hasSelectedSpec,
  selectedSpecKind,
  forcePromptOnly,
  onToggleForcePromptOnly,
  includeRegressionChecks,
  onToggleRegressionChecks,
  regressionIssueCount,
  useAiSpecGeneration,
  onToggleUseAiSpecGeneration,
  isGeneratingWithAi,
  onLoadBundled,
  onDiscover,
  onLoadFromFile,
  onSaveToFile,
  onBuildWorkflow,
  onCompileStateMachine,
  onAnalyzeSpecs,
  onRunSync,
  isSyncing,
  syncProgress,
  onCancelSync,
  onToggleEditMode,
}: ConnectionBarProps) {
  const [url, setUrl] = useState(connection.url || "http://localhost:3001");

  // Sync All Specs pre-flight state.
  // `onlyQuarantined` defaults true — auto-disabled below if the most-recent
  // quarantine record has no entries.
  const [onlyQuarantined, setOnlyQuarantined] = useState(true);
  const [hasQuarantined, setHasQuarantined] = useState(false);
  const [pendingAnalyze, setPendingAnalyze] = useState<AnalyzeResult | null>(null);
  const [analyzing, setAnalyzing] = useState(false);
  const [modalOpen, setModalOpen] = useState(false);

  // Background probe to enable/disable the toggle when the page first
  // mounts. Does NOT block the sync button; if the probe fails, fall back
  // to "no quarantined" (toggle disabled) — actual analyze happens on click.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const result = await onAnalyzeSpecs({ onlyQuarantined: true });
        if (!cancelled) {
          const any = result.quarantinedStateIds.size > 0;
          setHasQuarantined(any);
          if (!any) setOnlyQuarantined(false);
        }
      } catch {
        if (!cancelled) setHasQuarantined(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [onAnalyzeSpecs]);

  const handleSyncClick = async () => {
    if (analyzing) return;
    setAnalyzing(true);
    try {
      const result = await onAnalyzeSpecs({ onlyQuarantined });
      // Update the toggle's enabled-state from the freshest analyze pass.
      setHasQuarantined(result.quarantinedStateIds.size > 0);
      setPendingAnalyze(result);
      setModalOpen(true);
    } finally {
      setAnalyzing(false);
    }
  };

  const handleConfirm = () => {
    setModalOpen(false);
    const precomputed = pendingAnalyze;
    setPendingAnalyze(null);
    void onRunSync({ onlyQuarantined }, precomputed ?? undefined);
  };

  const handleModalCancel = () => {
    setModalOpen(false);
    setPendingAnalyze(null);
  };

  const { ref: bundledRef } = useUIElement({
    id: "specs-btn-bundled",
    type: "button",
    label: "Load bundled specs",
    actions: ["click"],
  });
  const { ref: fileRef } = useUIElement({
    id: "specs-btn-file",
    type: "button",
    label: "Load specs from file",
    actions: ["click"],
  });
  const { ref: discoverRef } = useUIElement({
    id: "specs-btn-discover",
    type: "button",
    label: "Discover specs from app",
    actions: ["click"],
  });
  const { ref: discoverInputRef } = useUIElement({
    id: "specs-input-url",
    type: "input",
    label: "App URL for spec discovery",
  });
  const { ref: editRef } = useUIElement({
    id: "specs-btn-edit",
    type: "button",
    label: "Toggle edit mode",
    actions: ["click"],
  });
  const { ref: saveRef } = useUIElement({
    id: "specs-btn-save",
    type: "button",
    label: "Save spec to file",
    actions: ["click"],
  });
  const { ref: aiEvalRef } = useUIElement({
    id: "specs-btn-ai-eval",
    type: "button",
    label: "Toggle AI evaluation mode",
    actions: ["click"],
  });
  const { ref: buildRef } = useUIElement({
    id: "specs-btn-build-workflow",
    type: "button",
    label: "Build workflow from spec",
    actions: ["click"],
  });
  const { ref: syncRef } = useUIElement({
    id: "specs-btn-sync-all",
    type: "button",
    label: "Sync all specs with AI",
    actions: ["click"],
  });
  const { ref: compileRef } = useUIElement({
    id: "specs-btn-compile-sm",
    type: "button",
    label: "Compile state machine",
    actions: ["click"],
  });

  return (
    <div className="border-b border-border bg-white/[0.01]">
      <div className="flex items-center gap-2 px-4 py-2 flex-wrap">
        {/* Load bundled */}
        <button
          ref={bundledRef}
          onClick={onLoadBundled}
          className="flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
          bg-purple-500/10 text-purple-400 border border-purple-500/20
          hover:bg-purple-500/20 transition-colors shrink-0"
        >
          <BookOpen className="w-3.5 h-3.5" />
          Bundled
        </button>

        {/* Load from file */}
        <button
          ref={fileRef}
          onClick={onLoadFromFile}
          className="flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
          bg-emerald-500/10 text-emerald-400 border border-emerald-500/20
          hover:bg-emerald-500/20 transition-colors shrink-0"
        >
          <FolderOpen className="w-3.5 h-3.5" />
          File
        </button>

        <div className="w-px h-5 bg-border" />

        {/* Discover from app */}
        <div className="flex items-center gap-1.5 flex-1 min-w-0">
          <input
            ref={discoverInputRef}
            type="text"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="http://localhost:3001"
            className="flex-1 min-w-[140px] max-w-[260px] px-2.5 py-1 text-xs rounded
            bg-white/5 border border-white/10 text-foreground
            placeholder:text-muted-foreground/50
            focus:outline-hidden focus:border-cyan-500/50"
            onKeyDown={(e) => {
              if (e.key === "Enter" && url.trim()) onDiscover(url.trim());
            }}
          />
          <button
            ref={discoverRef}
            onClick={() => url.trim() && onDiscover(url.trim())}
            disabled={isLoading || !url.trim()}
            className="flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
            bg-cyan-500/10 text-cyan-400 border border-cyan-500/20
            hover:bg-cyan-500/20 disabled:opacity-50 transition-colors shrink-0"
          >
            {isLoading ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Search className="w-3.5 h-3.5" />
            )}
            Discover
          </button>
        </div>

        <div className="w-px h-5 bg-border" />

        {/* Edit mode toggle */}
        {hasSelectedSpec && selectedSpecKind === "page-spec" && (
          <button
            ref={editRef}
            onClick={onToggleEditMode}
            className={`flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
            border transition-colors shrink-0 ${
              editMode
                ? "bg-amber-500/15 text-amber-400 border-amber-500/30"
                : "bg-white/5 text-muted-foreground border-white/10 hover:bg-white/10"
            }`}
          >
            {editMode ? <PencilOff className="w-3.5 h-3.5" /> : <Pencil className="w-3.5 h-3.5" />}
            {editMode ? "Done" : "Edit"}
          </button>
        )}

        {/* Save to file */}
        {hasSelectedSpec && (
          <button
            ref={saveRef}
            onClick={onSaveToFile}
            className="flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
            bg-white/5 text-muted-foreground border border-white/10
            hover:bg-white/10 transition-colors shrink-0"
          >
            <Save className="w-3.5 h-3.5" />
            Save
          </button>
        )}

        {/* Force AI evaluation toggle + Build workflow */}
        {hasSelectedSpec && selectedSpecKind === "page-spec" && (
          <button
            ref={aiEvalRef}
            onClick={onToggleForcePromptOnly}
            className={`flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
            border transition-colors shrink-0 ${
              forcePromptOnly
                ? "bg-amber-500/15 text-amber-400 border-amber-500/30"
                : "bg-white/5 text-muted-foreground border-white/10 hover:bg-white/10"
            }`}
            title="When enabled, all assertions are evaluated by AI instead of using fast deterministic checks. Use this when you need AI judgment on element existence."
          >
            <Brain className="w-3.5 h-3.5" />
            AI Eval
          </button>
        )}
        {hasSelectedSpec && selectedSpecKind === "page-spec" && regressionIssueCount > 0 && (
          <button
            onClick={onToggleRegressionChecks}
            className={`flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
            border transition-colors shrink-0 ${
              includeRegressionChecks
                ? "bg-purple-500/15 text-purple-400 border-purple-500/30"
                : "bg-white/5 text-muted-foreground border-white/10 hover:bg-white/10"
            }`}
            title="Include regression checks for known issues in the generated workflow"
          >
            <Bug className="w-3.5 h-3.5" />
            Regression ({regressionIssueCount})
          </button>
        )}
        {/* AI spec-generation toggle — routes Build Workflow through the
            Builder-Verifier-Fixer pipeline instead of the deterministic
            buildSpecWorkflow path. Persisted via instanceStorage. */}
        <button
          id="specs-ai-toggle"
          type="button"
          role="switch"
          aria-pressed={useAiSpecGeneration}
          aria-label="Generate with AI (spec brief)"
          onClick={onToggleUseAiSpecGeneration}
          className={`flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
          border transition-colors shrink-0 ${
            useAiSpecGeneration
              ? "bg-fuchsia-500/15 text-fuchsia-400 border-fuchsia-500/30"
              : "bg-white/5 text-muted-foreground border-white/10 hover:bg-white/10"
          }`}
          title="Generate the workflow with the AI spec-brief pipeline (experimental). When off, the deterministic buildSpecWorkflow path is used."
        >
          <Sparkles className="w-3.5 h-3.5" />
          Generate with AI (spec brief)
        </button>
        {hasSelectedSpec && (
          <button
            ref={buildRef}
            onClick={onBuildWorkflow}
            disabled={isLoading || isGeneratingWithAi}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold rounded-md
            bg-orange-500 text-white shadow-xs shadow-orange-500/25
            hover:bg-orange-600 disabled:opacity-50 transition-colors shrink-0"
          >
            {isGeneratingWithAi ? (
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
            ) : (
              <Hammer className="w-3.5 h-3.5" />
            )}
            {isGeneratingWithAi ? "Generating…" : "Build Workflow"}
          </button>
        )}
        {isSyncing ? (
          <button
            ref={syncRef}
            onClick={onCancelSync}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold rounded-md
            bg-teal-600 text-white shadow-xs shadow-teal-600/25
            hover:bg-teal-700 transition-colors shrink-0"
          >
            <RefreshCw className="w-3.5 h-3.5 animate-spin" />
            {syncProgress && syncProgress.total > 0
              ? `Syncing ${syncProgress.current}/${syncProgress.total}...`
              : "Syncing..."}
            <span className="ml-1 text-[10px] opacity-75">(cancel)</span>
          </button>
        ) : (
          <>
            {/* "Sync only quarantined" toggle — defaults ON when there are
                quarantined specs, otherwise disabled with a tooltip. */}
            <button
              id="specs-toggle-only-quarantined"
              type="button"
              role="switch"
              aria-pressed={onlyQuarantined}
              aria-label="Sync only quarantined specs"
              onClick={() => hasQuarantined && setOnlyQuarantined((v) => !v)}
              disabled={!hasQuarantined}
              title={
                hasQuarantined
                  ? "When ON, only specs that touch a quarantined state are re-synced. Toggle OFF to force a full re-sync (e.g. after editing the AI prompt)."
                  : "no quarantined specs"
              }
              className={`flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
              border transition-colors shrink-0 ${
                onlyQuarantined && hasQuarantined
                  ? "bg-amber-500/15 text-amber-400 border-amber-500/30"
                  : "bg-white/5 text-muted-foreground border-white/10 hover:bg-white/10 disabled:opacity-50 disabled:cursor-not-allowed"
              }`}
            >
              <ShieldAlert className="w-3.5 h-3.5" />
              Only quarantined
            </button>

            <button
              ref={syncRef}
              onClick={() => void handleSyncClick()}
              disabled={isLoading || stats.totalSpecs === 0 || analyzing}
              title={
                "Re-runs the AI against every loaded spec that is in the latest " +
                "quarantine record (or missing its stateMachine section). Toggle " +
                "'Only quarantined' off to force a full re-sync. A confirmation modal " +
                "shows the count + estimated duration before any work starts."
              }
              className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold rounded-md
              bg-teal-600 text-white shadow-xs shadow-teal-600/25
              hover:bg-teal-700 disabled:opacity-50 transition-colors shrink-0"
            >
              {analyzing ? (
                <Loader2 className="w-3.5 h-3.5 animate-spin" />
              ) : (
                <RefreshCw className="w-3.5 h-3.5" />
              )}
              {analyzing ? "Analyzing…" : "Sync All Specs"}
            </button>
          </>
        )}
        <SyncConfirmModal
          open={modalOpen}
          toSync={pendingAnalyze?.toSync ?? []}
          onConfirm={handleConfirm}
          onCancel={handleModalCancel}
        />
        <SpecDriftButton />
        <button
          ref={compileRef}
          onClick={onCompileStateMachine}
          disabled={isLoading || stats.totalSpecs === 0}
          title={
            "Compiles the stateMachine sections of every loaded .spec.uibridge.json " +
            "into one runtime state machine, loads it into the UI Bridge engine so the " +
            "pathfinder can navigate by state, and persists it to the backend DB. " +
            "This is the canonical build-from-declared-specs flow. " +
            "Note: different from the State Machine page's 'Generate for Runner' button, " +
            "which reverse-engineers a state machine by statically analyzing React source."
          }
          className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold rounded-md
          bg-violet-600 text-white shadow-xs shadow-violet-600/25
          hover:bg-violet-700 disabled:opacity-50 transition-colors shrink-0"
        >
          <Network className="w-3.5 h-3.5" />
          Compile State Machine
        </button>

        {/* Stats */}
        {stats.totalSpecs > 0 && (
          <div className="flex items-center gap-3 text-[10px] text-muted-foreground shrink-0 ml-auto">
            <span>{stats.totalSpecs} specs</span>
            <span>{stats.totalGroups} groups</span>
            <span>{stats.totalAssertions} assertions</span>
          </div>
        )}
      </div>

      {/* Connection status — separate row so it never overlaps buttons */}
      {connection.status !== "disconnected" && (
        <div className="flex items-center gap-1.5 px-4 py-1 text-xs border-t border-border/50">
          {connection.status === "connected" && (
            <>
              <CheckCircle2 className="w-3.5 h-3.5 text-green-400" />
              <span className="text-green-400">{connection.appName || "Connected"}</span>
            </>
          )}
          {connection.status === "error" && (
            <>
              <AlertCircle className="w-3.5 h-3.5 text-red-400 shrink-0" />
              <span className="text-red-400">{connection.error}</span>
            </>
          )}
          {connection.status === "connecting" && (
            <>
              <Loader2 className="w-3.5 h-3.5 text-cyan-400 animate-spin" />
              <span className="text-cyan-400">Connecting…</span>
            </>
          )}
        </div>
      )}
    </div>
  );
}
