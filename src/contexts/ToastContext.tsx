/**
 * ToastContext
 *
 * Exposes the app's ONE toast system (`useToast`, owned by `App`) to the tree
 * below it, so a hook deep in a feature can raise a toast without a prop being
 * threaded through every layer. It carries only `showToast`; the toast list and
 * its `ToastContainer` stay in `App`.
 *
 * `useShowToast` answers `null` outside the provider (unit tests, any future
 * root that does not mount one). Callers must treat that as "no toast surface",
 * never as an error.
 */

import { createContext, useContext, type ReactNode } from "react";
import type { ShowToastFn } from "@/hooks/useToast";

const ToastContext = createContext<ShowToastFn | null>(null);

export function ToastProvider({
  showToast,
  children,
}: {
  showToast: ShowToastFn;
  children: ReactNode;
}) {
  return <ToastContext.Provider value={showToast}>{children}</ToastContext.Provider>;
}

export function useShowToast(): ShowToastFn | null {
  return useContext(ToastContext);
}
