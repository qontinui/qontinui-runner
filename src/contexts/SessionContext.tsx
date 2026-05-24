/**
 * SessionContext — plan 2026-05-22-coord-native-session-coordination §Phase 4.
 *
 * Single source of truth on the runner frontend for the coord-native
 * Session primitive (Phase 2 PR #240). Wraps the new `session_*` Tauri
 * commands and exposes a flat React API:
 *
 *   - `sessions`     — list mirror from `session_list()`
 *   - `active`       — currently-focused descriptor (null until first focus)
 *   - `startSession` — `session_start(intent) → SessionDescriptor`
 *   - `focusSession` — `session_focus(id)` (also updates local `active`)
 *   - `closeSession` — `session_close(id)`
 *   - `stealSession` — `session_steal(id, reason)` (min 10 chars enforced
 *                      server-side; surfaced as `SessionContextError` here)
 *   - `subscribeEvents` — per-session Tauri-event subscription helper
 *
 * The wire types (SessionKind / SessionState / Intent / SessionDescriptor)
 * MIRROR `qontinui-runner/src-tauri/src/session/mod.rs` byte-for-byte —
 * `snake_case` serde rename on the Rust side, matching string literals
 * here. Changes to those Rust enums MUST land alongside an edit to this
 * file or the runtime payload deserializes silently to `undefined`.
 *
 * NOT a replacement for the existing terminal-page state (tabs, zone
 * layout, file conflicts, AI workflow generation). That lives in the
 * collapsed-into-one TerminalSession context under
 * `components/terminal/contexts/`. SessionContext is the COORD surface;
 * TerminalSession is the LOCAL terminal-page state.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { createLogger } from "@/lib/logger";

const logger = createLogger("SessionContext");

/** Mirrors `coord.sessions.session_kind` CHECK constraint exactly. */
export type SessionKind =
  | "terminal_shell"
  | "terminal_claude"
  | "agentic"
  | "workflow"
  | "automation"
  | "debug";

/** Mirrors `coord.sessions.state` CHECK constraint exactly. */
export type SessionState = "active" | "pending_resolution" | "stale" | "closed";

/** Mirrors `crate::session::SessionEventKind`. */
export type SessionEventKind =
  | "started"
  | "state_change"
  | "closed"
  | "heartbeat"
  | "claim_stolen"
  | "output_chunk"
  | "handoff_request";

/**
 * Typed intent — plan §D6. Mirrors `crate::session::Intent`. `purpose`
 * must be ≥3 chars after trim (validated server-side); the frontend MAY
 * pre-check to give a better error before the round-trip, but the
 * authoritative check is the Rust side.
 */
export interface Intent {
  kind: SessionKind;
  purpose: string;
  repo?: string | null;
  branch?: string | null;
  declared_paths?: string[];
  share_output?: boolean;
  redact_secrets?: boolean | null;
}

/**
 * Mirrors `crate::session::SessionDescription` — the subset the frontend
 * needs. Tenant + device are constant per machine and the frontend
 * already knows them.
 */
export interface SessionDescriptor {
  id: string;
  kind: SessionKind;
  state: SessionState;
  intent: Intent;
  /** ISO-8601 UTC. */
  started_at: string;
  /** ISO-8601 UTC or null. */
  last_heartbeat_at: string | null;
  /** ISO-8601 UTC or null. */
  closed_at: string | null;
  parent_session_id: string | null;
  /** Discriminator for the transport — "pty" | "claude_cli" | "workflow". */
  transport_handle_kind: "pty" | "claude_cli" | "workflow";
}

/**
 * Event payload broadcast over Tauri `session-*` events. The Rust side
 * doesn't fan these out yet (Phase 3 wires the SSE/JetStream bridge);
 * the listener here is laid down so that when the event surface lands
 * the consumers see refresh ticks without a callsite change.
 *
 * Shape is intentionally narrow — `kind`, `session_id`, optional `state`
 * for state-change deltas, and an opaque `payload` for everything else
 * (claim_stolen reason, output chunks, etc.). Frontend consumers should
 * branch on `kind` and pull from `payload` rather than guessing.
 */
export interface SessionEvent {
  kind: SessionEventKind;
  session_id: string;
  state?: SessionState;
  payload?: Record<string, unknown>;
  /** ISO-8601 UTC of the runner's local emission time. */
  emitted_at?: string;
}

/** Tauri envelope returned by the Rust commands. */
interface CommandResponse<T = unknown> {
  success: boolean;
  message?: string | null;
  data?: T | null;
}

/** Error raised by SessionContext callers. Wraps the Rust string. */
export class SessionContextError extends Error {
  /** Structured code prefix from `commands::session::map_err`. */
  code:
    | "session:intent_invalid"
    | "session:transport_error"
    | "session:reason_too_short"
    | "session:not_found"
    | "session:outbox_error"
    | "session:unknown";
  constructor(raw: string) {
    super(raw);
    const code = raw.split(":").slice(0, 2).join(":");
    if (
      code === "session:intent_invalid" ||
      code === "session:transport_error" ||
      code === "session:reason_too_short" ||
      code === "session:not_found" ||
      code === "session:outbox_error"
    ) {
      this.code = code;
    } else {
      this.code = "session:unknown";
    }
    this.name = "SessionContextError";
  }
}

function wrapInvokeError(e: unknown): SessionContextError {
  return new SessionContextError(typeof e === "string" ? e : String(e));
}

export interface SessionContextValue {
  sessions: SessionDescriptor[];
  active: SessionDescriptor | null;
  /**
   * Start a session via `session_start`. Returns the full descriptor
   * (the Rust side returns just `{id}`; we follow with `session_describe`
   * so callers don't have to chain the round-trip themselves).
   */
  startSession: (intent: Intent) => Promise<SessionDescriptor>;
  focusSession: (id: string) => Promise<void>;
  closeSession: (id: string) => Promise<void>;
  /** Plan §D14 — min 10 chars on reason; server enforces. */
  stealSession: (id: string, reason: string) => Promise<void>;
  /**
   * Plan §Phase 7 — "Continue elsewhere". Publish a handoff request so the
   * target device materializes the session and closes this one. Returns
   * once coord has accepted the request (the actual transfer is async).
   */
  handoffSession: (id: string, targetDeviceId: string) => Promise<void>;
  /** Re-fetch the full list. Local mirror is refreshed best-effort on every
   * mutation; this is the explicit knob when an external surface (CLI, peer
   * runner) might have changed coord state. */
  refresh: () => Promise<void>;
  /**
   * Subscribe to Tauri session events for a specific session id. Returns
   * an unsubscribe handle. Events also feed into the local mirror via
   * the same listener under the hood.
   */
  subscribeEvents: (id: string, cb: (e: SessionEvent) => void) => () => void;
}

const SessionContext = createContext<SessionContextValue | null>(null);

interface SessionProviderProps {
  children: ReactNode;
}

export function SessionProvider({ children }: SessionProviderProps) {
  const [sessions, setSessions] = useState<SessionDescriptor[]>([]);
  const [active, setActive] = useState<SessionDescriptor | null>(null);

  /** Per-session subscribers (in-process fanout from the global Tauri
   * listener). Map<session_id, Set<callback>>. */
  const perSessionSubs = useRef<Map<string, Set<(e: SessionEvent) => void>>>(
    new Map(),
  );

  /** Refresh from `session_list()`. Best-effort; failures log only. */
  const refresh = useCallback(async () => {
    try {
      const res = await invoke<CommandResponse<{ sessions: SessionDescriptor[] }>>(
        "session_list",
      );
      const list = res?.data?.sessions ?? [];
      setSessions(list);
      setActive((prev) => {
        if (!prev) return prev;
        const next = list.find((s) => s.id === prev.id);
        return next ?? null;
      });
    } catch (e) {
      logger.warn(`session_list failed: ${e}`);
    }
  }, []);

  // Initial list pull on mount.
  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Global Tauri-event listener — fans `session-*` events out to the
  // per-session in-process subscribers AND triggers a list refresh on
  // lifecycle deltas (started / closed / state_change). The
  // qontinui-coord SSE bridge lands in Phase 3+, but we lay the wiring
  // down now so Phase 4 callsites don't need a follow-up edit.
  useEffect(() => {
    let unlistenSession: UnlistenFn | null = null;

    void listen<SessionEvent>("session-event", (event) => {
      const payload = event.payload;
      if (!payload?.session_id) return;
      const subs = perSessionSubs.current.get(payload.session_id);
      if (subs) {
        for (const cb of subs) {
          try {
            cb(payload);
          } catch (err) {
            logger.warn(`session subscriber threw: ${err}`);
          }
        }
      }
      // Lifecycle-deltas → refresh the list mirror so consumers see the
      // new state without polling. `heartbeat` and `output_chunk` arrive
      // frequently and shouldn't trigger a full list refresh.
      if (
        payload.kind === "started" ||
        payload.kind === "closed" ||
        payload.kind === "state_change" ||
        payload.kind === "claim_stolen"
      ) {
        void refresh();
      }
    }).then((unlisten) => {
      unlistenSession = unlisten;
    });

    return () => {
      unlistenSession?.();
    };
  }, [refresh]);

  const startSession = useCallback(
    async (intent: Intent): Promise<SessionDescriptor> => {
      let startResp: CommandResponse<{ id: string }>;
      try {
        startResp = await invoke<CommandResponse<{ id: string }>>("session_start", {
          args: intent,
        });
      } catch (e) {
        throw wrapInvokeError(e);
      }
      const id = startResp?.data?.id;
      if (!id) {
        throw new SessionContextError("session:unknown: session_start returned no id");
      }
      // Follow with describe so the caller gets a full descriptor, then
      // refresh the list mirror.
      let descResp: CommandResponse<SessionDescriptor>;
      try {
        descResp = await invoke<CommandResponse<SessionDescriptor>>(
          "session_describe",
          { sessionId: id },
        );
      } catch (e) {
        throw wrapInvokeError(e);
      }
      const desc = descResp?.data;
      if (!desc) {
        throw new SessionContextError(
          "session:not_found: session_describe returned empty after start",
        );
      }
      // Optimistic local-mirror insert; refresh() will reconcile.
      setSessions((prev) => {
        if (prev.some((s) => s.id === desc.id)) return prev;
        return [...prev, desc];
      });
      // Don't await — best-effort.
      void refresh();
      return desc;
    },
    [refresh],
  );

  const focusSession = useCallback(
    async (id: string): Promise<void> => {
      try {
        await invoke<CommandResponse>("session_focus", { sessionId: id });
      } catch (e) {
        throw wrapInvokeError(e);
      }
      // Update local "active" pointer from the list mirror (or describe
      // if the list hasn't refreshed yet).
      setActive((prev) => {
        if (prev?.id === id) return prev;
        const fromList = sessions.find((s) => s.id === id);
        return fromList ?? prev;
      });
      void refresh();
    },
    [refresh, sessions],
  );

  const closeSession = useCallback(
    async (id: string): Promise<void> => {
      try {
        await invoke<CommandResponse>("session_close", { sessionId: id });
      } catch (e) {
        throw wrapInvokeError(e);
      }
      setActive((prev) => (prev?.id === id ? null : prev));
      // Optimistic remove; refresh reconciles.
      setSessions((prev) => prev.filter((s) => s.id !== id));
      void refresh();
    },
    [refresh],
  );

  const stealSession = useCallback(
    async (id: string, reason: string): Promise<void> => {
      // Best-effort frontend pre-check — the server enforces 10. Skip
      // the invoke entirely if the operator clearly didn't fill it.
      if (reason.trim().length < 10) {
        throw new SessionContextError(
          `session:reason_too_short: steal reason too short (${reason.trim().length} < 10 chars)`,
        );
      }
      try {
        await invoke<CommandResponse>("session_steal", {
          sessionId: id,
          reason,
        });
      } catch (e) {
        throw wrapInvokeError(e);
      }
      void refresh();
    },
    [refresh],
  );

  const handoffSession = useCallback(
    async (id: string, targetDeviceId: string): Promise<void> => {
      if (!targetDeviceId.trim()) {
        throw new SessionContextError(
          "session:intent_invalid: targetDeviceId is required",
        );
      }
      try {
        await invoke<CommandResponse>("session_handoff", {
          sessionId: id,
          targetDeviceId,
        });
      } catch (e) {
        throw wrapInvokeError(e);
      }
      // The source session transitions to `closed` once the target
      // materializes; refresh so the list mirror catches that delta.
      void refresh();
    },
    [refresh],
  );

  const subscribeEvents = useCallback(
    (id: string, cb: (e: SessionEvent) => void): (() => void) => {
      let set = perSessionSubs.current.get(id);
      if (!set) {
        set = new Set();
        perSessionSubs.current.set(id, set);
      }
      set.add(cb);
      return () => {
        const s = perSessionSubs.current.get(id);
        s?.delete(cb);
        if (s && s.size === 0) {
          perSessionSubs.current.delete(id);
        }
      };
    },
    [],
  );

  const value = useMemo<SessionContextValue>(
    () => ({
      sessions,
      active,
      startSession,
      focusSession,
      closeSession,
      stealSession,
      handoffSession,
      refresh,
      subscribeEvents,
    }),
    [
      sessions,
      active,
      startSession,
      focusSession,
      closeSession,
      stealSession,
      handoffSession,
      refresh,
      subscribeEvents,
    ],
  );

  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>;
}

/**
 * Hook into the unified session surface. Throws when called outside a
 * `SessionProvider` (matches the precedent of the other contexts under
 * `components/terminal/contexts/`).
 */
export function useSession(): SessionContextValue {
  const ctx = useContext(SessionContext);
  if (!ctx) {
    throw new Error("useSession must be used within a SessionProvider");
  }
  return ctx;
}
