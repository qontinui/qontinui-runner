/**
 * GenerateFromStatesModal.tsx
 *
 * Modal for generating a unified workflow from a loaded state machine configuration.
 * Provides options for customizing the generated workflow.
 */

import React, { useState, useCallback, useEffect } from "react";
import { X, Loader2, Sparkles, AlertCircle, CheckCircle, GitBranch } from "lucide-react";
import { useWorkflowGenerator } from "../../hooks/useWorkflowGenerator";
import type { UnifiedWorkflow } from "../../types/unified-workflow";
import type { GeneratorOptions } from "../../services/WorkflowGeneratorService";

interface GenerateFromStatesModalProps {
  isOpen: boolean;
  onClose: () => void;
  onWorkflowGenerated: (workflow: UnifiedWorkflow) => void;
}

export function GenerateFromStatesModal({
  isOpen,
  onClose,
  onWorkflowGenerated,
}: GenerateFromStatesModalProps) {
  const { generate, isGenerating, error, checkConfigAvailable, getStateCount } =
    useWorkflowGenerator();

  // Form state
  const [workflowName, setWorkflowName] = useState("State Machine Verification");
  const [workflowDescription, setWorkflowDescription] = useState("");
  const [maxIterations, setMaxIterations] = useState(10);
  const [stateTimeout, setStateTimeout] = useState(30);
  const [includeSetupNavigation, setIncludeSetupNavigation] = useState(true);
  const [includeContexts, setIncludeContexts] = useState(true);
  const [includeSummary, setIncludeSummary] = useState(true);

  // Validation state
  const [hasConfig, setHasConfig] = useState(false);
  const [stateCount, setStateCount] = useState(0);
  const [isCheckingConfig, setIsCheckingConfig] = useState(true);

  // Check for available config when modal opens
  useEffect(() => {
    if (isOpen) {
      setIsCheckingConfig(true);
      Promise.all([checkConfigAvailable(), getStateCount()])
        .then(([available, count]) => {
          setHasConfig(available);
          setStateCount(count);
        })
        .finally(() => {
          setIsCheckingConfig(false);
        });
    }
  }, [isOpen, checkConfigAvailable, getStateCount]);

  const handleGenerate = useCallback(async () => {
    const options: GeneratorOptions = {
      workflowName,
      workflowDescription: workflowDescription || undefined,
      maxIterations,
      stateTimeout,
      includeSetupNavigation,
      includeContexts,
      includeSummary,
    };

    const result = await generate(options);

    if (result.success && result.workflow) {
      onWorkflowGenerated(result.workflow);
      onClose();
    }
  }, [
    generate,
    workflowName,
    workflowDescription,
    maxIterations,
    stateTimeout,
    includeSetupNavigation,
    includeContexts,
    includeSummary,
    onWorkflowGenerated,
    onClose,
  ]);

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div
        role="button"
        tabIndex={0}
        aria-label="Close modal"
        className="absolute inset-0 bg-black/60 backdrop-blur-sm"
        onClick={onClose}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") onClose();
        }}
      />

      {/* Modal */}
      <div className="relative w-full max-w-lg bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-700">
          <div className="flex items-center gap-3">
            <div className="p-2 rounded-lg bg-emerald-500/20">
              <GitBranch className="w-5 h-5 text-emerald-400" />
            </div>
            <div>
              <h2 className="text-lg font-semibold text-zinc-100">Generate from State Machine</h2>
              <p className="text-sm text-zinc-400">
                Create a verification workflow from loaded states
              </p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="p-1.5 rounded-md text-zinc-400 hover:text-zinc-200 hover:bg-zinc-800 transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="p-6 space-y-5">
          {/* Config status */}
          {isCheckingConfig ? (
            <div className="flex items-center gap-3 p-4 bg-zinc-800/50 rounded-lg">
              <Loader2 className="w-5 h-5 animate-spin text-zinc-400" />
              <span className="text-zinc-400">Checking loaded configuration...</span>
            </div>
          ) : !hasConfig ? (
            <div className="flex items-start gap-3 p-4 bg-amber-500/10 border border-amber-500/30 rounded-lg">
              <AlertCircle className="w-5 h-5 text-amber-400 shrink-0 mt-0.5" />
              <div>
                <p className="text-amber-400 font-medium">No Configuration Loaded</p>
                <p className="text-sm text-zinc-400 mt-1">
                  Please load a state machine configuration first. Go to the Configuration panel and
                  load a config file exported from qontinui-web.
                </p>
              </div>
            </div>
          ) : (
            <div className="flex items-start gap-3 p-4 bg-emerald-500/10 border border-emerald-500/30 rounded-lg">
              <CheckCircle className="w-5 h-5 text-emerald-400 shrink-0 mt-0.5" />
              <div>
                <p className="text-emerald-400 font-medium">Configuration Ready</p>
                <p className="text-sm text-zinc-400 mt-1">
                  Found <span className="font-medium text-zinc-300">{stateCount} states</span> in
                  the loaded configuration. A verification step will be created for each state.
                </p>
              </div>
            </div>
          )}

          {/* Form fields */}
          {hasConfig && !isCheckingConfig && (
            <>
              {/* Workflow name */}
              <div>
                <label
                  htmlFor="generate-workflow-name"
                  className="block text-sm font-medium text-zinc-400 mb-1.5"
                >
                  Workflow Name
                </label>
                <input
                  id="generate-workflow-name"
                  type="text"
                  value={workflowName}
                  onChange={(e) => setWorkflowName(e.target.value)}
                  placeholder="Enter workflow name..."
                  className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
                />
              </div>

              {/* Description */}
              <div>
                <label
                  htmlFor="generate-workflow-description"
                  className="block text-sm font-medium text-zinc-400 mb-1.5"
                >
                  Description (optional)
                </label>
                <textarea
                  id="generate-workflow-description"
                  value={workflowDescription}
                  onChange={(e) => setWorkflowDescription(e.target.value)}
                  placeholder="What does this workflow verify?"
                  rows={2}
                  className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 placeholder-zinc-500 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50 resize-none"
                />
              </div>

              {/* Settings grid */}
              <div className="grid grid-cols-2 gap-4">
                {/* Max iterations */}
                <div>
                  <label
                    htmlFor="generate-max-iterations"
                    className="block text-sm font-medium text-zinc-400 mb-1.5"
                  >
                    Max Iterations
                  </label>
                  <input
                    id="generate-max-iterations"
                    type="number"
                    value={maxIterations}
                    onChange={(e) => setMaxIterations(parseInt(e.target.value) || 10)}
                    min={1}
                    max={100}
                    className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
                  />
                  <p className="text-xs text-zinc-500 mt-1">Verification/Agentic loop limit</p>
                </div>

                {/* State timeout */}
                <div>
                  <label
                    htmlFor="generate-state-timeout"
                    className="block text-sm font-medium text-zinc-400 mb-1.5"
                  >
                    State Timeout (sec)
                  </label>
                  <input
                    id="generate-state-timeout"
                    type="number"
                    value={stateTimeout}
                    onChange={(e) => setStateTimeout(parseInt(e.target.value) || 30)}
                    min={5}
                    max={300}
                    className="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-md text-zinc-200 focus:outline-hidden focus:ring-2 focus:ring-emerald-500/50"
                  />
                  <p className="text-xs text-zinc-500 mt-1">Per-state verification timeout</p>
                </div>
              </div>

              {/* Toggle options */}
              <div className="space-y-3">
                {/* Include setup navigation */}
                <label
                  htmlFor="generate-include-setup-nav"
                  aria-label="Include Setup Navigation"
                  className="flex items-center justify-between p-3 bg-zinc-800/50 rounded-md cursor-pointer hover:bg-zinc-800 transition-colors"
                >
                  <div>
                    <span className="text-sm font-medium text-zinc-300">
                      Include Setup Navigation
                    </span>
                    <p className="text-xs text-zinc-500 mt-0.5">
                      Navigate to initial state in setup phase
                    </p>
                  </div>
                  <input
                    id="generate-include-setup-nav"
                    type="checkbox"
                    checked={includeSetupNavigation}
                    onChange={(e) => setIncludeSetupNavigation(e.target.checked)}
                    className="w-4 h-4 rounded border-zinc-600 bg-zinc-700 text-emerald-500 focus:ring-emerald-500/50"
                  />
                </label>

                {/* Include AI contexts */}
                <label
                  htmlFor="generate-include-contexts"
                  aria-label="Include AI Contexts"
                  className="flex items-center justify-between p-3 bg-zinc-800/50 rounded-md cursor-pointer hover:bg-zinc-800 transition-colors"
                >
                  <div>
                    <span className="text-sm font-medium text-zinc-300">Include AI Contexts</span>
                    <p className="text-xs text-zinc-500 mt-0.5">
                      Add context snippets to agentic prompt
                    </p>
                  </div>
                  <input
                    id="generate-include-contexts"
                    type="checkbox"
                    checked={includeContexts}
                    onChange={(e) => setIncludeContexts(e.target.checked)}
                    className="w-4 h-4 rounded border-zinc-600 bg-zinc-700 text-emerald-500 focus:ring-emerald-500/50"
                  />
                </label>

                {/* Include AI summary */}
                <label
                  htmlFor="generate-include-summary"
                  aria-label="Include AI Summary"
                  className="flex items-center justify-between p-3 bg-zinc-800/50 rounded-md cursor-pointer hover:bg-zinc-800 transition-colors"
                >
                  <div>
                    <span className="text-sm font-medium text-zinc-300">Include AI Summary</span>
                    <p className="text-xs text-zinc-500 mt-0.5">
                      Generate summary in completion phase
                    </p>
                  </div>
                  <input
                    id="generate-include-summary"
                    type="checkbox"
                    checked={includeSummary}
                    onChange={(e) => setIncludeSummary(e.target.checked)}
                    className="w-4 h-4 rounded border-zinc-600 bg-zinc-700 text-emerald-500 focus:ring-emerald-500/50"
                  />
                </label>
              </div>

              {/* Error display */}
              {error && (
                <div className="flex items-start gap-3 p-4 bg-red-500/10 border border-red-500/30 rounded-lg">
                  <AlertCircle className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
                  <p className="text-red-400 text-sm">{error}</p>
                </div>
              )}
            </>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-3 px-6 py-4 border-t border-zinc-700">
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm font-medium text-zinc-400 hover:text-zinc-200 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleGenerate}
            disabled={!hasConfig || isGenerating || isCheckingConfig}
            className="flex items-center gap-2 px-4 py-2 text-sm font-medium bg-emerald-600 hover:bg-emerald-500 text-white rounded-md transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isGenerating ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                <span>Generating...</span>
              </>
            ) : (
              <>
                <Sparkles className="w-4 h-4" />
                <span>Generate Workflow</span>
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
