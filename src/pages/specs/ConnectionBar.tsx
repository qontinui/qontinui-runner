/**
 * ConnectionBar — Connect to apps via UI Bridge, load bundled/file specs, build workflows
 */

import { useState } from "react";
import { useUIElement } from "ui-bridge";
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
} from "lucide-react";
import type { ConnectionState } from "./types";

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
  onSyncAllSpecs: () => void;
  isSyncing: boolean;
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
  onSyncAllSpecs,
  isSyncing,
  onToggleEditMode,
}: ConnectionBarProps) {
  const [url, setUrl] = useState(connection.url || "http://localhost:3001");

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
        <button
          ref={syncRef}
          onClick={onSyncAllSpecs}
          disabled={isLoading || isSyncing || stats.totalSpecs === 0}
          className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-semibold rounded-md
          bg-teal-600 text-white shadow-xs shadow-teal-600/25
          hover:bg-teal-700 disabled:opacity-50 transition-colors shrink-0"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${isSyncing ? "animate-spin" : ""}`} />
          Sync All Specs
        </button>
        <button
          ref={compileRef}
          onClick={onCompileStateMachine}
          disabled={isLoading || stats.totalSpecs === 0}
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
