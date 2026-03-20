/**
 * WorktreeComparisonTab — branch management for autoresearch worktree comparisons.
 *
 * Uses Tauri invoke commands backed by the worktree database records.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";

interface WorktreeEntry {
  branch_name: string;
  task_run_id?: string;
  worktree_path: string;
  source_branch: string;
  repo_path: string;
  status: string;
  workflow_name?: string;
  created_at: string;
}

interface DiffResult {
  diff: string;
  files_changed: number;
}

const statusBadge: Record<string, string> = {
  active: "bg-blue-900/50 text-blue-300 border-blue-700",
  ready: "bg-cyan-900/50 text-cyan-300 border-cyan-700",
  completed: "bg-green-900/50 text-green-300 border-green-700",
  failed: "bg-red-900/50 text-red-300 border-red-700",
  merged: "bg-purple-900/50 text-purple-300 border-purple-700",
  removed: "bg-zinc-900/50 text-zinc-400 border-zinc-700",
};

export function WorktreeComparisonTab() {
  const [worktrees, setWorktrees] = useState<WorktreeEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [diffResult, setDiffResult] = useState<{ branch: string; data: DiffResult } | null>(null);
  const [comparisonReport, setComparisonReport] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null);
  const [actionLoading, setActionLoading] = useState<string | null>(null);

  const fetchWorktrees = useCallback(async () => {
    try {
      const records = await invoke<WorktreeEntry[]>("list_worktree_records");
      setWorktrees(records);
    } catch {
      // Command may not be available yet
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchWorktrees();
  }, [fetchWorktrees]);

  async function viewDiff(branch: string) {
    setActionLoading(branch);
    setError(null);
    try {
      const data = await invoke<DiffResult>("get_worktree_diff", { branchName: branch });
      setDiffResult({ branch, data });
    } catch (e) {
      setError(`Diff failed: ${e}`);
    } finally {
      setActionLoading(null);
    }
  }

  async function mergeWorktree(branch: string) {
    setActionLoading(branch);
    setError(null);
    try {
      await invoke("merge_worktree_branch", { branchName: branch, aiResolve: true });
      await fetchWorktrees();
    } catch (e) {
      setError(`Merge failed: ${e}`);
    } finally {
      setActionLoading(null);
    }
  }

  async function removeWorktree(branch: string) {
    setActionLoading(branch);
    setError(null);
    setConfirmRemove(null);
    try {
      await invoke("remove_worktree_branch", { branchName: branch });
      await fetchWorktrees();
    } catch (e) {
      setError(`Remove failed: ${e}`);
    } finally {
      setActionLoading(null);
    }
  }

  async function compareAll() {
    setActionLoading("compare");
    setError(null);
    try {
      const data = await invoke<{ report: string }>("compare_worktree_branches");
      setComparisonReport(data.report ?? JSON.stringify(data, null, 2));
    } catch (e) {
      setError(`Compare failed: ${e}`);
    } finally {
      setActionLoading(null);
    }
  }

  if (loading) {
    return <div className="text-sm text-zinc-500 p-4">Loading worktrees...</div>;
  }

  // Filter out removed worktrees from the list
  const visibleWorktrees = worktrees.filter((wt) => wt.status !== "removed");

  return (
    <div className="p-4 space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-bold text-zinc-200">Worktree Branches</h3>
        <button
          onClick={compareAll}
          disabled={actionLoading === "compare" || visibleWorktrees.length < 2}
          className="px-3 py-1.5 bg-blue-600 hover:bg-blue-700 disabled:opacity-50 text-white text-xs rounded-md transition-colors"
        >
          {actionLoading === "compare" ? "Comparing..." : "Compare All"}
        </button>
      </div>

      {error && <div className="text-xs bg-red-900/30 text-red-400 rounded px-2 py-1">{error}</div>}

      {/* Worktree list */}
      {visibleWorktrees.length === 0 ? (
        <div className="text-sm text-zinc-500">
          No worktree branches found. Start a campaign with worktree isolation enabled.
        </div>
      ) : (
        <div className="space-y-2">
          {visibleWorktrees.map((wt) => (
            <div
              key={wt.branch_name}
              className="border border-zinc-700 rounded-md p-3 flex items-center justify-between"
            >
              <div className="flex items-center gap-3">
                <span className="font-mono text-sm text-zinc-200">{wt.branch_name}</span>
                <span
                  className={`text-xs px-2 py-0.5 border rounded ${statusBadge[wt.status] ?? "bg-zinc-800 text-zinc-400 border-zinc-700"}`}
                >
                  {wt.status}
                </span>
                {wt.task_run_id && (
                  <span className="text-xs text-zinc-500">Run: {wt.task_run_id}</span>
                )}
                {wt.workflow_name && (
                  <span className="text-xs text-zinc-600">{wt.workflow_name}</span>
                )}
              </div>
              <div className="flex gap-2">
                <button
                  onClick={() => viewDiff(wt.branch_name)}
                  disabled={actionLoading === wt.branch_name}
                  className="px-2 py-1 text-xs bg-zinc-700 hover:bg-zinc-600 disabled:opacity-50 text-zinc-200 rounded transition-colors"
                >
                  View Diff
                </button>
                <button
                  onClick={() => mergeWorktree(wt.branch_name)}
                  disabled={actionLoading === wt.branch_name || wt.status === "merged"}
                  className="px-2 py-1 text-xs bg-zinc-700 hover:bg-zinc-600 disabled:opacity-50 text-zinc-200 rounded transition-colors"
                >
                  Merge
                </button>
                <button
                  onClick={() =>
                    confirmRemove === wt.branch_name
                      ? removeWorktree(wt.branch_name)
                      : setConfirmRemove(wt.branch_name)
                  }
                  disabled={actionLoading === wt.branch_name}
                  className={`px-2 py-1 text-xs rounded transition-colors ${
                    confirmRemove === wt.branch_name
                      ? "bg-red-700 hover:bg-red-600 text-white"
                      : "bg-zinc-700 hover:bg-zinc-600 text-zinc-200"
                  }`}
                >
                  {confirmRemove === wt.branch_name ? "Confirm Remove" : "Remove"}
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* Diff viewer */}
      {diffResult && (
        <div className="border border-zinc-700 rounded-lg">
          <div className="flex items-center justify-between px-3 py-2 border-b border-zinc-700">
            <span className="text-xs text-zinc-400">
              Diff: <span className="font-mono text-zinc-200">{diffResult.branch}</span> (
              {diffResult.data.files_changed} files)
            </span>
            <button
              onClick={() => setDiffResult(null)}
              className="text-xs text-zinc-500 hover:text-zinc-300"
            >
              Close
            </button>
          </div>
          <pre className="p-3 text-xs font-mono overflow-auto max-h-96 bg-zinc-900 text-zinc-300">
            {diffResult.data.diff || "(no changes)"}
          </pre>
        </div>
      )}

      {/* Comparison report */}
      {comparisonReport && (
        <div className="border border-zinc-700 rounded-lg">
          <div className="flex items-center justify-between px-3 py-2 border-b border-zinc-700">
            <span className="text-xs text-zinc-400">AI Comparison Report</span>
            <button
              onClick={() => setComparisonReport(null)}
              className="text-xs text-zinc-500 hover:text-zinc-300"
            >
              Close
            </button>
          </div>
          <pre className="p-3 text-xs whitespace-pre-wrap overflow-auto max-h-96 bg-zinc-900 text-zinc-300">
            {comparisonReport}
          </pre>
        </div>
      )}
    </div>
  );
}
