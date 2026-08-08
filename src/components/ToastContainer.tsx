/**
 * ToastContainer
 *
 * Renders a stack of toast notifications fixed to the bottom-right.
 * Pairs with the useToast hook.
 */

import { X } from "lucide-react";
import type { Toast } from "@/hooks/useToast";

interface ToastContainerProps {
  toasts: Toast[];
  onDismiss: (id: string) => void;
}

const typeStyles: Record<Toast["type"], string> = {
  success: "border-green-500/60 bg-green-500/10 text-green-400",
  error: "border-red-500/60 bg-red-500/10 text-red-400",
  info: "border-blue-500/60 bg-blue-500/10 text-blue-400",
};

export function ToastContainer({ toasts, onDismiss }: ToastContainerProps) {
  if (toasts.length === 0) return null;

  return (
    <div className="fixed bottom-4 right-4 z-toast flex flex-col gap-2 max-w-sm">
      {toasts.map((toast) => (
        <div
          key={toast.id}
          className={`flex items-start gap-2 rounded-lg border px-4 py-3 shadow-lg bg-surface-primary text-text-primary text-sm animate-in slide-in-from-right-5 fade-in duration-200 ${typeStyles[toast.type]}`}
        >
          <div className="flex-1 min-w-0">
            <span className="block">{toast.message}</span>
            {/* Optional deep link (see `ToastAction`). Rendered as a button
                rather than an anchor because every destination inside this app
                is a tab switch, not a URL. */}
            {toast.action && (
              <button
                onClick={() => {
                  toast.action?.onClick();
                  onDismiss(toast.id);
                }}
                className="mt-1.5 text-xs font-medium underline underline-offset-2 hover:opacity-80 transition-opacity"
              >
                {toast.action.label}
              </button>
            )}
          </div>
          <button
            onClick={() => onDismiss(toast.id)}
            className="shrink-0 rounded p-0.5 hover:bg-surface-secondary transition-colors"
            aria-label="Dismiss"
          >
            <X className="size-3.5" />
          </button>
        </div>
      ))}
    </div>
  );
}
