/**
 * Macro Builder
 *
 * Main component for building sequential macro action workflows.
 * Provides a linear step editor for composing CLICK, TYPE, HOTKEY, and GO_TO_STATE actions.
 */

import { useMemo, useState } from "react";
import {
  Play,
  Save,
  Plus,
  AlertCircle,
  CheckCircle,
  XCircle,
  Loader2,
  FileText,
  Search,
  MousePointer2,
  Clock,
  Trash2,
} from "lucide-react";
import { cn } from "../../lib/utils";
import { getStatusColors, getAccentColors } from "@/design-system";

import { MacroBuilderProvider, useMacroBuilder } from "./MacroBuilderContext";
import { useMacroBuilderState } from "./useMacroBuilderState";
import { StepItem } from "./StepItem";
import { AddStepDropdown } from "./AddStepDropdown";
import { StepEditor } from "./StepEditor";
import { SaveMacroDialog } from "./SaveMacroDialog";
import type { MacroBuilderTabProps } from "./types";
import type { SavedMacro } from "../../types/macro";

const accentColors = getAccentColors("cyan");

function formatDate(dateString: string): string {
  const date = new Date(dateString);
  return date.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function MacroBuilderContent() {
  const {
    steps,
    addStep,
    removeStep,
    moveStepUp,
    moveStepDown,
    currentMacroId,
    hasUnsavedChanges,
    formState,
    setFormState,
    editingStepId,
    setEditingStepId,
    setShowSaveDialog,
    handleSaveMacro,
    handleNewMacro,
    configLoaded,
    isRunning,
    runMacro,
    lastResult,
    savedMacros,
    loadMacro,
    deleteMacro,
  } = useMacroBuilder();

  const [searchQuery, setSearchQuery] = useState("");

  const editingStep = useMemo(
    () => steps.find((s) => s.id === editingStepId) || null,
    [steps, editingStepId],
  );

  const filteredMacros = useMemo(() => {
    if (!searchQuery.trim()) return savedMacros;
    const query = searchQuery.toLowerCase();
    return savedMacros.filter(
      (m) =>
        m.name.toLowerCase().includes(query) ||
        (m.description && m.description.toLowerCase().includes(query)),
    );
  }, [savedMacros, searchQuery]);

  const handleDelete = async (e: React.MouseEvent, macro: SavedMacro) => {
    e.stopPropagation();
    if (confirm(`Are you sure you want to delete "${macro.name}"?`)) {
      try {
        await deleteMacro(macro.id);
      } catch (error) {
        console.error("Failed to delete macro:", error);
      }
    }
  };

  return (
    <div className="h-full flex">
      {/* Left Panel - Macro List */}
      <div className="w-80 border-r border-border flex flex-col bg-background">
        {/* Header */}
        <div className={`p-4 border-b ${accentColors.border}`}>
          <div className="flex items-center justify-between mb-3">
            <div className="flex items-center gap-2">
              <MousePointer2 className={`h-5 w-5 ${accentColors.text}`} />
              <h2 className="font-semibold">Macros</h2>
            </div>
            <span className="text-xs text-muted-foreground">
              {filteredMacros.length} macro{filteredMacros.length !== 1 ? "s" : ""}
            </span>
          </div>
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
            <input
              type="text"
              placeholder="Search macros..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-3 py-2 text-sm bg-muted/50 border border-border rounded-md focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>
        </div>

        {/* Create New Button */}
        <div className="p-3 border-b border-border">
          <button
            onClick={handleNewMacro}
            disabled={isRunning}
            className={`w-full flex items-center justify-center gap-2 px-3 py-2 text-sm font-medium ${accentColors.bgSolid} text-white rounded-md hover:opacity-90 transition-opacity disabled:opacity-50 disabled:cursor-not-allowed`}
          >
            <Plus className="h-4 w-4" />
            Create New Macro
          </button>
        </div>

        {/* Macro List */}
        <div className="flex-1 overflow-y-auto p-2 space-y-1">
          {filteredMacros.length === 0 ? (
            <div className="text-sm text-muted-foreground text-center py-8">
              {searchQuery ? "No matching macros" : "No saved macros yet"}
            </div>
          ) : (
            filteredMacros.map((macro) => (
              <div
                key={macro.id}
                className={cn(
                  "group p-3 rounded-lg border cursor-pointer transition-colors",
                  currentMacroId === macro.id
                    ? `${accentColors.border} ${accentColors.bg}`
                    : "border-transparent hover:border-border hover:bg-muted/50",
                )}
                onClick={() => loadMacro(macro)}
              >
                <div className="flex items-start justify-between gap-2">
                  <div className="flex-1 min-w-0">
                    <div className="font-medium truncate">{macro.name}</div>
                    {macro.description && (
                      <div className="text-sm text-muted-foreground truncate">
                        {macro.description}
                      </div>
                    )}
                    <div className="flex items-center gap-3 mt-1 text-xs text-muted-foreground">
                      <span>
                        {macro.steps.length} step{macro.steps.length !== 1 ? "s" : ""}
                      </span>
                      {macro.run_count > 0 && (
                        <span className="flex items-center gap-1">
                          <Play className="h-3 w-3" />
                          {macro.run_count}
                        </span>
                      )}
                    </div>
                    <div className="flex items-center gap-1 mt-1 text-xs text-muted-foreground">
                      <Clock className="h-3 w-3" />
                      {formatDate(macro.modified_at)}
                    </div>
                  </div>
                  <button
                    className="p-1.5 rounded hover:bg-muted text-destructive opacity-0 group-hover:opacity-100 transition-opacity"
                    onClick={(e) => handleDelete(e, macro)}
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>
            ))
          )}
        </div>
      </div>

      {/* Main Content */}
      <div className="flex-1 flex flex-col min-w-0">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <div className="flex items-center gap-4">
            <div className="flex items-center gap-2">
              <FileText className="h-5 w-5 text-primary" />
              <input
                type="text"
                value={formState.name}
                onChange={(e) => setFormState((prev) => ({ ...prev, name: e.target.value }))}
                placeholder="Untitled Macro"
                className="h-8 w-48 px-2 bg-background border border-border rounded-md text-sm font-medium focus:outline-none focus:ring-2 focus:ring-primary"
              />
              {hasUnsavedChanges && (
                <span className="px-2 py-0.5 text-xs border border-border rounded-md text-muted-foreground">
                  Unsaved
                </span>
              )}
            </div>
          </div>

          <div className="flex items-center gap-2">
            <button
              onClick={currentMacroId ? handleSaveMacro : () => setShowSaveDialog(true)}
              disabled={isRunning}
              className="flex items-center gap-1 px-3 py-1.5 text-sm font-medium border border-border rounded-md hover:bg-muted transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Save className="h-4 w-4" />
              Save
            </button>
            <button
              onClick={runMacro}
              disabled={isRunning || steps.length === 0}
              className="flex items-center gap-1 px-3 py-1.5 text-sm font-medium bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {isRunning ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                <Play className="h-4 w-4" />
              )}
              Run
            </button>
          </div>
        </div>

        {/* Result Banner */}
        {lastResult && (
          <div
            className={cn(
              "flex items-center gap-2 px-4 py-2 text-sm",
              lastResult.success
                ? `${getStatusColors("success").bg} ${getStatusColors("success").text}`
                : `${getStatusColors("error").bg} ${getStatusColors("error").text}`,
            )}
          >
            {lastResult.success ? (
              <CheckCircle className="h-4 w-4" />
            ) : (
              <XCircle className="h-4 w-4" />
            )}
            {lastResult.message}
          </div>
        )}

        {/* Config Warning */}
        {!configLoaded && (
          <div
            className={`flex items-center gap-2 px-4 py-2 text-sm ${getStatusColors("warning").bg} ${getStatusColors("warning").text}`}
          >
            <AlertCircle className="h-4 w-4" />
            Load a configuration to access images and states for targeting
          </div>
        )}

        {/* Steps Area */}
        <div className="flex-1 overflow-y-auto">
          <div className="p-4 space-y-4">
            {steps.length === 0 ? (
              <div className="border border-border rounded-lg bg-card">
                <div className="flex flex-col items-center justify-center py-12">
                  <div className="text-muted-foreground text-center mb-4">
                    <p className="text-lg font-medium">No steps yet</p>
                    <p className="text-sm">Add steps to build your macro</p>
                  </div>
                  <AddStepDropdown onAddStep={addStep} />
                </div>
              </div>
            ) : (
              <>
                <div className="space-y-2">
                  {steps.map((step, index) => (
                    <StepItem
                      key={step.id}
                      step={step}
                      index={index}
                      isEditing={editingStepId === step.id}
                      isFirst={index === 0}
                      isLast={index === steps.length - 1}
                      onEdit={() => setEditingStepId(step.id)}
                      onRemove={() => removeStep(step.id)}
                      onMoveUp={() => moveStepUp(index)}
                      onMoveDown={() => moveStepDown(index)}
                    />
                  ))}
                </div>
                <AddStepDropdown onAddStep={addStep} disabled={isRunning} />
              </>
            )}
          </div>
        </div>
      </div>

      {/* Step Editor Dialog */}
      <StepEditor
        step={editingStep}
        open={editingStepId !== null}
        onClose={() => setEditingStepId(null)}
      />

      {/* Save Dialog */}
      <SaveMacroDialog />
    </div>
  );
}

export function MacroBuilderTab({ editMacroId }: MacroBuilderTabProps) {
  const state = useMacroBuilderState({ editMacroId });

  return (
    <MacroBuilderProvider value={state}>
      <MacroBuilderContent />
    </MacroBuilderProvider>
  );
}

export default MacroBuilderTab;
