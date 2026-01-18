/**
 * AiGenerateWorkflowModal.tsx
 *
 * Modal dialog for generating a workflow from natural language using AI.
 */

import { useState, useCallback } from "react";
import { Sparkles, X, Loader2, Check, AlertCircle, ChevronDown, ChevronUp } from "lucide-react";
import type { UnifiedWorkflow } from "../../types";
import { getAccentColors } from "@/design-system";

const API_BASE = "http://localhost:9876";

interface GenerateWorkflowResponse {
  workflow: UnifiedWorkflow | null;
  validation_errors: string[];
  success: boolean;
  error: string | null;
  model_used: string | null;
}

interface AiGenerateWorkflowModalProps {
  /** Whether the modal is open */
  isOpen: boolean;
  /** Called when the modal is closed */
  onClose: () => void;
  /** Called when a workflow is generated and user wants to load it */
  onWorkflowGenerated: (workflow: UnifiedWorkflow) => void;
}

export function AiGenerateWorkflowModal({
  isOpen,
  onClose,
  onWorkflowGenerated,
}: AiGenerateWorkflowModalProps) {
  const [description, setDescription] = useState("");
  const [category, setCategory] = useState("");
  const [tags, setTags] = useState("");
  const [isGenerating, setIsGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [generatedWorkflow, setGeneratedWorkflow] = useState<UnifiedWorkflow | null>(null);
  const [validationErrors, setValidationErrors] = useState<string[]>([]);
  const [showPreview, setShowPreview] = useState(false);

  const accentColors = getAccentColors("blue");

  const handleGenerate = useCallback(async () => {
    if (!description.trim()) {
      setError("Please provide a description of what the workflow should do.");
      return;
    }

    setIsGenerating(true);
    setError(null);
    setGeneratedWorkflow(null);
    setValidationErrors([]);

    try {
      const response = await fetch(`${API_BASE}/unified-workflows/generate`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          description: description.trim(),
          category: category.trim() || undefined,
          tags: tags
            .split(",")
            .map((t) => t.trim())
            .filter(Boolean),
        }),
      });

      const result = await response.json();

      if (!response.ok) {
        throw new Error(result.error || `HTTP ${response.status}`);
      }

      // Extract the inner response data
      const data: GenerateWorkflowResponse = result.data || result;

      if (data.success && data.workflow) {
        setGeneratedWorkflow(data.workflow);
        setValidationErrors(data.validation_errors || []);
        setShowPreview(true);
      } else {
        setError(data.error || "Failed to generate workflow. Please try again.");
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to generate workflow");
    } finally {
      setIsGenerating(false);
    }
  }, [description, category, tags]);

  const handleLoadWorkflow = useCallback(() => {
    if (generatedWorkflow) {
      onWorkflowGenerated(generatedWorkflow);
      handleClose();
    }
  }, [generatedWorkflow, onWorkflowGenerated]);

  const handleClose = useCallback(() => {
    setDescription("");
    setCategory("");
    setTags("");
    setError(null);
    setGeneratedWorkflow(null);
    setValidationErrors([]);
    setShowPreview(false);
    onClose();
  }, [onClose]);

  const handleRegenerate = useCallback(() => {
    setGeneratedWorkflow(null);
    setValidationErrors([]);
    setShowPreview(false);
    handleGenerate();
  }, [handleGenerate]);

  if (!isOpen) return null;

  const getStepCount = (workflow: UnifiedWorkflow): number => {
    return (
      (workflow.setup_steps?.length || 0) +
      (workflow.verification_steps?.length || 0) +
      (workflow.agentic_steps?.length || 0) +
      (workflow.completion_steps?.length || 0)
    );
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={handleClose} />

      {/* Dialog */}
      <div className="relative bg-card border border-border rounded-lg shadow-xl w-full max-w-2xl mx-4 max-h-[85vh] flex flex-col overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-border">
          <div className="flex items-center gap-2">
            <Sparkles className={`w-5 h-5 ${accentColors.text}`} />
            <h3 className="text-lg font-semibold">Generate Workflow with AI</h3>
          </div>
          <button
            onClick={handleClose}
            className="p-1 text-muted-foreground hover:text-foreground transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-6 space-y-4">
          {/* Description Input */}
          <div>
            <label className="block text-sm font-medium text-foreground mb-2">
              What should the workflow do?
            </label>
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Describe what you want the workflow to accomplish. For example: 'Run TypeScript type checking and fix any errors automatically' or 'Build a React app and run Playwright tests, fixing failures'"
              rows={4}
              className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary resize-none"
              disabled={isGenerating}
              autoFocus
            />
          </div>

          {/* Optional Fields */}
          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="block text-sm font-medium text-muted-foreground mb-1">
                Category (optional)
              </label>
              <input
                type="text"
                value={category}
                onChange={(e) => setCategory(e.target.value)}
                placeholder="e.g., testing, deployment"
                className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                disabled={isGenerating}
              />
            </div>
            <div>
              <label className="block text-sm font-medium text-muted-foreground mb-1">
                Tags (optional)
              </label>
              <input
                type="text"
                value={tags}
                onChange={(e) => setTags(e.target.value)}
                placeholder="e.g., typescript, react"
                className="w-full px-3 py-2 bg-background border border-border rounded-md text-sm focus:outline-none focus:ring-2 focus:ring-primary"
                disabled={isGenerating}
              />
              <p className="text-xs text-muted-foreground mt-1">Comma-separated</p>
            </div>
          </div>

          {/* Error Display */}
          {error && (
            <div className="flex items-start gap-2 p-3 bg-red-500/10 border border-red-500/30 rounded-md">
              <AlertCircle className="w-5 h-5 text-red-500 flex-shrink-0 mt-0.5" />
              <div className="text-sm text-red-400">{error}</div>
            </div>
          )}

          {/* Generated Workflow Preview */}
          {generatedWorkflow && (
            <div className="space-y-3">
              <div className="flex items-center justify-between">
                <h4 className="text-sm font-medium text-foreground">Generated Workflow</h4>
                <button
                  onClick={() => setShowPreview(!showPreview)}
                  className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
                >
                  {showPreview ? (
                    <>
                      <ChevronUp className="w-4 h-4" />
                      Hide Preview
                    </>
                  ) : (
                    <>
                      <ChevronDown className="w-4 h-4" />
                      Show Preview
                    </>
                  )}
                </button>
              </div>

              {/* Workflow Summary */}
              <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-md">
                <div className="flex items-center gap-2 mb-2">
                  <Check className="w-4 h-4 text-green-500" />
                  <span className="font-medium text-green-400">{generatedWorkflow.name}</span>
                </div>
                <p className="text-sm text-muted-foreground">{generatedWorkflow.description}</p>
                <div className="flex items-center gap-4 mt-2 text-xs text-muted-foreground">
                  <span>{getStepCount(generatedWorkflow)} steps</span>
                  <span>Setup: {generatedWorkflow.setup_steps?.length || 0}</span>
                  <span>Verification: {generatedWorkflow.verification_steps?.length || 0}</span>
                  <span>Agentic: {generatedWorkflow.agentic_steps?.length || 0}</span>
                  <span>Completion: {generatedWorkflow.completion_steps?.length || 0}</span>
                </div>
              </div>

              {/* Validation Warnings */}
              {validationErrors.length > 0 && (
                <div className="p-3 bg-yellow-500/10 border border-yellow-500/30 rounded-md">
                  <div className="flex items-center gap-2 mb-2">
                    <AlertCircle className="w-4 h-4 text-yellow-500" />
                    <span className="text-sm font-medium text-yellow-400">Validation Warnings</span>
                  </div>
                  <ul className="text-xs text-yellow-400/80 space-y-1 ml-6 list-disc">
                    {validationErrors.map((err, i) => (
                      <li key={i}>{err}</li>
                    ))}
                  </ul>
                </div>
              )}

              {/* JSON Preview */}
              {showPreview && (
                <div className="border border-border rounded-md overflow-hidden">
                  <div className="px-3 py-2 bg-muted text-xs font-medium text-muted-foreground">
                    JSON Preview
                  </div>
                  <pre className="p-3 text-xs overflow-x-auto max-h-60 bg-background/50 text-foreground/80">
                    {JSON.stringify(generatedWorkflow, null, 2)}
                  </pre>
                </div>
              )}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex gap-3 px-6 py-4 border-t border-border">
          <button
            onClick={handleClose}
            className="flex-1 px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 transition-colors"
          >
            Cancel
          </button>

          {generatedWorkflow ? (
            <>
              <button
                onClick={handleRegenerate}
                disabled={isGenerating}
                className="px-4 py-2 bg-muted text-foreground rounded-md font-medium hover:bg-muted/80 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
              >
                Regenerate
              </button>
              <button
                onClick={handleLoadWorkflow}
                className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${accentColors.bgSolid} text-white rounded-md font-medium hover:opacity-90 transition-colors`}
              >
                <Check className="w-4 h-4" />
                Load into Builder
              </button>
            </>
          ) : (
            <button
              onClick={handleGenerate}
              disabled={isGenerating || !description.trim()}
              className={`flex-1 flex items-center justify-center gap-2 px-4 py-2 ${accentColors.bgSolid} text-white rounded-md font-medium hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed transition-colors`}
            >
              {isGenerating ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Generating...
                </>
              ) : (
                <>
                  <Sparkles className="w-4 h-4" />
                  Generate Workflow
                </>
              )}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

export default AiGenerateWorkflowModal;
