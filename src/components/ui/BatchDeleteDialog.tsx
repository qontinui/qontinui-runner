/**
 * BatchDeleteDialog.tsx
 *
 * Reusable confirmation dialog for batch deletion of items.
 * Shows a list of items to be deleted and requires confirmation.
 */

import { AlertTriangle, X, Loader2 } from "lucide-react";

interface BatchDeleteDialogProps {
  /** Whether the dialog is open */
  open: boolean;
  /** Title for the dialog */
  title: string;
  /** Type of items being deleted (e.g., "workflow", "step") */
  itemType: string;
  /** Names of items to be deleted */
  itemNames: string[];
  /** Whether deletion is in progress */
  isDeleting?: boolean;
  /** Called when the dialog is closed */
  onClose: () => void;
  /** Called when user confirms deletion */
  onConfirm: () => void;
}

export function BatchDeleteDialog({
  open,
  title,
  itemType,
  itemNames,
  isDeleting = false,
  onClose,
  onConfirm,
}: BatchDeleteDialogProps) {
  if (!open) return null;

  const itemCount = itemNames.length;
  const pluralSuffix = itemCount === 1 ? "" : "s";

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div className="absolute inset-0 bg-black/50" onClick={isDeleting ? undefined : onClose} />

      {/* Dialog */}
      <div
        data-ui-id="dialog-batch-delete"
        className="relative bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl w-full max-w-md mx-4 overflow-hidden"
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-700 bg-red-500/10">
          <div className="flex items-center gap-2">
            <AlertTriangle className="w-5 h-5 text-red-400" />
            <h3 className="text-lg font-semibold text-red-400">{title}</h3>
          </div>
          <button
            data-ui-id="dialog-batch-delete-close-btn"
            onClick={onClose}
            disabled={isDeleting}
            className="p-1 text-zinc-400 hover:text-zinc-200 transition-colors disabled:opacity-50"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="p-6 space-y-4">
          <p className="text-zinc-300">
            Are you sure you want to delete {itemCount} {itemType}
            {pluralSuffix}? This action cannot be undone.
          </p>

          {/* Item List */}
          {itemNames.length > 0 && (
            <div className="max-h-40 overflow-y-auto bg-zinc-800 rounded-md border border-zinc-700">
              <ul className="divide-y divide-zinc-700">
                {itemNames.map((name, index) => (
                  <li key={index} className="px-3 py-2 text-sm text-zinc-300 truncate">
                    {name}
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-3 px-6 py-4 border-t border-zinc-700 bg-zinc-800/50">
          <button
            data-ui-id="dialog-batch-delete-cancel-btn"
            onClick={onClose}
            disabled={isDeleting}
            className="px-4 py-2 text-sm font-medium text-zinc-300 hover:text-zinc-100 transition-colors disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            data-ui-id="dialog-batch-delete-confirm-btn"
            onClick={onConfirm}
            disabled={isDeleting}
            className="flex items-center gap-2 px-4 py-2 text-sm font-medium bg-red-600 hover:bg-red-500 text-white rounded-md transition-colors disabled:opacity-50"
          >
            {isDeleting ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Deleting...
              </>
            ) : (
              <>
                Delete {itemCount} {itemType}
                {pluralSuffix}
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
