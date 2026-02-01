/**
 * ConfirmDialog.tsx
 *
 * Reusable confirmation dialog for actions that require user confirmation.
 * Consistent styling with the runner's design system.
 */

import { AlertTriangle, Info, X, Loader2 } from "lucide-react";

type DialogVariant = "warning" | "info" | "danger";

interface ConfirmDialogProps {
  /** Whether the dialog is open */
  open: boolean;
  /** Title for the dialog */
  title: string;
  /** Main message/description */
  message: string;
  /** Optional secondary description */
  description?: string;
  /** Visual variant of the dialog */
  variant?: DialogVariant;
  /** Text for the confirm button */
  confirmText?: string;
  /** Text for the cancel button */
  cancelText?: string;
  /** Whether an action is in progress */
  isLoading?: boolean;
  /** Called when the dialog is closed (cancel or backdrop click) */
  onClose: () => void;
  /** Called when user confirms */
  onConfirm: () => void;
}

const variantStyles: Record<DialogVariant, {
  headerBg: string;
  iconColor: string;
  titleColor: string;
  buttonBg: string;
  buttonHover: string;
  Icon: typeof AlertTriangle | typeof Info;
}> = {
  warning: {
    headerBg: "bg-amber-500/10",
    iconColor: "text-amber-400",
    titleColor: "text-amber-400",
    buttonBg: "bg-amber-600",
    buttonHover: "hover:bg-amber-500",
    Icon: AlertTriangle,
  },
  info: {
    headerBg: "bg-blue-500/10",
    iconColor: "text-blue-400",
    titleColor: "text-blue-400",
    buttonBg: "bg-blue-600",
    buttonHover: "hover:bg-blue-500",
    Icon: Info,
  },
  danger: {
    headerBg: "bg-red-500/10",
    iconColor: "text-red-400",
    titleColor: "text-red-400",
    buttonBg: "bg-red-600",
    buttonHover: "hover:bg-red-500",
    Icon: AlertTriangle,
  },
};

export function ConfirmDialog({
  open,
  title,
  message,
  description,
  variant = "warning",
  confirmText = "Confirm",
  cancelText = "Cancel",
  isLoading = false,
  onClose,
  onConfirm,
}: ConfirmDialogProps) {
  if (!open) return null;

  const styles = variantStyles[variant];
  const { Icon } = styles;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div
        className="absolute inset-0 bg-black/50"
        onClick={isLoading ? undefined : onClose}
      />

      {/* Dialog */}
      <div
        data-ui-id="dialog-confirm"
        className="relative bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl w-full max-w-md mx-4 overflow-hidden"
      >
        {/* Header */}
        <div className={`flex items-center justify-between px-6 py-4 border-b border-zinc-700 ${styles.headerBg}`}>
          <div className="flex items-center gap-2">
            <Icon className={`w-5 h-5 ${styles.iconColor}`} />
            <h3 className={`text-lg font-semibold ${styles.titleColor}`}>{title}</h3>
          </div>
          <button
            data-ui-id="dialog-confirm-close-btn"
            onClick={onClose}
            disabled={isLoading}
            className="p-1 text-zinc-400 hover:text-zinc-200 transition-colors disabled:opacity-50"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="p-6 space-y-3">
          <p className="text-zinc-300">{message}</p>
          {description && (
            <p className="text-sm text-zinc-400">{description}</p>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-3 px-6 py-4 border-t border-zinc-700 bg-zinc-800/50">
          <button
            data-ui-id="dialog-confirm-cancel-btn"
            onClick={onClose}
            disabled={isLoading}
            className="px-4 py-2 text-sm font-medium text-zinc-300 hover:text-zinc-100 transition-colors disabled:opacity-50"
          >
            {cancelText}
          </button>
          <button
            data-ui-id="dialog-confirm-action-btn"
            onClick={onConfirm}
            disabled={isLoading}
            className={`flex items-center gap-2 px-4 py-2 text-sm font-medium ${styles.buttonBg} ${styles.buttonHover} text-white rounded-md transition-colors disabled:opacity-50`}
          >
            {isLoading ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Loading...
              </>
            ) : (
              confirmText
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
