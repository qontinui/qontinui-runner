/**
 * WindowAssignmentsContext — Phase 1 pop-out terminal windows.
 *
 * One runner process can host multiple OS windows ("main" + "term-N" pop-outs),
 * each rendering its own subset of terminal tabs. This context is each window's
 * live view of "which window owns which session", sourced from the Rust
 * `window_assignments` state.
 *
 * - On mount it reads `getCurrentWindow().label` (the window identity) and
 *   fetches the assignment snapshot (`get_window_assignments`).
 * - It subscribes to `session-assignment-changed` (broadcast to all windows) and
 *   keeps the local owner map current, so a moved tab is dropped from its old
 *   window and claimed by its new one.
 *
 * BACKWARD-COMPATIBLE: with a single ("main") window and no assignments,
 * `ownerOf` returns "main" for everything and `isOwned` is true for every tab —
 * so the main window renders all tabs exactly as before.
 */

import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  useCallback,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

export const MAIN_WINDOW_LABEL = "main";

/** Map of sessionId/terminalId → owning window label. */
export type SessionOwnerMap = Record<string, string>;

interface WindowAssignmentsValue {
  /** This window's own label ("main" | "term-N"). */
  windowLabel: string;
  /** Live sessionId → owning-window map (sessions absent default to "main"). */
  assignments: SessionOwnerMap;
  /** Owning window of a session, defaulting to "main". */
  ownerOf: (sessionId: string) => string;
  /** Whether a session is owned by THIS window. */
  isOwned: (sessionId: string) => boolean;
}

function readWindowLabel(): string {
  try {
    return getCurrentWindow().label || MAIN_WINDOW_LABEL;
  } catch {
    // Non-Tauri env (tests / SSR) — behave as the single main window.
    return MAIN_WINDOW_LABEL;
  }
}

const WindowAssignmentsContext = createContext<WindowAssignmentsValue | null>(null);

interface SessionAssignmentChanged {
  session_id: string;
  from?: string | null;
  to: string;
}

interface WindowAssignmentsSnapshot {
  session_owner?: SessionOwnerMap;
}

export function WindowAssignmentsProvider({ children }: { children: ReactNode }) {
  // Stable for the window's lifetime (useState initializer, not a ref — so it's
  // a plain render-safe value).
  const [windowLabel] = useState<string>(readWindowLabel);
  const [assignments, setAssignments] = useState<SessionOwnerMap>({});

  useEffect(() => {
    let mounted = true;

    // Hydrate from the persisted snapshot.
    invoke<WindowAssignmentsSnapshot>("get_window_assignments")
      .then((snap) => {
        if (mounted && snap?.session_owner) setAssignments(snap.session_owner);
      })
      .catch(() => {
        // Command unavailable (older backend) — stay single-window safe.
      });

    // Track moves. "to === main" means the entry is cleared (default rule).
    const unlistenPromise = listen<SessionAssignmentChanged>(
      "session-assignment-changed",
      (event) => {
        if (!mounted) return;
        const { session_id, to } = event.payload;
        setAssignments((prev) => {
          const next = { ...prev };
          if (to === MAIN_WINDOW_LABEL) {
            delete next[session_id];
          } else {
            next[session_id] = to;
          }
          return next;
        });
      },
    );

    return () => {
      mounted = false;
      unlistenPromise.then((un) => un()).catch(() => {});
    };
  }, []);

  const ownerOf = useCallback(
    (sessionId: string): string => assignments[sessionId] ?? MAIN_WINDOW_LABEL,
    [assignments],
  );

  const isOwned = useCallback(
    (sessionId: string): boolean => ownerOf(sessionId) === windowLabel,
    [ownerOf, windowLabel],
  );

  const value = useMemo<WindowAssignmentsValue>(
    () => ({ windowLabel, assignments, ownerOf, isOwned }),
    [windowLabel, assignments, ownerOf, isOwned],
  );

  return (
    <WindowAssignmentsContext value={value}>{children}</WindowAssignmentsContext>
  );
}

/**
 * Read the window-assignments context. When used OUTSIDE a provider (e.g. a
 * test, or a code path that predates Phase 1) it returns a safe single-window
 * default: label "main", everything owned by "main".
 */
export function useWindowAssignments(): WindowAssignmentsValue {
  const ctx = useContext(WindowAssignmentsContext);
  const fallback = useMemo<WindowAssignmentsValue>(
    () => ({
      windowLabel: readWindowLabel(),
      assignments: {},
      ownerOf: () => MAIN_WINDOW_LABEL,
      isOwned: () => true,
    }),
    [],
  );
  return ctx ?? fallback;
}
