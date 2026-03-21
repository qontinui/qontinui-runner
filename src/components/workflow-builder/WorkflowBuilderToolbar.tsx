import React from "react";
import {
  Save,
  Play,
  Square,
  Settings,
  Loader2,
  Download,
  Upload,
  ChevronDown,
  MoreHorizontal,
  PanelLeft,
  ShieldCheck,
  Layers,
} from "lucide-react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { EditableWorkflowTitle } from "./SettingsPanel";

interface WorkflowBuilderToolbarProps {
  workflowName: string;
  hasUnsavedChanges: boolean;
  isSaving: boolean;
  isExecuting: boolean;
  isEmpty: boolean;
  showLibrary: boolean;
  showSettings: boolean;
  showConstraints: boolean;
  workflowId: string | undefined;
  isExportingWorkflow: boolean;
  isExportingSkills: boolean;
  isImportingSkills: boolean;
  originalWorkflow: unknown;
  onToggleLibrary: () => void;
  onToggleSettings: () => void;
  onToggleConstraints: () => void;
  onNameChange: (name: string) => void;
  onSave: () => void;
  onRun: () => void;
  onShowRunOptions: () => void;
  onStop: () => void;
  onExportWorkflow: () => void;
  onOpenExportDialog: () => void;
  onImportSkillsClick: () => void;
  onShowCompositionBuilder: () => void;
  skillFileInputRef: React.RefObject<HTMLInputElement | null>;
  onImportFileSelected: (event: React.ChangeEvent<HTMLInputElement>) => void;
}

export function WorkflowBuilderToolbar({
  workflowName,
  hasUnsavedChanges,
  isSaving,
  isExecuting,
  isEmpty,
  showLibrary,
  showSettings,
  showConstraints,
  workflowId,
  isExportingWorkflow,
  isExportingSkills,
  isImportingSkills,
  originalWorkflow,
  onToggleLibrary,
  onToggleSettings,
  onToggleConstraints,
  onNameChange,
  onSave,
  onRun,
  onShowRunOptions,
  onStop,
  onExportWorkflow,
  onOpenExportDialog,
  onImportSkillsClick,
  onShowCompositionBuilder,
  skillFileInputRef,
  onImportFileSelected,
}: WorkflowBuilderToolbarProps) {
  return (
    <div className="flex items-center justify-between p-4 border-b border-zinc-700">
      <div className="flex items-center gap-3">
        <button
          onClick={onToggleLibrary}
          className={`p-1.5 rounded-md transition-colors ${
            showLibrary
              ? "bg-zinc-700 text-zinc-200"
              : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800"
          }`}
          title={showLibrary ? "Hide workflow library" : "Show workflow library"}
        >
          <PanelLeft className="w-4 h-4" />
        </button>
        <button
          data-tutorial-id="workflow-settings"
          onClick={onToggleSettings}
          className={`p-1.5 rounded-md transition-colors ${
            showSettings
              ? "bg-zinc-700 text-zinc-200"
              : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800"
          }`}
          title="Settings"
        >
          <Settings className="w-4 h-4" />
        </button>
        <button
          onClick={onToggleConstraints}
          className={`p-1.5 rounded-md transition-colors ${
            showConstraints
              ? "bg-zinc-700 text-zinc-200"
              : "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800"
          }`}
          title="Constraints"
        >
          <ShieldCheck className="w-4 h-4" />
        </button>
        <EditableWorkflowTitle
          name={workflowName}
          onChange={onNameChange}
          hasUnsavedChanges={hasUnsavedChanges}
        />
      </div>

      <div className="flex items-center gap-2">
        <button
          data-tutorial-id="save-workflow-button"
          onClick={onSave}
          disabled={isSaving || (!hasUnsavedChanges && !!originalWorkflow)}
          className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-200 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          title="Save workflow"
        >
          {isSaving ? <Loader2 className="w-4 h-4 animate-spin" /> : <Save className="w-4 h-4" />}
          <span className="text-sm">{isSaving ? "Saving..." : "Save"}</span>
        </button>

        <DropdownMenu.Root>
          <DropdownMenu.Trigger asChild>
            <button
              className="flex items-center justify-center p-1.5 rounded-md text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800 transition-colors"
              title="More actions"
            >
              <MoreHorizontal className="w-4 h-4" />
            </button>
          </DropdownMenu.Trigger>
          <DropdownMenu.Portal>
            <DropdownMenu.Content
              className="min-w-[200px] bg-card/95 backdrop-blur rounded-lg shadow-xl p-1 animate-slideDown z-50"
              sideOffset={5}
              align="end"
            >
              <DropdownMenu.Item
                className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors disabled:opacity-50"
                disabled={!workflowId || isExportingWorkflow}
                onSelect={onExportWorkflow}
              >
                <Download className="w-3.5 h-3.5 text-muted-foreground" />
                <span className="flex-1">Export Workflow</span>
              </DropdownMenu.Item>
              <DropdownMenu.Separator className="h-px bg-border/50 my-1" />
              <DropdownMenu.Item
                className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors disabled:opacity-50"
                disabled={isExportingSkills}
                onSelect={onOpenExportDialog}
              >
                <Download className="w-3.5 h-3.5 text-muted-foreground" />
                <span className="flex-1">Export Skills</span>
              </DropdownMenu.Item>
              <DropdownMenu.Item
                className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors disabled:opacity-50"
                disabled={isImportingSkills}
                onSelect={onImportSkillsClick}
              >
                <Upload className="w-3.5 h-3.5 text-muted-foreground" />
                <span className="flex-1">Import Skills</span>
              </DropdownMenu.Item>
              <DropdownMenu.Separator className="h-px bg-border/50 my-1" />
              <DropdownMenu.Item
                className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-hidden hover:bg-muted/50 transition-colors"
                onSelect={onShowCompositionBuilder}
              >
                <Layers className="w-3.5 h-3.5 text-muted-foreground" />
                <span className="flex-1">Create Composition</span>
              </DropdownMenu.Item>
            </DropdownMenu.Content>
          </DropdownMenu.Portal>
        </DropdownMenu.Root>
        <input
          ref={skillFileInputRef}
          type="file"
          accept=".json"
          className="hidden"
          onChange={onImportFileSelected}
        />

        {isExecuting ? (
          <button
            onClick={onStop}
            className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-red-600 hover:bg-red-500 text-white transition-colors"
            title="Stop execution"
          >
            <Square className="w-4 h-4" />
            <span className="text-sm">Stop</span>
          </button>
        ) : (
          <div className="flex items-stretch">
            <button
              onClick={onRun}
              disabled={isEmpty}
              className="flex items-center gap-2 px-3 py-1.5 rounded-l-md bg-blue-600 hover:bg-blue-500 text-white transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              title={isEmpty ? "Add steps to run this workflow" : "Run workflow"}
            >
              <Play className="w-4 h-4" />
              <span className="text-sm">Run</span>
            </button>
            <button
              onClick={onShowRunOptions}
              className="flex items-center px-2 py-1.5 rounded-r-md bg-blue-700 hover:bg-blue-600 text-white border-l border-blue-500/50 transition-colors"
              title="Run with options (architecture, worktree)"
            >
              <ChevronDown className="w-4 h-4" />
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
