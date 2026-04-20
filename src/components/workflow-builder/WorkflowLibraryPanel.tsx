import React from "react";
import {
  Plus,
  Loader2,
  Search,
  Sparkles,
  Trash2,
  Check,
  Upload,
  ExternalLink,
  Star,
} from "lucide-react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import type { UnifiedWorkflow } from "../../types";
import { getTotalStepCount } from "../../types/unified-workflow";
import { getAccentColors } from "@/design-system";
import { PageTutorialMenu } from "../tutorial";

interface WorkflowLibraryPanelProps {
  filteredWorkflows: UnifiedWorkflow[];
  workflowsLoading: boolean;
  currentWorkflowId: string | undefined;
  searchQuery: string;
  onSearchQueryChange: (query: string) => void;
  showFavoritesOnly: boolean;
  onToggleFavoritesOnly: () => void;
  hasFavorites: boolean;
  isWorkflowSelectionMode: boolean;
  onToggleSelectionMode: () => void;
  selectedWorkflowIds: Set<string>;
  onToggleWorkflowSelection: (id: string) => void;
  onExitSelectionMode: () => void;
  onShowBatchDeleteDialog: () => void;
  onNewWorkflow: () => void;
  onSelectWorkflow: (workflow: UnifiedWorkflow) => void;
  onToggleFavorite: (id: string) => void;
  onImportWorkflow: () => void;
  workflowImportError: string | null;
  onDismissImportError: () => void;
  workflowFileInputRef: React.RefObject<HTMLInputElement | null>;
  onImportFileChange: (event: React.ChangeEvent<HTMLInputElement>) => void;
}

export function WorkflowLibraryPanel({
  filteredWorkflows,
  workflowsLoading,
  currentWorkflowId,
  searchQuery,
  onSearchQueryChange,
  showFavoritesOnly,
  onToggleFavoritesOnly,
  hasFavorites,
  isWorkflowSelectionMode,
  onToggleSelectionMode,
  selectedWorkflowIds,
  onToggleWorkflowSelection,
  onExitSelectionMode,
  onShowBatchDeleteDialog,
  onNewWorkflow,
  onSelectWorkflow,
  onToggleFavorite,
  onImportWorkflow: _onImportWorkflow,
  workflowImportError,
  onDismissImportError,
  workflowFileInputRef,
  onImportFileChange,
}: WorkflowLibraryPanelProps) {
  const accentColors = getAccentColors("green");
  const getStepCount = getTotalStepCount;

  return (
    <div className="w-80 border-r border-border flex flex-col bg-card">
      <div className="p-4 border-b border-border">
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-lg font-semibold flex items-center gap-2">
            <Sparkles className="w-5 h-5" style={{ color: accentColors.bgSolid }} />
            Workflows
          </h2>
          <div className="flex items-center gap-1">
            <DropdownMenu.Root>
              <DropdownMenu.Trigger asChild>
                <button
                  className="flex items-center justify-center w-7 h-7 rounded-md hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
                  title="Add or import workflow"
                >
                  <Plus className="w-4 h-4" />
                </button>
              </DropdownMenu.Trigger>
              <DropdownMenu.Portal>
                <DropdownMenu.Content
                  className="min-w-[180px] bg-card/95 backdrop-blur rounded-lg shadow-xl p-1 animate-slideDown z-50"
                  sideOffset={5}
                  align="end"
                >
                  <DropdownMenu.Item
                    className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors"
                    onSelect={onNewWorkflow}
                  >
                    <Plus className="w-3.5 h-3.5 text-muted-foreground" />
                    <span className="flex-1">New Workflow</span>
                  </DropdownMenu.Item>
                  <DropdownMenu.Item
                    className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors disabled:opacity-50"
                    disabled={workflowsLoading}
                    onSelect={() => workflowFileInputRef.current?.click()}
                  >
                    <Upload className="w-3.5 h-3.5 text-muted-foreground" />
                    <span className="flex-1">Import from File</span>
                  </DropdownMenu.Item>
                  <DropdownMenu.Separator className="h-px bg-border/50 my-1" />
                  <DropdownMenu.Item
                    className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors"
                    asChild
                  >
                    <a
                      href="http://localhost:3001/build/workflows"
                      target="_blank"
                      rel="noopener noreferrer"
                      className="flex items-center gap-2 px-3 py-2 text-xs rounded-md no-underline text-inherit hover:bg-muted/50 transition-colors"
                    >
                      <ExternalLink className="w-3.5 h-3.5 text-indigo-400" />
                      <span className="flex-1">Open in Web</span>
                    </a>
                  </DropdownMenu.Item>
                </DropdownMenu.Content>
              </DropdownMenu.Portal>
            </DropdownMenu.Root>
            <input
              ref={workflowFileInputRef}
              type="file"
              accept=".json"
              className="hidden"
              onChange={onImportFileChange}
            />
            <button
              onClick={onToggleSelectionMode}
              className={`flex items-center justify-center w-7 h-7 rounded-md transition-colors ${
                isWorkflowSelectionMode
                  ? "bg-red-500/20 text-red-400"
                  : "text-muted-foreground hover:text-red-400 hover:bg-muted"
              }`}
              title={isWorkflowSelectionMode ? "Cancel selection" : "Select workflows to delete"}
            >
              <Trash2 className="w-3.5 h-3.5" />
            </button>
            <PageTutorialMenu page="unified-workflow-builder" variant="compact" />
          </div>
        </div>

        {workflowImportError && (
          <div className="mx-4 mb-2 p-2 bg-red-500/10 border border-red-500/30 rounded-md text-red-400 text-xs flex items-center justify-between">
            <span>{workflowImportError}</span>
            <button onClick={onDismissImportError} className="p-0.5 hover:bg-red-500/20 rounded">
              <span className="text-red-400">&times;</span>
            </button>
          </div>
        )}

        <div className="relative">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-muted-foreground" />
          <input
            type="text"
            placeholder="Search workflows..."
            value={searchQuery}
            onChange={(e) => onSearchQueryChange(e.target.value)}
            className="w-full pl-9 pr-3 py-2 bg-muted border border-border rounded-lg text-sm focus:outline-hidden focus:border-border"
          />
        </div>
        {hasFavorites && (
          <button
            onClick={onToggleFavoritesOnly}
            className={`flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium transition-colors ${
              showFavoritesOnly
                ? "bg-amber-500/20 text-amber-400 border border-amber-500/50"
                : "bg-white/5 text-muted-foreground border border-transparent hover:bg-white/10"
            }`}
          >
            <Star className={`w-3 h-3 ${showFavoritesOnly ? "fill-amber-400" : ""}`} />
            Favorites
          </button>
        )}
      </div>

      {isWorkflowSelectionMode && (
        <div className="flex items-center justify-between px-4 py-2 bg-red-500/10 border-b border-red-500/30">
          <span className="text-sm text-red-400">{selectedWorkflowIds.size} selected</span>
          <div className="flex items-center gap-2">
            {selectedWorkflowIds.size > 0 && (
              <button
                onClick={onShowBatchDeleteDialog}
                className="flex items-center gap-1 px-3 py-1 text-sm font-medium bg-red-600 hover:bg-red-500 text-white rounded-md transition-colors"
              >
                <Trash2 className="w-3.5 h-3.5" />
                Delete
              </button>
            )}
            <button
              onClick={onExitSelectionMode}
              className="px-3 py-1 text-sm text-muted-foreground hover:text-foreground transition-colors"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-2">
        {workflowsLoading ? (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="w-6 h-6 animate-spin text-muted-foreground" />
          </div>
        ) : filteredWorkflows.length === 0 ? (
          <div className="text-center py-8 text-muted-foreground">
            <Sparkles className="w-8 h-8 mx-auto mb-2 opacity-50" />
            <p className="text-sm">No workflows found</p>
          </div>
        ) : (
          <div className="space-y-1">
            {filteredWorkflows.map((workflow) => (
              <button
                key={workflow.id}
                aria-label={`${isWorkflowSelectionMode ? "Select" : "Open"} workflow: ${workflow.name}`}
                onClick={() => {
                  if (isWorkflowSelectionMode) {
                    onToggleWorkflowSelection(workflow.id);
                  } else {
                    onSelectWorkflow(workflow);
                  }
                }}
                className={`w-full text-left p-3 rounded-lg transition-colors flex items-start gap-3 ${
                  isWorkflowSelectionMode && selectedWorkflowIds.has(workflow.id)
                    ? "bg-red-500/20 border border-red-500/50"
                    : currentWorkflowId === workflow.id
                      ? "bg-muted/80"
                      : "hover:bg-muted"
                } ${isWorkflowSelectionMode ? "border" : ""} ${
                  isWorkflowSelectionMode && !selectedWorkflowIds.has(workflow.id)
                    ? "border-transparent"
                    : ""
                }`}
              >
                {isWorkflowSelectionMode && (
                  <div
                    className={`shrink-0 w-5 h-5 mt-0.5 rounded border-2 flex items-center justify-center transition-colors ${
                      selectedWorkflowIds.has(workflow.id)
                        ? "bg-red-500 border-red-500"
                        : "border-muted-foreground"
                    }`}
                  >
                    {selectedWorkflowIds.has(workflow.id) && (
                      <Check className="w-3 h-3 text-white" />
                    )}
                  </div>
                )}
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-1.5">
                    {!isWorkflowSelectionMode && (
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          onToggleFavorite(workflow.id);
                        }}
                        className="shrink-0 p-0.5 rounded transition-colors hover:bg-white/10"
                        title={workflow.isFavorite ? "Remove from favorites" : "Add to favorites"}
                      >
                        <Star
                          className={`w-3.5 h-3.5 ${workflow.isFavorite ? "text-amber-400 fill-amber-400" : "text-muted-foreground"}`}
                        />
                      </button>
                    )}
                    <div className="font-medium text-sm truncate">{workflow.name}</div>
                  </div>
                  {workflow.description && (
                    <div className="text-xs text-muted-foreground truncate mt-0.5">
                      {workflow.description}
                    </div>
                  )}
                  <div className="flex items-center gap-2 mt-1.5">
                    <span className="text-xs text-muted-foreground">
                      {getStepCount(workflow)} steps
                    </span>
                    {workflow.category && (
                      <span className="text-xs px-1.5 py-0.5 rounded bg-muted text-muted-foreground">
                        {workflow.category}
                      </span>
                    )}
                  </div>
                </div>
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
