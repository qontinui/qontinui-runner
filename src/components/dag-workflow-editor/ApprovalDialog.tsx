/**
 * ApprovalDialog.tsx
 *
 * Global overlay that listens for `dag:approval-requested` Tauri events and
 * presents an Approve / Reject dialog to the user.  Mounted at the app level
 * so it is visible regardless of which page is active.
 *
 * When the user responds, the `respond_dag_approval` Tauri command is invoked,
 * which unblocks the `DagApprovalHandler` waiting in the Rust backend.
 */

import { useState, useEffect, useCallback } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ShieldCheck, ShieldX } from "lucide-react";
import { Button } from "@/components/ui/Button";

interface ApprovalRequest {
  approval_id: string;
  message: string;
  node_name?: string;
  task_run_id?: string;
}

export function ApprovalDialog() {
  const [request, setRequest] = useState<ApprovalRequest | null>(null);
  const [responding, setResponding] = useState(false);

  useEffect(() => {
    let unlisten: UnlistenFn | null = null;

    listen<ApprovalRequest>("dag:approval-requested", (event) => {
      setRequest(event.payload);
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch((err) => {
        console.error("[ApprovalDialog] Failed to set up event listener:", err);
      });

    return () => {
      unlisten?.();
    };
  }, []);

  const respond = useCallback(
    async (approved: boolean) => {
      if (!request) return;
      setResponding(true);
      try {
        await invoke("respond_dag_approval", {
          approvalId: request.approval_id,
          approved,
        });
      } catch (err) {
        console.error("[ApprovalDialog] Failed to respond to approval:", err);
      } finally {
        setRequest(null);
        setResponding(false);
      }
    },
    [request],
  );

  if (!request) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div className="bg-zinc-900 border border-zinc-700 rounded-xl shadow-2xl max-w-md w-full mx-4 p-6">
        {/* Header */}
        <div className="flex items-center gap-3 mb-4">
          <div className="p-2 rounded-lg bg-amber-500/10 border border-amber-500/20 shrink-0">
            <ShieldCheck className="w-6 h-6 text-amber-400" />
          </div>
          <div className="min-w-0">
            <h3 className="text-lg font-semibold text-zinc-100">Approval Required</h3>
            {request.node_name && (
              <p className="text-xs text-zinc-400 truncate">Node: {request.node_name}</p>
            )}
          </div>
        </div>

        {/* Message */}
        <p className="text-sm text-zinc-300 mb-6 leading-relaxed">{request.message}</p>

        {/* Task run context */}
        {request.task_run_id && (
          <p className="text-xs text-zinc-500 mb-4">
            Run:{" "}
            <span className="font-mono">{request.task_run_id.slice(0, 12)}…</span>
          </p>
        )}

        {/* Actions */}
        <div className="flex gap-3 justify-end">
          <Button
            variant="danger"
            size="sm"
            onClick={() => respond(false)}
            disabled={responding}
          >
            <ShieldX className="w-4 h-4" />
            Reject
          </Button>
          <Button
            variant="success"
            size="sm"
            onClick={() => respond(true)}
            disabled={responding}
          >
            <ShieldCheck className="w-4 h-4" />
            Approve
          </Button>
        </div>
      </div>
    </div>
  );
}
