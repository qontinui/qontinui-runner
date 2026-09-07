/**
 * TenantContext — plan 2026-05-22-coord-native-session-coordination
 * §D12 + §Phase 4.
 *
 * Resolves this machine's DEFAULT tenant for NEW sessions and persists the
 * operator's choice in `~/.qontinui/machine.json::active_tenant_id`. The
 * on-disk key keeps its historical name; the in-process API does not, because
 * "active tenant" reads as "the runner's tenant" and no such thing exists on a
 * device bound to more than one.
 * Wraps the `get_active_tenant` / `set_active_tenant` Tauri commands
 * added in this PR (`commands/tenant.rs`).
 *
 * Conditional rendering: when the operator belongs to exactly one tenant
 * (`candidates.length <= 1`), the switcher does NOT render — Phase 4's
 * D12 is explicit that the single-tenant case shows no UI. When the
 * operator belongs to multiple tenants, the runner header (and other
 * consumers) read `useTenant().showSwitcher` and render the switcher
 * accordingly. The `candidates` list ships empty in Phase 4 — Phase 5
 * dashboard populates it from a coord round-trip.
 *
 * Plan §D12 verbatim: "every session is stamped with its tenant at
 * start and keeps it for life — switching active tenant doesn't migrate
 * live sessions." This file ONLY tracks the *default* tenant for new
 * sessions; mutating it does not affect any running session.
 *
 * Wraps SessionProvider in the React tree:
 *
 *   <TenantProvider>
 *     <SessionProvider>
 *       {children}
 *     </SessionProvider>
 *   </TenantProvider>
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";

import { createLogger } from "@/lib/logger";

const logger = createLogger("TenantContext");

interface CommandResponse<T = unknown> {
  success: boolean;
  message?: string | null;
  data?: T | null;
}

interface GetActiveTenantResponse {
  active_tenant_id: string | null;
  source: "machine.json" | "paired_user.json" | null;
  candidates: string[];
}

export interface TenantContextValue {
  /**
   * The device's DEFAULT tenant for NEW sessions — never "the tenant this
   * runner belongs to", a concept that does not exist once a device holds N
   * concurrent tenant bindings (`coord.tenant_devices`). A session stamps its
   * tenant at spawn and keeps it for life, so changing this re-points FUTURE
   * spawns only; it never migrates a running session.
   *
   * It is also not how artifact tenancy is MEANT to be resolved — that is a
   * per-artifact rule keyed on the artifact's own repo. Note the Rust side is
   * weaker than "never": in `session_archive::tenancy` this pin is the
   * documented LAST-RESORT rung, and an attribution derived from it is
   * labelled `derived_sole_binding`, never `declared`.
   *
   * Persisted as `machine.json::active_tenant_id`; the on-disk key keeps its
   * name because it is a file format read by independent Rust consumers.
   * `src-tauri/src/commands/tenant.rs`'s module doc is the authority for that
   * key's full meaning — it is the default for new sessions AND for
   * device-level surfaces (heartbeat, census, backstop, maintenance,
   * doctor, flag-poll). This TS
   * symbol is named for the only half the frontend uses.
   *
   * `null` while loading, or when the operator hasn't paired yet.
   */
  defaultTenantIdForNewSessions: string | null;
  /** Where the default tenant was resolved from. Diagnostic surface for
   * the settings panel. */
  source: "machine.json" | "paired_user.json" | null;
  /** Tenants the operator belongs to. Phase 4 ships empty; Phase 5
   * dashboard populates this via a coord round-trip. */
  candidates: string[];
  /** True iff the runner UI should render a tenant switcher. Per D12,
   * single-tenant operators see no UI. */
  showSwitcher: boolean;
  /** Persist the operator's choice of DEFAULT tenant for new sessions to
   * machine.json. Async — best-effort local state update is immediate; the
   * Rust side is the source of truth on next mount. Running sessions are
   * unaffected. */
  setDefaultTenantForNewSessions: (tenantId: string) => Promise<void>;
  /** Re-pull from Rust. */
  refresh: () => Promise<void>;
  /**
   * F2 — the tenant the NEXT spawn will bind to. `null` means "nothing has
   * been resolved yet; fall back to `defaultTenantIdForNewSessions`".
   *
   * This is deliberately SEPARATE from `defaultTenantIdForNewSessions`: that
   * is the persisted device default (written to machine.json), whereas this is a
   * transient, un-persisted, per-spawn selection driven by repo→tenant
   * inference and the operator's picker. Setting it never writes machine.json
   * and — per D12 — never migrates a RUNNING session; a session's tenant is
   * stamped at spawn and immortal.
   *
   * Written by `SpawnTenantPicker` (the sole writer); read by the spawn
   * handlers on the terminal page.
   */
  spawnTenantId: string | null;
  /** Set the tenant for the next spawn. See {@link spawnTenantId}. */
  setSpawnTenantId: (tenantId: string | null) => void;
}

const TenantContext = createContext<TenantContextValue | null>(null);

interface TenantProviderProps {
  children: ReactNode;
}

export function TenantProvider({ children }: TenantProviderProps) {
  const [defaultTenantIdForNewSessions, setDefaultTenantIdForNewSessions] = useState<string | null>(
    null,
  );
  const [source, setSource] = useState<TenantContextValue["source"]>(null);
  const [candidates, setCandidates] = useState<string[]>([]);
  const [spawnTenantId, setSpawnTenantId] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const resp = await invoke<CommandResponse<GetActiveTenantResponse>>("get_active_tenant");
      const data = resp?.data;
      setDefaultTenantIdForNewSessions(data?.active_tenant_id ?? null);
      setSource(data?.source ?? null);
      setCandidates(data?.candidates ?? []);
    } catch (e) {
      // Non-fatal — runner still works without tenant resolution,
      // sessions just go out without the default-tenant stamp until
      // pair completes.
      logger.warn(`get_active_tenant failed: ${e}`);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- initial tenant load on mount; refresh is the explicit synchronization point with the backend
    void refresh();
  }, [refresh]);

  const setDefaultTenantForNewSessions = useCallback(
    async (tenantId: string): Promise<void> => {
      const trimmed = tenantId.trim();
      if (!trimmed) {
        throw new Error("tenant id cannot be empty");
      }
      try {
        await invoke<CommandResponse<{ active_tenant_id: string; source: string }>>(
          "set_active_tenant",
          { tenantId: trimmed },
        );
      } catch (e) {
        throw new Error(String(e), { cause: e });
      }
      // Optimistic update; refresh reconciles.
      setDefaultTenantIdForNewSessions(trimmed);
      setSource("machine.json");
      void refresh();
    },
    [refresh],
  );

  const showSwitcher = candidates.length > 1;

  const value = useMemo<TenantContextValue>(
    () => ({
      defaultTenantIdForNewSessions,
      source,
      candidates,
      showSwitcher,
      setDefaultTenantForNewSessions,
      refresh,
      spawnTenantId,
      setSpawnTenantId,
    }),
    [
      defaultTenantIdForNewSessions,
      source,
      candidates,
      showSwitcher,
      setDefaultTenantForNewSessions,
      refresh,
      spawnTenantId,
    ],
  );

  return <TenantContext.Provider value={value}>{children}</TenantContext.Provider>;
}

/**
 * Hook into the default-tenant resolver. Throws when called outside a
 * `TenantProvider`.
 */
export function useTenant(): TenantContextValue {
  const ctx = useContext(TenantContext);
  if (!ctx) {
    throw new Error("useTenant must be used within a TenantProvider");
  }
  return ctx;
}
