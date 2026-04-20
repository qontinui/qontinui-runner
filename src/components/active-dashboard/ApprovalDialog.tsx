import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { Loader2, History, ChevronDown, ChevronUp } from "lucide-react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { useTaskRunProgress } from "@/hooks/graphql";

interface ApprovalRequest {
  id: string;
  execution_id: string;
  iteration: number;
  prompt: string;
  context: {
    summary: string;
    files_modified: string[];
    git_diff_stat?: string;
    git_diff?: string;
  };
  options: string[];
  created_at: string;
}

interface ApprovalGateRecord {
  id: string;
  task_run_id: string;
  iteration: number;
  prompt: string;
  action: string | null;
  comment: string | null;
  status: string;
  created_at: string;
  resolved_at: string | null;
}

interface ApprovalDialogProps {
  taskRunId: string;
  onResolved?: () => void;
}

export function ApprovalDialog({ taskRunId, onResolved }: ApprovalDialogProps) {
  const [approvals, setApprovals] = useState<ApprovalRequest[]>([]);
  const [comment, setComment] = useState("");
  const [responding, setResponding] = useState(false);
  const [showDiff, setShowDiff] = useState(false);
  const [dismissed, setDismissed] = useState(false);

  // Approval history state
  const [showHistory, setShowHistory] = useState(false);
  const [history, setHistory] = useState<ApprovalGateRecord[]>([]);
  const [historyLoading, setHistoryLoading] = useState(false);

  const prevCountRef = useRef(0);
  const approveButtonRef = useRef<HTMLButtonElement>(null);
  const commentRef = useRef<HTMLTextAreaElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);

  // GraphQL subscription triggers re-fetch on task progress events
  const { data: progressData } = useTaskRunProgress(taskRunId);
  const lastProgressKeyRef = useRef("");

  // Poll for pending approvals — subscription replaces aggressive 3s polling
  const pollRef = useRef<() => Promise<void>>(undefined);
  useEffect(() => {
    let cancelled = false;

    const poll = async () => {
      try {
        const resp = await tracedFetch(`${getApiBase()}/task-runs/${taskRunId}/approvals`);
        if (resp.ok && !cancelled) {
          const data = await resp.json();
          setApprovals(data);

          // Play notification sound when a new approval arrives
          if (data.length > 0 && prevCountRef.current === 0) {
            // Reset dismissed state when a new approval arrives
            setDismissed(false);
            try {
              const ctx = new AudioContext();
              const osc = ctx.createOscillator();
              const gain = ctx.createGain();
              osc.connect(gain);
              gain.connect(ctx.destination);
              osc.frequency.value = 800;
              gain.gain.value = 0.1;
              osc.start();
              osc.stop(ctx.currentTime + 0.15);
            } catch {
              // Audio not available, ignore
            }
          }
          prevCountRef.current = data.length;
        }
      } catch {
        // API not available, ignore
      }
    };
    pollRef.current = poll;

    poll();
    // Relaxed from 3s to 10s — subscription handles real-time events
    const interval = setInterval(poll, 10000);

    // Also listen for Tauri "approval-required" events for instant notification
    let unlisten: (() => void) | undefined;
    listen("approval-required", () => {
      poll();
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      cancelled = true;
      clearInterval(interval);
      unlisten?.();
    };
  }, [taskRunId]);

  // Re-fetch approvals immediately when subscription delivers a step progress event
  useEffect(() => {
    const event = progressData?.taskRunProgress;
    if (!event) return;
    const key = `${event.__typename}:${event.status}:${event.timestamp}`;
    if (key !== lastProgressKeyRef.current) {
      lastProgressKeyRef.current = key;
      pollRef.current?.();
    }
  }, [progressData]);

  const handleRespond = useCallback(
    async (approvalId: string, action: string) => {
      setResponding(true);
      try {
        const resp = await tracedFetch(
          `${getApiBase()}/task-runs/${taskRunId}/approvals/${approvalId}/respond`,
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
              action,
              comment: comment || undefined,
            }),
          },
        );
        if (resp.ok) {
          setApprovals((prev) => prev.filter((a) => a.id !== approvalId));
          setComment("");
          onResolved?.();
        }
      } catch (e) {
        console.error("Failed to respond to approval:", e);
      } finally {
        setResponding(false);
      }
    },
    [taskRunId, comment, onResolved],
  );

  // Auto-focus approve button when dialog appears
  useEffect(() => {
    if (approvals.length > 0 && !dismissed) {
      // Slight delay to allow animation to start
      const timer = setTimeout(() => {
        approveButtonRef.current?.focus();
      }, 100);
      return () => clearTimeout(timer);
    }
  }, [approvals.length, dismissed]);

  // Keyboard shortcuts
  useEffect(() => {
    if (approvals.length === 0 || dismissed) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      // Don't trigger shortcuts when typing in textarea
      const isTyping = document.activeElement === commentRef.current;

      if (e.key === "Escape") {
        e.preventDefault();
        setDismissed(true);
        return;
      }

      if (e.key === "Enter" && !isTyping && !e.shiftKey && !responding) {
        e.preventDefault();
        const approval = approvals[0];
        if (approval) {
          handleRespond(approval.id, "approve");
        }
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [approvals, dismissed, responding, handleRespond]);

  // Fetch approval history
  const fetchHistory = useCallback(async () => {
    setHistoryLoading(true);
    try {
      const resp = await tracedFetch(`${getApiBase()}/task-runs/${taskRunId}/approval-gates`);
      if (resp.ok) {
        const data = await resp.json();
        // Filter to only resolved gates (not the currently pending one)
        const resolved = (data as ApprovalGateRecord[]).filter((g) => g.status !== "pending");
        setHistory(resolved);
      }
    } catch {
      // API not available, ignore
    } finally {
      setHistoryLoading(false);
    }
  }, [taskRunId]);

  // Fetch history when toggled on
  useEffect(() => {
    if (showHistory) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      fetchHistory();
    }
  }, [showHistory, fetchHistory]);

  if (approvals.length === 0) {
    return null;
  }

  // If dismissed, show a small floating indicator to re-open
  if (dismissed) {
    return (
      <button
        onClick={() => setDismissed(false)}
        className="fixed bottom-4 right-4 z-50 flex items-center gap-2 px-4 py-2 bg-amber-500/20 border border-amber-500/40 rounded-lg text-amber-300 text-sm font-medium hover:bg-amber-500/30 transition-colors shadow-lg"
      >
        <div className="w-2 h-2 bg-amber-400 rounded-full animate-pulse" />
        Approval Waiting ({approvals.length})
      </button>
    );
  }

  const approval = approvals[0]; // Show one at a time

  const formatTimestamp = (ts: string) => {
    try {
      const d = new Date(ts);
      return d.toLocaleTimeString([], {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      });
    } catch {
      return ts;
    }
  };

  const actionBadgeClass = (action: string | null) => {
    switch (action) {
      case "approve":
        return "bg-green-500/20 text-green-300 border-green-500/30";
      case "reject":
        return "bg-red-500/20 text-red-300 border-red-500/30";
      case "abort":
        return "bg-gray-500/20 text-gray-300 border-gray-500/30";
      default:
        return "bg-gray-500/20 text-gray-400 border-gray-500/30";
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div
        ref={dialogRef}
        className="bg-gray-900 border border-amber-500/50 rounded-lg shadow-2xl max-w-2xl w-full mx-4 max-h-[80vh] flex flex-col"
        style={{
          animation: "approvalSlideIn 0.25s ease-out",
        }}
      >
        {/* Inline keyframes for slide-in animation */}
        <style>{`
          @keyframes approvalSlideIn {
            from {
              opacity: 0;
              transform: translateY(16px) scale(0.98);
            }
            to {
              opacity: 1;
              transform: translateY(0) scale(1);
            }
          }
        `}</style>

        {/* Header */}
        <div className="flex items-center gap-3 px-5 py-3 border-b border-gray-700 bg-amber-500/10 rounded-t-lg">
          <div className="w-2 h-2 bg-amber-400 rounded-full animate-pulse" />
          <h2 className="text-sm font-semibold text-amber-300">Approval Required</h2>
          <button
            onClick={() => setShowHistory(!showHistory)}
            className={`ml-2 flex items-center gap-1 px-2 py-0.5 text-xs rounded border transition-colors ${
              showHistory
                ? "bg-gray-700 text-gray-200 border-gray-600"
                : "bg-transparent text-gray-500 border-gray-700 hover:text-gray-300 hover:border-gray-600"
            }`}
            title="Toggle approval history"
          >
            <History className="w-3 h-3" />
            History
            {showHistory ? <ChevronUp className="w-3 h-3" /> : <ChevronDown className="w-3 h-3" />}
          </button>
          <span className="ml-auto text-xs text-gray-500">Iteration {approval.iteration}</span>
        </div>

        {/* History section (collapsible) */}
        {showHistory && (
          <div className="border-b border-gray-700 bg-gray-800/50 px-5 py-3 max-h-40 overflow-auto">
            {historyLoading ? (
              <div className="flex items-center gap-2 text-xs text-gray-500">
                <Loader2 className="w-3 h-3 animate-spin" />
                Loading history...
              </div>
            ) : history.length === 0 ? (
              <p className="text-xs text-gray-500">No previous approvals for this run.</p>
            ) : (
              <div className="space-y-1.5">
                <p className="text-xs font-medium text-gray-400 mb-2">
                  Past Approvals ({history.length})
                </p>
                {history.map((gate) => (
                  <div key={gate.id} className="flex items-center gap-2 text-xs">
                    <span className="text-gray-500 font-mono w-8 shrink-0">#{gate.iteration}</span>
                    <span
                      className={`px-1.5 py-0.5 rounded border text-[10px] font-medium uppercase tracking-wide ${actionBadgeClass(gate.action)}`}
                    >
                      {gate.action || "unknown"}
                    </span>
                    {gate.comment && (
                      <span className="text-gray-400 truncate max-w-[200px]">"{gate.comment}"</span>
                    )}
                    <span className="ml-auto text-gray-600 shrink-0">
                      {gate.resolved_at ? formatTimestamp(gate.resolved_at) : "--"}
                    </span>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}

        {/* Content */}
        <div className="flex-1 overflow-auto px-5 py-4 space-y-4">
          {/* Prompt */}
          <p className="text-sm text-white">{approval.prompt}</p>

          {/* Summary */}
          {approval.context.summary && (
            <div className="bg-gray-800 rounded p-3">
              <h3 className="text-xs font-medium text-gray-400 mb-1">What was done</h3>
              <p className="text-sm text-gray-200">{approval.context.summary}</p>
            </div>
          )}

          {/* Files modified */}
          {approval.context.files_modified.length > 0 && (
            <div className="bg-gray-800 rounded p-3">
              <h3 className="text-xs font-medium text-gray-400 mb-1">
                Files modified ({approval.context.files_modified.length})
              </h3>
              <ul className="text-xs text-gray-300 font-mono space-y-0.5">
                {approval.context.files_modified.map((f) => (
                  <li key={f}>{f}</li>
                ))}
              </ul>
            </div>
          )}

          {/* Diff stat */}
          {approval.context.git_diff_stat && (
            <div className="text-xs text-gray-400 font-mono">{approval.context.git_diff_stat}</div>
          )}

          {/* Collapsible diff */}
          {approval.context.git_diff && (
            <div>
              <button
                onClick={() => setShowDiff(!showDiff)}
                className="text-xs text-blue-400 hover:text-blue-300"
              >
                {showDiff ? "Hide diff" : "Show diff"}
              </button>
              {showDiff && (
                <pre className="mt-2 bg-gray-950 rounded p-3 text-xs text-gray-300 font-mono overflow-auto max-h-60">
                  {approval.context.git_diff}
                </pre>
              )}
            </div>
          )}

          {/* Comment */}
          <div>
            <label className="block text-xs font-medium text-gray-400 mb-1">
              Comment (optional)
            </label>
            <textarea
              ref={commentRef}
              className="w-full px-3 py-2 text-sm bg-gray-800 border border-gray-600 rounded text-white placeholder-gray-500 focus:border-blue-500 focus:outline-hidden"
              value={comment}
              onChange={(e) => setComment(e.target.value)}
              placeholder="Add a comment..."
              rows={2}
            />
          </div>
        </div>

        {/* Actions */}
        <div className="flex flex-col gap-2 px-5 py-3 border-t border-gray-700">
          <div className="flex items-center gap-2">
            <button
              onClick={() => handleRespond(approval.id, "reject")}
              disabled={responding}
              className="px-4 py-2 text-sm text-red-300 bg-red-500/10 border border-red-500/30 rounded hover:bg-red-500/20 disabled:opacity-50"
            >
              Reject
            </button>
            <button
              onClick={() => handleRespond(approval.id, "abort")}
              disabled={responding}
              className="px-4 py-2 text-sm text-gray-300 bg-gray-700 rounded hover:bg-gray-600 disabled:opacity-50"
            >
              Abort Workflow
            </button>
            <div className="flex-1" />
            <button
              ref={approveButtonRef}
              onClick={() => handleRespond(approval.id, "approve")}
              disabled={responding}
              className="px-6 py-2 text-sm text-white bg-green-600 rounded hover:bg-green-500 disabled:opacity-50 font-medium focus:ring-2 focus:ring-green-400 focus:ring-offset-1 focus:ring-offset-gray-900 focus:outline-hidden flex items-center gap-2"
            >
              {responding ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Approving...
                </>
              ) : (
                "Approve"
              )}
            </button>
          </div>
          <p className="text-[10px] text-gray-600 text-center">
            Keyboard shortcuts: Enter = Approve, Esc = Dismiss
          </p>
        </div>
      </div>
    </div>
  );
}
