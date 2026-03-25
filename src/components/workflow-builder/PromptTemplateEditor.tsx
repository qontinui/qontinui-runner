/**
 * PromptTemplateEditor.tsx
 *
 * Component for viewing and editing the developer prompt template in the Unified Workflow Builder.
 * Allows customization at both global and per-workflow levels.
 *
 * Based on the legacy AI Builder implementation.
 */

import React, { useState, useCallback, useEffect } from "react";
import {
  FileCode,
  RotateCcw,
  Save,
  X,
  ChevronDown,
  ChevronUp,
  Info,
  Globe,
  File,
  CheckCircle,
  History,
} from "lucide-react";
import { PromptVersionHistory } from "../prompt-versions";
import {
  DEFAULT_UNIFIED_PROMPT_TEMPLATE,
  TEMPLATE_VARIABLES,
  getGlobalPromptTemplate,
  saveGlobalPromptTemplate,
  resetGlobalPromptTemplate,
  isUsingGlobalCustomTemplate,
} from "./prompt-template-constants";

export type PromptTemplateScope = "global" | "workflow";

interface PromptTemplateEditorProps {
  /** The current workflow's prompt template (null means use global) */
  workflowTemplate: string | null | undefined;
  /** Callback to update the workflow's prompt template */
  onWorkflowTemplateChange: (template: string | null) => void;
  /** Whether the workflow has agentic steps (template only makes sense for agentic workflows) */
  hasAgenticSteps?: boolean;
  /** Optional template ID to enable version history */
  templateId?: string | null;
}

export function PromptTemplateEditor({
  workflowTemplate,
  onWorkflowTemplateChange,
  hasAgenticSteps = false,
  templateId = null,
}: PromptTemplateEditorProps) {
  // Track which scope we're editing
  const [activeScope, setActiveScope] = useState<PromptTemplateScope>(
    workflowTemplate ? "workflow" : "global",
  );

  // Editor content state
  const [editorContent, setEditorContent] = useState("");

  // Expanded state for the panel
  const [isExpanded, setIsExpanded] = useState(false);

  // Show variables help
  const [showVariables, setShowVariables] = useState(false);

  // Success message
  const [successMessage, setSuccessMessage] = useState<string | null>(null);

  // Track if there are unsaved changes
  const [hasChanges, setHasChanges] = useState(false);

  // Version history panel
  const [showVersionHistory, setShowVersionHistory] = useState(false);

  // Get the effective template being used
  const _getEffectiveTemplate = useCallback((): string => {
    if (workflowTemplate) {
      return workflowTemplate;
    }
    return getGlobalPromptTemplate();
  }, [workflowTemplate]);

  // Initialize editor content based on scope
  useEffect(() => {
    if (activeScope === "workflow") {
      setEditorContent(workflowTemplate || getGlobalPromptTemplate());
    } else {
      setEditorContent(getGlobalPromptTemplate());
    }
    setHasChanges(false);
  }, [activeScope, workflowTemplate]);

  // Handle scope change
  const handleScopeChange = useCallback((scope: PromptTemplateScope) => {
    setActiveScope(scope);
    setSuccessMessage(null);
  }, []);

  // Handle editor content change
  const handleContentChange = useCallback((value: string) => {
    setEditorContent(value);
    setHasChanges(true);
    setSuccessMessage(null);
  }, []);

  // Save template
  const handleSave = useCallback(() => {
    if (activeScope === "workflow") {
      // Save to workflow
      onWorkflowTemplateChange(editorContent);
      setSuccessMessage("Workflow template saved!");
    } else {
      // Save to global
      saveGlobalPromptTemplate(editorContent);
      setSuccessMessage("Global template saved!");
    }
    setHasChanges(false);
    setTimeout(() => setSuccessMessage(null), 3000);
  }, [activeScope, editorContent, onWorkflowTemplateChange]);

  // Reset template
  const handleReset = useCallback(() => {
    if (activeScope === "workflow") {
      // Reset workflow to use global
      onWorkflowTemplateChange(null);
      setEditorContent(getGlobalPromptTemplate());
      setSuccessMessage("Workflow template reset to global!");
    } else {
      // Reset global to default
      resetGlobalPromptTemplate();
      setEditorContent(DEFAULT_UNIFIED_PROMPT_TEMPLATE);
      setSuccessMessage("Global template reset to default!");
    }
    setHasChanges(false);
    setTimeout(() => setSuccessMessage(null), 3000);
  }, [activeScope, onWorkflowTemplateChange]);

  // Use global template for workflow (remove custom)
  const handleUseGlobal = useCallback(() => {
    onWorkflowTemplateChange(null);
    setActiveScope("global");
    setEditorContent(getGlobalPromptTemplate());
    setSuccessMessage("Workflow now uses global template");
    setHasChanges(false);
    setTimeout(() => setSuccessMessage(null), 3000);
  }, [onWorkflowTemplateChange]);

  // Don't render if no agentic steps
  if (!hasAgenticSteps) {
    return null;
  }

  const isUsingWorkflowTemplate = workflowTemplate !== null && workflowTemplate !== undefined;
  const isUsingCustomGlobal = isUsingGlobalCustomTemplate();

  return (
    <div className="border border-zinc-700 rounded-lg overflow-hidden">
      {/* Header */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="w-full flex items-center justify-between p-3 bg-zinc-800/50 hover:bg-zinc-800 transition-colors"
      >
        <div className="flex items-center gap-2">
          <FileCode className="w-4 h-4 text-amber-400" />
          <span className="font-medium text-sm">Developer Prompt Template</span>
          {isUsingWorkflowTemplate && (
            <span className="text-xs px-2 py-0.5 bg-amber-500/20 text-amber-400 rounded">
              Custom
            </span>
          )}
          {!isUsingWorkflowTemplate && isUsingCustomGlobal && (
            <span className="text-xs px-2 py-0.5 bg-blue-500/20 text-blue-400 rounded">
              Global Custom
            </span>
          )}
        </div>
        {isExpanded ? (
          <ChevronUp className="w-4 h-4 text-zinc-400" />
        ) : (
          <ChevronDown className="w-4 h-4 text-zinc-400" />
        )}
      </button>

      {/* Content */}
      {isExpanded && (
        <div className="p-4 space-y-4">
          {/* Info text */}
          <p className="text-xs text-zinc-400">
            Customize the system prompt template used when running agentic workflows. This template
            defines how the AI operates in the iterative feedback loop.
          </p>

          {/* Scope tabs */}
          <div className="flex items-center gap-2">
            <button
              onClick={() => handleScopeChange("global")}
              className={`flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm transition-colors ${
                activeScope === "global"
                  ? "bg-blue-500/20 text-blue-400 border border-blue-500/30"
                  : "bg-zinc-800 text-zinc-400 hover:text-zinc-200 border border-transparent"
              }`}
            >
              <Globe className="w-3.5 h-3.5" />
              <span>Global</span>
            </button>
            <button
              onClick={() => handleScopeChange("workflow")}
              className={`flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm transition-colors ${
                activeScope === "workflow"
                  ? "bg-amber-500/20 text-amber-400 border border-amber-500/30"
                  : "bg-zinc-800 text-zinc-400 hover:text-zinc-200 border border-transparent"
              }`}
            >
              <File className="w-3.5 h-3.5" />
              <span>This Workflow</span>
              {isUsingWorkflowTemplate && <CheckCircle className="w-3.5 h-3.5 text-amber-400" />}
            </button>
          </div>

          {/* Scope description */}
          <div className="text-xs text-zinc-500 bg-zinc-800/30 rounded-md p-2">
            {activeScope === "global" ? (
              <>
                <strong className="text-zinc-400">Global template:</strong> Used by all workflows
                that don't have a custom template. Changes here affect all workflows.
              </>
            ) : (
              <>
                <strong className="text-zinc-400">Workflow template:</strong>{" "}
                {isUsingWorkflowTemplate
                  ? "This workflow has a custom template that overrides the global one."
                  : "This workflow uses the global template. Edit here to create a custom template for this workflow only."}
              </>
            )}
          </div>

          {/* Variables help */}
          <div>
            <button
              onClick={() => setShowVariables(!showVariables)}
              className="flex items-center gap-1.5 text-xs text-zinc-400 hover:text-zinc-300"
            >
              <Info className="w-3.5 h-3.5" />
              <span>Available Template Variables</span>
              {showVariables ? (
                <ChevronUp className="w-3 h-3" />
              ) : (
                <ChevronDown className="w-3 h-3" />
              )}
            </button>
            {showVariables && (
              <div className="mt-2 p-2 bg-zinc-800/50 rounded-md text-xs space-y-1">
                {TEMPLATE_VARIABLES.map((v) => (
                  <div key={v.name} className="flex items-start gap-2">
                    <code className="text-amber-400 whitespace-nowrap">{v.name}</code>
                    <span className="text-zinc-400">{v.description}</span>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* Editor */}
          <div>
            <textarea
              value={editorContent}
              onChange={(e) => handleContentChange(e.target.value)}
              className="w-full h-64 px-3 py-2 bg-zinc-900 border border-zinc-700 rounded-md text-sm text-zinc-200 font-mono resize-y focus:outline-hidden focus:ring-2 focus:ring-blue-500/50"
              placeholder="Enter your prompt template..."
            />
            <div className="mt-1 flex items-center justify-between text-xs text-zinc-500">
              <span data-content-role="metric" data-content-label="template character count">
                {editorContent.length} characters
              </span>
              {hasChanges && (
                <span data-content-role="status" className="text-amber-400">
                  Unsaved changes
                </span>
              )}
            </div>
          </div>

          {/* Success message */}
          {successMessage && (
            <div className="flex items-center gap-2 p-2 bg-green-500/20 text-green-400 text-sm rounded-md">
              <CheckCircle className="w-4 h-4" />
              {successMessage}
            </div>
          )}

          {/* Actions */}
          <div className="flex items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              {/* Save button */}
              <button
                onClick={handleSave}
                disabled={!hasChanges}
                className="flex items-center gap-1.5 px-3 py-1.5 bg-blue-600 hover:bg-blue-500 disabled:bg-zinc-700 disabled:text-zinc-500 text-white text-sm rounded-md transition-colors"
              >
                <Save className="w-3.5 h-3.5" />
                Save {activeScope === "global" ? "Global" : "Workflow"}
              </button>

              {/* Reset button */}
              <button
                onClick={handleReset}
                className="flex items-center gap-1.5 px-3 py-1.5 bg-zinc-700 hover:bg-zinc-600 text-zinc-200 text-sm rounded-md transition-colors"
              >
                <RotateCcw className="w-3.5 h-3.5" />
                Reset to {activeScope === "global" ? "Default" : "Global"}
              </button>
            </div>

            <div className="flex items-center gap-2">
              {/* Version History button */}
              {templateId && (
                <button
                  onClick={() => setShowVersionHistory(!showVersionHistory)}
                  className={`flex items-center gap-1.5 px-3 py-1.5 text-sm rounded-md transition-colors ${
                    showVersionHistory
                      ? "bg-blue-500/20 text-blue-400 border border-blue-500/30"
                      : "bg-zinc-700 hover:bg-zinc-600 text-zinc-200"
                  }`}
                >
                  <History className="w-3.5 h-3.5" />
                  Version History
                </button>
              )}

              {/* Use global button (only for workflow scope when there's a custom template) */}
              {activeScope === "workflow" && isUsingWorkflowTemplate && (
                <button
                  onClick={handleUseGlobal}
                  className="flex items-center gap-1.5 px-3 py-1.5 text-zinc-400 hover:text-zinc-200 text-sm transition-colors"
                >
                  <X className="w-3.5 h-3.5" />
                  Remove custom template
                </button>
              )}
            </div>
          </div>

          {/* Version History panel */}
          {showVersionHistory && templateId && (
            <div className="border border-zinc-700 rounded-lg overflow-hidden">
              <PromptVersionHistory templateId={templateId} />
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default PromptTemplateEditor;
