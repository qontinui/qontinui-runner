/**
 * F2 (plan `2026-07-08-runner-multi-tenant-session-ux`) — tenant picker for
 * the spawn moment, defaulted by repo→tenant inference.
 *
 * Spawn is the ONE moment a session's tenant is mutable: it is stamped onto
 * `Intent.tenant_id` at creation and immortal thereafter. So this control is
 * explicitly a "tenant for the NEXT session" selector, not a live switch —
 * changing it never migrates a running session, and the label says so.
 *
 * Default selection comes from the repo the operator is standing in: coord
 * maps repos to tenants, so opening a `portofino-pizzeria` checkout
 * pre-selects that tenant. Inference is a smart DEFAULT, never a hard lock —
 * the operator can always override, and an unreachable coord degrades
 * SILENTLY to the device default (no error surface: a coord hiccup must not
 * interrupt a spawn).
 *
 * Renders NOTHING when the device has <= 1 binding (`showSwitcher`), per the
 * plan's D12 no-clutter rule — a single-tenant operator never sees tenant UI.
 * On current `main` `candidates` ships empty, so this is inert until the
 * bindings-population change lands; that is the intended degradation.
 *
 * Mounted in `CommandBar` (the always-visible spawn console) and in
 * `ZoneControlPanel`'s footer next to "New Session" (the click path), so both
 * live spawn surfaces show the same selection.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Building2 } from "lucide-react";

import { useTenant } from "@/contexts/TenantContext";
import { createLogger } from "@/lib/logger";

const logger = createLogger("SpawnTenantPicker");

/**
 * Resolve which tenant a spawn should bind to.
 *
 * Precedence: the operator's explicit override wins; else the repo→tenant
 * inference; else the device's default tenant for new sessions. Returns `undefined` when nothing
 * is known (unpaired device) — callers omit `tenant_id` and Rust stamps its
 * own default, which is the pre-F2 behavior.
 *
 * An inferred tenant the device is NOT bound to is discarded: coord may know
 * a repo belongs to a tenant this device has no binding (and therefore no
 * credential) for, and pre-selecting it would produce a session that cannot
 * authenticate. Falling back to the device default is the honest default.
 *
 * Pure + exported so the precedence contract is unit-testable without
 * rendering (the runner's vitest config is `environment: "node"`).
 */
export function resolveSpawnTenant(args: {
  override?: string | null;
  inferred?: string | null;
  defaultForNewSessions?: string | null;
  candidates: readonly string[];
}): string | undefined {
  const { override, inferred, defaultForNewSessions, candidates } = args;
  const bound = (id: string | null | undefined): id is string =>
    Boolean(id) && candidates.includes(id as string);
  if (bound(override)) return override;
  if (bound(inferred)) return inferred;
  return defaultForNewSessions ?? undefined;
}

/**
 * Page-level resolution of the tenant a spawn SENDS — used by EVERY spawn path
 * on the terminal page (console `/spawn`/`/spawn-ai --tenant`, the New Session
 * button, the launch-menu `create-*` actions, the profile-load auto-fill, and
 * Ctrl+Shift+T).
 *
 * Only an EXPLICIT choice is a spawn tenant: the per-invocation tenant (the
 * `/spawn-ai --tenant` flag), else the tenant the operator actually picked in
 * {@link SpawnTenantPicker} (`spawnTenantId`, which the picker publishes ONLY for
 * a pick — see {@link explicitSpawnTenant}). The repo inference and the device
 * default are DISPLAYED by the picker but never sent. With nothing chosen this
 * returns `undefined`, the caller omits `tenant_id`, and Rust stamps its own
 * device default.
 *
 * Why (plan 2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential,
 * re-review F1): the runner now REFUSES a spawn tenant it cannot present a
 * credential for, fail-closed. Sending the default on every spawn made a plain
 * shell tab refuse with `tenant_not_paired` after `sign_out_full` (slots
 * cleared, `paired_user.json` kept) or on a store with no tenant slot — a spawn
 * refused for a tenant it never asked for.
 *
 * Pure + exported so the precedence contract is unit-testable without
 * rendering the page (the runner's vitest config is `environment: "node"`).
 */
export function pickSpawnTenant(args: {
  explicit?: string;
  spawnTenantId?: string | null;
}): string | undefined {
  const { explicit, spawnTenantId } = args;
  return explicit?.trim() || spawnTenantId?.trim() || undefined;
}

/**
 * What {@link SpawnTenantPicker} publishes as `spawnTenantId`: the operator's
 * own pick for the current cwd, when it is a tenant this device is bound to —
 * and nothing else. The inferred and default tenants are only displayed.
 */
export function explicitSpawnTenant(args: {
  override?: string | null;
  candidates: readonly string[];
}): string | null {
  const { override, candidates } = args;
  return override && candidates.includes(override) ? override : null;
}

/** Short display form for a tenant id (uuids share a long tail). */
export function shortTenantId(tenantId: string): string {
  return tenantId.length > 8 ? tenantId.slice(0, 8) : tenantId;
}

interface SpawnTenantPickerProps {
  /**
   * Working directory the inference reads. Rust resolves it to an
   * `owner/name` slug via `git remote get-url origin` and asks coord which
   * tenant owns it. Undefined → no inference, selection falls back to the
   * default tenant for new sessions.
   */
  cwd?: string;
  /** Extra classes for per-surface sizing. */
  className?: string;
}

export function SpawnTenantPicker({ cwd, className }: SpawnTenantPickerProps) {
  const {
    defaultTenantIdForNewSessions,
    candidates,
    showSwitcher,
    spawnTenantId,
    setSpawnTenantId,
  } = useTenant();
  const [inferred, setInferred] = useState<string | null>(null);
  /**
   * The cwd whose inference the operator has already overridden. Moving to a
   * different repo re-arms inference (the new repo's tenant is a fresh, more
   * relevant default); staying put keeps the operator's choice sticky.
   */
  const [overriddenForCwd, setOverriddenForCwd] = useState<string | null>(null);
  const override = overriddenForCwd === (cwd ?? "") ? spawnTenantId : null;

  // Repo→tenant inference. Silent on every failure path: `tenant_for_repo`
  // already returns Ok(None) for an unreachable coord, and a thrown invoke
  // (command missing on an older backend) is logged, not surfaced.
  useEffect(() => {
    if (!showSwitcher) return;
    let cancelled = false;
    void (async () => {
      try {
        const result = await invoke<string | null>("tenant_for_repo", {
          repo: null,
          workingDir: cwd ?? null,
        });
        if (!cancelled) setInferred(result ?? null);
      } catch (e) {
        if (!cancelled) setInferred(null);
        logger.warn(`tenant_for_repo failed: ${e}`);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [cwd, showSwitcher]);

  const resolved = resolveSpawnTenant({
    override,
    inferred,
    defaultForNewSessions: defaultTenantIdForNewSessions,
    candidates,
  });

  // Publish ONLY an explicit pick for the page's spawn handlers (see
  // `pickSpawnTenant`); the resolved value — inference or device default — is
  // what the select DISPLAYS. The onChange below and this effect are the only
  // writers of `spawnTenantId`.
  const published = explicitSpawnTenant({ override, candidates });
  useEffect(() => {
    setSpawnTenantId(published);
  }, [published, setSpawnTenantId]);

  if (!showSwitcher) return null;

  return (
    <label
      className={`flex items-center gap-1 shrink-0 text-[10px] text-[#565f89] ${className ?? ""}`}
      title={
        "Tenant for the NEXT session spawned from this page. " +
        "Defaults to the tenant coord associates with this repo. " +
        "A running session's tenant never changes."
      }
    >
      <Building2 className="w-3 h-3 text-[#7aa2f7]" aria-hidden />
      <select
        data-ui-bridge-id="terminal.spawn-tenant-picker"
        aria-label="Tenant for the next spawned session"
        value={resolved ?? ""}
        onChange={(e) => {
          setOverriddenForCwd(cwd ?? "");
          setSpawnTenantId(e.target.value || null);
        }}
        className="bg-transparent text-[10px] text-[#7aa2f7] font-mono outline-none cursor-pointer hover:text-[#c0caf5]"
      >
        {candidates.map((id) => (
          <option key={id} value={id} className="bg-[#1a1b26] text-[#c0caf5]">
            {shortTenantId(id)}
            {id === inferred ? " (repo)" : ""}
          </option>
        ))}
      </select>
    </label>
  );
}
