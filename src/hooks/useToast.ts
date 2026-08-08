/**
 * useToast Hook
 *
 * Lightweight toast notification system. No external dependencies.
 * Pattern follows useTriggers.ts toast implementation but extracted
 * as a reusable, standalone hook.
 */

import { useState, useCallback } from "react";

export type ToastType = "success" | "error" | "info";

/**
 * Optional single action rendered inside the toast.
 *
 * Added for the spawn-time resource guard (plan
 * `2026-08-07-runner-resource-guard-and-session-protection` §Part D), whose
 * warn notice has to *link into* `Settings > Resource Guard` — a toast that
 * names a settings page the reader then has to go find by hand is a strictly
 * worse version of the same message. Kept to exactly one action: this is a
 * self-dismissing notification, not a dialog, and a second button would make it
 * a decision surface it has no business being.
 */
export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface Toast {
  id: string;
  type: ToastType;
  message: string;
  action?: ToastAction;
}

export type ShowToastFn = (message: string, type: ToastType, action?: ToastAction) => void;

/** Auto-dismiss for a plain toast. */
const DISMISS_MS = 4000;
/**
 * Auto-dismiss for a toast that carries an action. Longer because 4 s is about
 * how long it takes to *read* an actionable message, which leaves no time to
 * reach for the button — a link nobody can click is not a link.
 */
const ACTION_DISMISS_MS = 10000;

let toastCounter = 0;

export function useToast() {
  const [toasts, setToasts] = useState<Toast[]>([]);

  const showToast: ShowToastFn = useCallback(
    (message: string, type: ToastType, action?: ToastAction) => {
      const id = `toast-${++toastCounter}`;
      setToasts((prev) => [...prev, { id, type, message, action }]);
      setTimeout(
        () => {
          setToasts((prev) => prev.filter((t) => t.id !== id));
        },
        action ? ACTION_DISMISS_MS : DISMISS_MS,
      );
    },
    [],
  );

  const dismissToast = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  return { toasts, showToast, dismissToast };
}
