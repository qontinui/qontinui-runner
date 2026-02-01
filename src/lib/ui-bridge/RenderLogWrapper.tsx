/**
 * RenderLogWrapper
 *
 * A React component wrapper that enables ui-bridge render log functionality.
 * Replaces the old RenderLogProvider from contexts.
 */

import type { ReactNode } from "react";
import { useRenderLogManager } from "./useRenderLogManager";

export interface RenderLogWrapperProps {
  children: ReactNode;
  /** Current active tab */
  activeTab: string;
  /** Task run ID */
  taskRunId?: string | number;
  /** Enable on mount */
  enableOnMount?: boolean;
  /** Enable mutation observer */
  enableMutationObserver?: boolean;
  /** Mutation debounce time (ms) */
  mutationDebounceMs?: number;
}

/**
 * Wrapper component that enables render log capturing via ui-bridge.
 *
 * This is a drop-in replacement for the old RenderLogProvider context.
 * It uses the useRenderLogManager hook internally.
 */
export function RenderLogWrapper({
  children,
  activeTab,
  taskRunId,
  enableOnMount = true,
  enableMutationObserver = true,
}: RenderLogWrapperProps) {
  // Use the render log manager hook
  useRenderLogManager({
    enabled: enableOnMount,
    activeTab,
    taskRunId,
    captureOnNavigation: true,
    captureChanges: enableMutationObserver,
    interactiveOnly: false,
    includeHidden: false,
    maxEntries: 1000,
  });

  // Simply render children - the hook handles all the capture logic
  return <>{children}</>;
}
