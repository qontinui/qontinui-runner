/**
 * ContextList.tsx
 *
 * Main list view component for displaying and managing AI contexts.
 * Integrates filters, cards, and editor dialog.
 */

import { useState, useCallback } from "react";
import { BookOpen, Plus, RefreshCw, Loader2, AlertCircle, FolderOpen, User } from "lucide-react";
import { useContexts } from "./hooks/useContexts";
import { ContextFilters } from "./ContextFilters";
import { ContextCard } from "./ContextCard";
import { ContextEditor } from "./ContextEditor";
import { getAccentColors, getStatusColors } from "@/design-system";
import type {
  ContextWithMetadata,
  CreateContextRequest,
  UpdateContextRequest,
} from "../../types/context";

type LogLevel = "info" | "warning" | "error" | "debug" | "success";

/**
 * Local editor state that uses ContextWithMetadata
 * (extends the base ContextEditorState which uses Context)
 */
interface LocalEditorState {
  isOpen: boolean;
  context: ContextWithMetadata | null;
  mode: "create" | "edit" | "view";
  createScope?: "project" | "user";
}

interface ContextListProps {
  onLog?: (level: LogLevel, message: string) => void;
}

export function ContextList({ onLog }: ContextListProps) {
  const {
    contexts,
    categories,
    tags,
    filters,
    setFilters,
    filteredContexts,
    loading,
    error,
    createContext,
    updateContext,
    deleteContext,
    duplicateContext,
    enableContext,
    disableContext,
    approveContextSync,
    dismissContextSync,
    refresh,
  } = useContexts();

  // Editor state - default to user scope since it's always available
  const [editorState, setEditorState] = useState<LocalEditorState>({
    isOpen: false,
    context: null,
    mode: "create",
    createScope: "user",
  });
  const [isSaving, setIsSaving] = useState(false);
  const [processingId, setProcessingId] = useState<string | null>(null);

  // Duplicate dialog state
  const [duplicateTarget, setDuplicateTarget] = useState<{
    context: ContextWithMetadata;
    showDialog: boolean;
  } | null>(null);

  const log = useCallback(
    (level: LogLevel, message: string) => {
      onLog?.(level, message);
      console.log(`[CONTEXTS] [${level.toUpperCase()}] ${message}`);
    },
    [onLog],
  );

  const handleCreate = (scope: "project" | "user") => {
    setEditorState({
      isOpen: true,
      context: null,
      mode: "create",
      createScope: scope,
    });
  };

  const handleEdit = (context: ContextWithMetadata) => {
    setEditorState({
      isOpen: true,
      context,
      mode: context.scope === "builtin" ? "view" : "edit",
    });
  };

  const handleView = (context: ContextWithMetadata) => {
    setEditorState({
      isOpen: true,
      context,
      mode: "view",
    });
  };

  const handleDelete = async (context: ContextWithMetadata) => {
    if (context.scope === "builtin") {
      log("warning", "Cannot delete built-in contexts");
      return;
    }

    if (!confirm(`Are you sure you want to delete "${context.name}"?`)) {
      return;
    }

    setProcessingId(context.id);
    const success = await deleteContext(context.scope, context.id);
    setProcessingId(null);

    if (success) {
      log("success", `Deleted context: ${context.name}`);
    } else {
      log("error", `Failed to delete context: ${context.name}`);
    }
  };

  const handleDuplicate = (context: ContextWithMetadata) => {
    setDuplicateTarget({ context, showDialog: true });
  };

  const handleDuplicateConfirm = async (targetScope: "project" | "user") => {
    if (!duplicateTarget) return;

    setProcessingId(duplicateTarget.context.id);
    const result = await duplicateContext(
      duplicateTarget.context.scope,
      duplicateTarget.context.id,
      targetScope,
    );
    setProcessingId(null);
    setDuplicateTarget(null);

    if (result) {
      log("success", `Duplicated context: ${result.name}`);
    } else {
      log("error", `Failed to duplicate context`);
    }
  };

  const handleToggleEnabled = async (context: ContextWithMetadata) => {
    setProcessingId(context.id);
    const success = context.enabled
      ? await disableContext(context.id)
      : await enableContext(context.id);
    setProcessingId(null);

    if (success) {
      log("success", `${context.enabled ? "Disabled" : "Enabled"} context: ${context.name}`);
    }
  };

  const handleApproveSync = async (context: ContextWithMetadata) => {
    setProcessingId(context.id);
    const success = await approveContextSync(context.id);
    setProcessingId(null);

    if (success) {
      log("success", `Synced context to qontinui-web: ${context.name}`);
    } else {
      log("error", `Failed to sync context: ${context.name}`);
    }
  };

  const handleDismissSync = async (context: ContextWithMetadata) => {
    setProcessingId(context.id);
    const success = await dismissContextSync(context.id);
    setProcessingId(null);

    if (success) {
      log("info", `Context kept local only: ${context.name}`);
    }
  };

  const handleSave = async (
    data: CreateContextRequest | UpdateContextRequest,
  ): Promise<boolean> => {
    setIsSaving(true);

    try {
      if (editorState.mode === "create") {
        const result = await createContext(
          editorState.createScope || "user",
          data as CreateContextRequest,
        );
        if (result) {
          log("success", `Created context: ${result.name}`);
          return true;
        }
      } else if (editorState.mode === "edit" && editorState.context) {
        const result = await updateContext(
          editorState.context.scope,
          editorState.context.id,
          data as UpdateContextRequest,
        );
        if (result) {
          log("success", `Updated context: ${result.name}`);
          return true;
        }
      }
      return false;
    } finally {
      setIsSaving(false);
    }
  };

  const closeEditor = () => {
    setEditorState({
      isOpen: false,
      context: null,
      mode: "create",
    });
  };

  // Group contexts by scope for display
  const groupedContexts = {
    project: filteredContexts.filter((c) => c.scope === "project"),
    user: filteredContexts.filter((c) => c.scope === "user"),
    builtin: filteredContexts.filter((c) => c.scope === "builtin"),
  };

  return (
    <div className="p-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <BookOpen className="w-6 h-6 text-primary" />
          <h2 className="text-xl font-semibold">AI Contexts</h2>
          <span className="text-sm text-muted-foreground">({contexts.length} total)</span>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={refresh}
            disabled={loading}
            className="btn-secondary flex items-center gap-2 px-3 py-2 text-sm"
            title="Refresh"
          >
            <RefreshCw className={`w-4 h-4 ${loading ? "animate-spin" : ""}`} />
          </button>
          <div className="relative group">
            <button className="btn-primary flex items-center gap-2 px-3 py-2 text-sm">
              <Plus className="w-4 h-4" />
              New Context
            </button>
            {/* Dropdown for scope selection */}
            <div className="absolute right-0 top-full mt-1 w-64 bg-card border border-border rounded-lg shadow-lg py-1 hidden group-hover:block z-10">
              <button
                onClick={() => handleCreate("user")}
                className="w-full flex items-start gap-2 px-3 py-2 text-sm text-foreground hover:bg-muted/50 transition-colors"
              >
                <User className={`w-4 h-4 ${getAccentColors("purple").text} mt-0.5`} />
                <div className="text-left">
                  <div className="font-medium">User Context</div>
                  <div className="text-xs text-muted-foreground">Available across all projects</div>
                </div>
              </button>
              <button
                onClick={() => handleCreate("project")}
                className="w-full flex items-start gap-2 px-3 py-2 text-sm text-foreground hover:bg-muted/50 transition-colors"
              >
                <FolderOpen className={`w-4 h-4 ${getAccentColors("blue").text} mt-0.5`} />
                <div className="text-left">
                  <div className="font-medium">Project Context</div>
                  <div className="text-xs text-muted-foreground">Saved with loaded config file</div>
                </div>
              </button>
            </div>
          </div>
        </div>
      </div>

      {/* Error Display */}
      {error && (
        <div
          className={`flex items-center gap-2 p-3 ${getStatusColors("error").bg} ${getStatusColors("error").text} rounded-lg`}
        >
          <AlertCircle className="w-5 h-5 flex-shrink-0" />
          <span className="text-sm">{error}</span>
        </div>
      )}

      {/* Filters */}
      <ContextFilters
        filters={filters}
        onFiltersChange={setFilters}
        categories={categories}
        resultCount={filteredContexts.length}
        totalCount={contexts.length}
      />

      {/* Content */}
      {loading && contexts.length === 0 ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="w-8 h-8 animate-spin text-primary" />
        </div>
      ) : filteredContexts.length === 0 ? (
        <div className="text-center py-12 text-muted-foreground">
          {contexts.length === 0 ? (
            <div className="space-y-2">
              <BookOpen className="w-12 h-12 mx-auto opacity-50" />
              <p>No contexts yet. Create your first context to get started!</p>
              <p className="text-xs">
                Contexts provide domain knowledge to AI tasks, helping them understand your project
                better.
              </p>
            </div>
          ) : (
            <p>No contexts match your filters.</p>
          )}
        </div>
      ) : (
        <div className="space-y-6">
          {/* User Contexts */}
          {groupedContexts.user.length > 0 && (
            <div>
              <h3 className="text-sm font-medium text-muted-foreground mb-3 flex items-center gap-2">
                <User className={`w-4 h-4 ${getAccentColors("purple").text}`} />
                User Contexts ({groupedContexts.user.length})
                <span className="text-xs text-muted-foreground/70">
                  &mdash; Available across all projects
                </span>
              </h3>
              <div className="space-y-2">
                {groupedContexts.user.map((context) => (
                  <ContextCard
                    key={context.id}
                    context={context}
                    onEdit={handleEdit}
                    onDelete={handleDelete}
                    onDuplicate={handleDuplicate}
                    onToggleEnabled={handleToggleEnabled}
                    isProcessing={processingId === context.id}
                  />
                ))}
              </div>
            </div>
          )}

          {/* Project Contexts */}
          {groupedContexts.project.length > 0 && (
            <div>
              <h3 className="text-sm font-medium text-muted-foreground mb-3 flex items-center gap-2">
                <FolderOpen className={`w-4 h-4 ${getAccentColors("blue").text}`} />
                Project Contexts ({groupedContexts.project.length})
                <span className="text-xs text-muted-foreground/70">
                  &mdash; Saved with loaded config
                </span>
              </h3>
              <div className="space-y-2">
                {groupedContexts.project.map((context) => (
                  <ContextCard
                    key={context.id}
                    context={context}
                    onEdit={handleEdit}
                    onDelete={handleDelete}
                    onDuplicate={handleDuplicate}
                    onToggleEnabled={handleToggleEnabled}
                    onApproveSync={handleApproveSync}
                    onDismissSync={handleDismissSync}
                    isProcessing={processingId === context.id}
                  />
                ))}
              </div>
            </div>
          )}

          {/* Built-in Contexts */}
          {groupedContexts.builtin.length > 0 && (
            <div>
              <h3 className="text-sm font-medium text-muted-foreground mb-3 flex items-center gap-2">
                <BookOpen className="w-4 h-4 text-muted-foreground" />
                Built-in Contexts ({groupedContexts.builtin.length})
              </h3>
              <div className="space-y-2">
                {groupedContexts.builtin.map((context) => (
                  <ContextCard
                    key={context.id}
                    context={context}
                    onEdit={handleView}
                    onDelete={handleDelete}
                    onDuplicate={handleDuplicate}
                    onToggleEnabled={handleToggleEnabled}
                    isProcessing={processingId === context.id}
                  />
                ))}
              </div>
            </div>
          )}
        </div>
      )}

      {/* Editor Dialog */}
      <ContextEditor
        isOpen={editorState.isOpen}
        onClose={closeEditor}
        context={editorState.context}
        mode={editorState.mode}
        createScope={editorState.createScope}
        categories={categories}
        tags={tags}
        onSave={handleSave}
        isSaving={isSaving}
      />

      {/* Duplicate Scope Selection Dialog */}
      {duplicateTarget?.showDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div className="absolute inset-0 bg-black/50" onClick={() => setDuplicateTarget(null)} />
          <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-sm mx-4 p-6 space-y-4">
            <h3 className="text-lg font-semibold">Duplicate Context</h3>
            <p className="text-sm text-muted-foreground">
              Where would you like to save the duplicate of "{duplicateTarget.context.name}"?
            </p>
            <div className="space-y-2">
              <button
                onClick={() => handleDuplicateConfirm("project")}
                className="w-full flex items-center gap-3 px-4 py-3 bg-muted/50 hover:bg-muted rounded-lg transition-colors"
              >
                <FolderOpen className={`w-5 h-5 ${getAccentColors("blue").text}`} />
                <div className="text-left">
                  <div className="font-medium">Project Scope</div>
                  <div className="text-xs text-muted-foreground">
                    Available only in this project
                  </div>
                </div>
              </button>
              <button
                onClick={() => handleDuplicateConfirm("user")}
                className="w-full flex items-center gap-3 px-4 py-3 bg-muted/50 hover:bg-muted rounded-lg transition-colors"
              >
                <User className={`w-5 h-5 ${getAccentColors("purple").text}`} />
                <div className="text-left">
                  <div className="font-medium">User Scope</div>
                  <div className="text-xs text-muted-foreground">Available across all projects</div>
                </div>
              </button>
            </div>
            <button
              onClick={() => setDuplicateTarget(null)}
              className="w-full px-4 py-2 text-sm text-muted-foreground hover:text-foreground transition-colors"
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

export default ContextList;
