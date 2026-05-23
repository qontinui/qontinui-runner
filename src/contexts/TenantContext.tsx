/**
 * TenantContext — plan 2026-05-22-coord-native-session-coordination
 * §D12 + §Phase 4.
 *
 * Resolves the active tenant id for this machine and persists the
 * operator's choice in `~/.qontinui/machine.json::active_tenant_id`.
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
  /** Active tenant id pinned for this machine. `null` while loading
   * or when the operator hasn't paired yet. */
  activeTenantId: string | null;
  /** Where the active tenant was resolved from. Diagnostic surface for
   * the settings panel. */
  source: "machine.json" | "paired_user.json" | null;
  /** Tenants the operator belongs to. Phase 4 ships empty; Phase 5
   * dashboard populates this via a coord round-trip. */
  candidates: string[];
  /** True iff the runner UI should render a tenant switcher. Per D12,
   * single-tenant operators see no UI. */
  showSwitcher: boolean;
  /** Persist the operator's choice to machine.json. Async — best-effort
   * local state update is immediate; the Rust side is the source of
   * truth on next mount. */
  setActiveTenant: (tenantId: string) => Promise<void>;
  /** Re-pull from Rust. */
  refresh: () => Promise<void>;
}

const TenantContext = createContext<TenantContextValue | null>(null);

interface TenantProviderProps {
  children: ReactNode;
}

export function TenantProvider({ children }: TenantProviderProps) {
  const [activeTenantId, setActiveTenantId] = useState<string | null>(null);
  const [source, setSource] = useState<TenantContextValue["source"]>(null);
  const [candidates, setCandidates] = useState<string[]>([]);

  const refresh = useCallback(async () => {
    try {
      const resp = await invoke<CommandResponse<GetActiveTenantResponse>>(
        "get_active_tenant",
      );
      const data = resp?.data;
      setActiveTenantId(data?.active_tenant_id ?? null);
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
    void refresh();
  }, [refresh]);

  const setActiveTenant = useCallback(
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
        throw new Error(String(e));
      }
      // Optimistic update; refresh reconciles.
      setActiveTenantId(trimmed);
      setSource("machine.json");
      void refresh();
    },
    [refresh],
  );

  const showSwitcher = candidates.length > 1;

  const value = useMemo<TenantContextValue>(
    () => ({
      activeTenantId,
      source,
      candidates,
      showSwitcher,
      setActiveTenant,
      refresh,
    }),
    [activeTenantId, source, candidates, showSwitcher, setActiveTenant, refresh],
  );

  return <TenantContext.Provider value={value}>{children}</TenantContext.Provider>;
}

/**
 * Hook into the active-tenant resolver. Throws when called outside a
 * `TenantProvider`.
 */
export function useTenant(): TenantContextValue {
  const ctx = useContext(TenantContext);
  if (!ctx) {
    throw new Error("useTenant must be used within a TenantProvider");
  }
  return ctx;
}
