/**
 * SyncConfirmModal — Pre-flight confirmation for the Sync All Specs flow.
 *
 * Shown after `useSpecSync.analyze` resolves but before `runSync` actually
 * starts the AI loop. Surfaces:
 *  - Spec count
 *  - Estimated duration (3.5 minutes per spec, sequential)
 *  - Reason breakdown (quarantined / missing-state-machine)
 *
 * Progress / spinner / cancellation are owned by SyncProgressBanner once the
 * sync starts — this dialog is purely "are you sure?".
 */

import { useEffect, useRef } from "react";
import { useUIElement } from "@qontinui/ui-bridge";
import { AlertTriangle, RefreshCw, X } from "lucide-react";
import type { SpecToSync } from "@/hooks/useSpecSync";

interface SyncConfirmModalProps {
  open: boolean;
  toSync: SpecToSync[];
  onConfirm: () => void;
  onCancel: () => void;
}

const MINUTES_PER_SPEC = 3.5;

function formatDuration(totalMinutes: number): string {
  if (totalMinutes <= 0) return "<1 min";
  if (totalMinutes < 60) {
    const m = Math.max(1, Math.round(totalMinutes));
    return `~${m} min`;
  }
  const totalRounded = Math.round(totalMinutes);
  const h = Math.floor(totalRounded / 60);
  const m = totalRounded % 60;
  if (m === 0) return `~${h} h`;
  return `~${h} h ${m} min`;
}

function countByReason(toSync: SpecToSync[]): Record<SpecToSync["reason"], number> {
  const counts: Record<SpecToSync["reason"], number> = {
    quarantined: 0,
    "missing-state-machine": 0,
  };
  for (const item of toSync) {
    counts[item.reason]++;
  }
  return counts;
}

export function SyncConfirmModal({ open, toSync, onConfirm, onCancel }: SyncConfirmModalProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);

  const { ref: confirmRef } = useUIElement({
    id: "specs-btn-sync-confirm",
    type: "button",
    label: "Confirm sync",
    actions: ["click"],
  });
  const { ref: cancelRef } = useUIElement({
    id: "specs-btn-sync-cancel-modal",
    type: "button",
    label: "Cancel sync confirmation",
    actions: ["click"],
  });

  // Sync the <dialog> open/close state with the controlled prop.
  useEffect(() => {
    const dlg = dialogRef.current;
    if (!dlg) return;
    if (open && !dlg.open) {
      try {
        dlg.showModal();
      } catch {
        // Already open (rare race) — ignore.
      }
    } else if (!open && dlg.open) {
      dlg.close();
    }
  }, [open]);

  if (!open) return null;

  const total = toSync.length;
  const counts = countByReason(toSync);
  const duration = formatDuration(total * MINUTES_PER_SPEC);

  return (
    <dialog
      ref={dialogRef}
      onCancel={(e) => {
        e.preventDefault();
        onCancel();
      }}
      className="bg-transparent p-0 backdrop:bg-black/60 max-w-md w-full"
    >
      <div className="bg-zinc-900 border border-white/10 rounded-lg shadow-xl text-foreground">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-white/10">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <RefreshCw className="w-4 h-4 text-teal-400" />
            Confirm Sync All Specs
          </h2>
          <button
            ref={cancelRef}
            onClick={onCancel}
            className="text-muted-foreground hover:text-foreground transition-colors"
            aria-label="Close"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Body */}
        <div className="px-4 py-3 space-y-3 text-xs">
          {total === 0 ? (
            <div className="flex items-start gap-2 text-amber-400">
              <AlertTriangle className="w-4 h-4 shrink-0 mt-0.5" />
              <span>
                No specs need syncing. All loaded specs have a state machine and none appear in the
                latest quarantine record.
              </span>
            </div>
          ) : (
            <>
              <div>
                <span className="text-muted-foreground">Specs to sync:</span>{" "}
                <span className="font-semibold">{total}</span>
              </div>
              <div>
                <span className="text-muted-foreground">Estimated duration:</span>{" "}
                <span className="font-semibold">{duration}</span>
                <span className="text-muted-foreground"> ({MINUTES_PER_SPEC} min/spec)</span>
              </div>
              <div className="pt-1 border-t border-white/5">
                <div className="text-muted-foreground mb-1">Breakdown:</div>
                <ul className="space-y-0.5 pl-2">
                  <li>
                    <span className="font-mono">{counts.quarantined}</span> quarantined
                  </li>
                  <li>
                    <span className="font-mono">{counts["missing-state-machine"]}</span>{" "}
                    missing-state-machine
                  </li>
                </ul>
              </div>
            </>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-white/10">
          <button
            onClick={onCancel}
            className="px-3 py-1 text-xs font-medium rounded
            bg-white/5 text-muted-foreground border border-white/10
            hover:bg-white/10 transition-colors"
          >
            Cancel
          </button>
          <button
            ref={confirmRef}
            onClick={onConfirm}
            disabled={total === 0}
            className="px-3 py-1 text-xs font-semibold rounded
            bg-teal-600 text-white shadow-xs shadow-teal-600/25
            hover:bg-teal-700 disabled:opacity-50 transition-colors"
          >
            Confirm
          </button>
        </div>
      </div>
    </dialog>
  );
}
