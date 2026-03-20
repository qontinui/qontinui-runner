/**
 * SlashCommandArgsDialog
 *
 * Modal dialog for entering $ARGUMENTS when running a slash command
 * workflow that requires arguments.
 */

import { useState } from "react";
import { Terminal, Play, X } from "lucide-react";

interface SlashCommandArgsDialogProps {
  workflowName: string;
  onRun: (args: string) => void;
  onCancel: () => void;
}

export function SlashCommandArgsDialog({
  workflowName,
  onRun,
  onCancel,
}: SlashCommandArgsDialogProps) {
  const [args, setArgs] = useState("");

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    onRun(args);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="card w-full max-w-lg mx-4 p-4 shadow-2xl">
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-2">
            <Terminal className="w-5 h-5 text-cyan-400" />
            <h3 className="text-h3 text-foreground">{workflowName}</h3>
          </div>
          <button
            onClick={onCancel}
            className="p-1 rounded-md text-muted-foreground hover:text-foreground hover:bg-white/5 transition-colors"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        <form onSubmit={handleSubmit}>
          <label className="block text-body-sm text-muted-foreground mb-2">
            This command accepts arguments. Enter them below (replaces{" "}
            <code className="px-1 py-0.5 rounded bg-white/10 text-cyan-400 text-xs">
              $ARGUMENTS
            </code>{" "}
            in the prompt):
          </label>
          <textarea
            value={args}
            onChange={(e) => setArgs(e.target.value)}
            placeholder="Enter arguments..."
            className="input w-full h-32 resize-y font-mono text-sm"
            autoFocus
          />
          <div className="flex justify-end gap-2 mt-4">
            <button type="button" onClick={onCancel} className="btn-secondary">
              Cancel
            </button>
            <button type="submit" className="btn-primary flex items-center gap-2">
              <Play className="w-4 h-4" />
              Run
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
