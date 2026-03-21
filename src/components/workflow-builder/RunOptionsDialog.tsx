/**
 * RunOptionsDialog.tsx
 *
 * Modal dialog for selecting workflow run options before execution.
 * Allows selecting one or more architectures, worktree isolation, and comparison runs.
 */

import { useState, useEffect } from "react";
import { X, Play, Loader2, GitBranch, BarChart3, Check } from "lucide-react";
import { Button } from "../ui";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

type WorkflowArchitecture = "traditional" | "agentic_verification" | "multi_agent_pipeline";

interface ArchitectureOption {
  value: WorkflowArchitecture;
  label: string;
  description: string;
}

const ARCHITECTURE_OPTIONS: ArchitectureOption[] = [
  {
    value: "traditional",
    label: "Traditional",
    description: "Sequential step execution with deterministic verification",
  },
  {
    value: "agentic_verification",
    label: "Agentic Verification",
    description: "AI verifier + AI worker reasoning loop",
  },
  {
    value: "multi_agent_pipeline",
    label: "Multi-Agent Pipeline",
    description:
      "DAG-based pipeline with specialized agents (spec analyst → locator → implementer → verifier)",
  },
];

export interface RunOptionsDialogProps {
  open: boolean;
  onClose: () => void;
  onRun: (overrides: Record<string, unknown>) => void;
  defaultArchitecture?: string;
  isLoading?: boolean;
  workflowId?: string | null;
}

export function RunOptionsDialog({
  open,
  onClose,
  onRun,
  defaultArchitecture = "traditional",
  isLoading = false,
  workflowId,
}: RunOptionsDialogProps) {
  const [selected, setSelected] = useState<Set<WorkflowArchitecture>>(
    new Set([(defaultArchitecture as WorkflowArchitecture) || "traditional"]),
  );
  const [useWorktree, setUseWorktree] = useState(false);
  const [isLaunching, setIsLaunching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setSelected(new Set([(defaultArchitecture as WorkflowArchitecture) || "traditional"]));
      setUseWorktree(false);
      setIsLaunching(false);
      setError(null);
    }
  }, [open, defaultArchitecture]);

  const toggle = (arch: WorkflowArchitecture) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(arch)) {
        if (next.size > 1) next.delete(arch);
      } else {
        next.add(arch);
      }
      return next;
    });
  };

  const selectAll = () => {
    setSelected(new Set(ARCHITECTURE_OPTIONS.map((o) => o.value)));
  };

  const isMultiple = selected.size > 1;

  const handleRun = async () => {
    setError(null);

    if (isMultiple) {
      // Pass all selected architectures to parent — it handles save-before-run + multi-launch
      onRun({
        _compareArchitectures: [...selected],
        use_worktree: true,
      });
      return;
    } else {
      // Single architecture → normal run via callback
      const arch = [...selected][0];
      const overrides: Record<string, unknown> = {};
      if (arch !== "traditional") overrides.workflow_architecture = arch;
      if (useWorktree) overrides.use_worktree = true;
      onRun(overrides);
    }
  };

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div
        role="button"
        tabIndex={0}
        aria-label="Close run options dialog"
        className="absolute inset-0 bg-black/50"
        onClick={isLoading ? undefined : onClose}
        onKeyDown={(e) => {
          if ((e.key === "Enter" || e.key === " ") && !isLoading) onClose();
        }}
      />

      <div className="relative bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl w-full max-w-md mx-4 overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-700 bg-zinc-800/50">
          <h3 className="text-lg font-semibold text-zinc-100">Run Options</h3>
          <button
            onClick={onClose}
            disabled={isLoading}
            className="p-1 text-zinc-400 hover:text-zinc-200 transition-colors disabled:opacity-50"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="p-6 space-y-5">
          {/* Architecture Selection */}
          <div className="space-y-2.5">
            <div className="flex items-center justify-between">
              <label className="text-sm font-medium text-zinc-300">
                Workflow Architecture
                {isMultiple && (
                  <span className="ml-2 text-xs text-amber-400">
                    ({selected.size} selected — will run all in parallel)
                  </span>
                )}
              </label>
              <button
                type="button"
                onClick={selectAll}
                className="text-xs text-blue-400 hover:text-blue-300 transition-colors"
              >
                Select all
              </button>
            </div>
            {ARCHITECTURE_OPTIONS.map((option) => {
              const isSelected = selected.has(option.value);
              return (
                <button
                  key={option.value}
                  type="button"
                  onClick={() => toggle(option.value)}
                  disabled={isLoading || isLaunching}
                  className={`w-full p-3 rounded-lg border transition-all text-left ${
                    isSelected
                      ? "border-blue-500/50 bg-blue-500/10 ring-1 ring-blue-500/30"
                      : "border-zinc-700 bg-zinc-800/50 hover:border-zinc-600"
                  } disabled:opacity-50 disabled:cursor-not-allowed`}
                >
                  <div className="flex items-center gap-3">
                    <div className="flex-1 min-w-0">
                      <span
                        className={`text-sm font-medium ${
                          isSelected ? "text-zinc-100" : "text-zinc-300"
                        }`}
                      >
                        {option.label}
                      </span>
                      <p className="text-xs text-zinc-400 mt-0.5">{option.description}</p>
                    </div>
                    <div
                      className={`w-5 h-5 rounded shrink-0 flex items-center justify-center border ${
                        isSelected ? "bg-blue-500 border-blue-500" : "border-zinc-600 bg-zinc-800"
                      }`}
                    >
                      {isSelected && <Check className="w-3.5 h-3.5 text-white" />}
                    </div>
                  </div>
                </button>
              );
            })}
          </div>

          {/* Worktree Isolation */}
          <label className="flex items-center gap-3 p-3 rounded-lg border border-zinc-700 bg-zinc-800/50 cursor-pointer hover:border-zinc-600 transition-colors">
            <input
              type="checkbox"
              checked={useWorktree || isMultiple}
              onChange={(e) => setUseWorktree(e.target.checked)}
              disabled={isLoading || isLaunching || isMultiple}
              className="w-4 h-4 rounded border-zinc-600 bg-zinc-800 text-blue-500 focus:ring-blue-500/50"
            />
            <GitBranch className="w-4 h-4 text-zinc-400 shrink-0" />
            <div className="flex-1 min-w-0">
              <span className="text-sm font-medium text-zinc-300">Worktree isolation</span>
              <p className="text-xs text-zinc-400 mt-0.5">
                {isMultiple
                  ? "Enabled — each architecture runs on its own branch"
                  : "Run in isolated git worktree (changes on separate branch)"}
              </p>
            </div>
          </label>

          {/* Error message */}
          {error && (
            <pre className="text-xs text-red-400 bg-red-900/20 border border-red-800/30 rounded-lg px-4 py-2 whitespace-pre-wrap overflow-auto max-h-32">
              {error}
            </pre>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-3 px-6 py-4 border-t border-zinc-700 bg-zinc-800/50">
          <Button variant="ghost" onClick={onClose} disabled={isLoading || isLaunching}>
            Cancel
          </Button>
          <Button variant="primary" onClick={handleRun} disabled={isLoading || isLaunching}>
            {isLaunching ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Launching {selected.size} runs...
              </>
            ) : isMultiple ? (
              <>
                <BarChart3 className="w-4 h-4" />
                Compare {selected.size} Architectures
              </>
            ) : (
              <>
                <Play className="w-4 h-4" />
                Run
              </>
            )}
          </Button>
        </div>
      </div>
    </div>
  );
}

export default RunOptionsDialog;
