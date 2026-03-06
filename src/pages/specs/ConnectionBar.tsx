/**
 * ConnectionBar — Connect to apps via UI Bridge, load bundled/file specs, build workflows
 */

import { useState } from "react";
import {
  WifiOff,
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
  onLoadBundled: () => void;
  onDiscover: (url: string) => void;
  onLoadFromFile: () => void;
  onSaveToFile: () => void;
  onBuildWorkflow: () => void;
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
  onLoadBundled,
  onDiscover,
  onLoadFromFile,
  onSaveToFile,
  onBuildWorkflow,
  onToggleEditMode,
}: ConnectionBarProps) {
  const [url, setUrl] = useState(connection.url || "http://localhost:3001");

  return (
    <div className="flex items-center gap-2 px-4 py-2 border-b border-border bg-white/[0.01] flex-wrap">
      {/* Load bundled */}
      <button
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
          type="text"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="http://localhost:3001"
          className="flex-1 min-w-[140px] max-w-[260px] px-2.5 py-1 text-xs rounded
            bg-white/5 border border-white/10 text-foreground
            placeholder:text-muted-foreground/50
            focus:outline-none focus:border-cyan-500/50"
          onKeyDown={(e) => {
            if (e.key === "Enter" && url.trim()) onDiscover(url.trim());
          }}
        />
        <button
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

      {/* Connection status */}
      <div className="flex items-center gap-1.5 text-xs shrink-0">
        {connection.status === "connected" && (
          <>
            <CheckCircle2 className="w-3.5 h-3.5 text-green-400" />
            <span className="text-green-400">{connection.appName || "Connected"}</span>
          </>
        )}
        {connection.status === "error" && (
          <>
            <AlertCircle className="w-3.5 h-3.5 text-red-400 shrink-0" />
            <span className="text-red-400 truncate max-w-[350px]" title={connection.error}>
              {connection.error}
            </span>
          </>
        )}
        {connection.status === "disconnected" && (
          <>
            <WifiOff className="w-3.5 h-3.5 text-muted-foreground/50" />
          </>
        )}
        {connection.status === "connecting" && (
          <>
            <Loader2 className="w-3.5 h-3.5 text-cyan-400 animate-spin" />
          </>
        )}
      </div>

      <div className="w-px h-5 bg-border" />

      {/* Edit mode toggle */}
      {hasSelectedSpec && selectedSpecKind === "page-spec" && (
        <button
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
      {hasSelectedSpec && (
        <button
          onClick={onBuildWorkflow}
          className="flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded
            bg-orange-500/10 text-orange-400 border border-orange-500/20
            hover:bg-orange-500/20 transition-colors shrink-0"
        >
          <Hammer className="w-3.5 h-3.5" />
          Build Workflow
        </button>
      )}

      {/* Stats */}
      {stats.totalSpecs > 0 && (
        <div className="flex items-center gap-3 text-[10px] text-muted-foreground shrink-0 ml-auto">
          <span>{stats.totalSpecs} specs</span>
          <span>{stats.totalGroups} groups</span>
          <span>{stats.totalAssertions} assertions</span>
        </div>
      )}
    </div>
  );
}
