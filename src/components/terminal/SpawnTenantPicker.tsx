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
 * the operator can always override, and no inference outcome blocks or
 * delays a spawn. What the inference could NOT establish is still SAID, as a
 * short note beside the select (no repo / no tenant / several / unknown) —
 * `tenant_for_repo` used to answer `null` for all of those and for "coord
 * unreachable" alike, so the picker showed nothing and the operator could not
 * tell "this repo has no tenant" from "we could not look" (plan
 * `2026-09-20-a-sessions-tenant-follows-its-repo-and-every-coord-answer-names-its-tenant`
 * Phase 2).
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

/** One `<option>` of the picker. */
export interface SpawnTenantOption {
  value: string;
  label: string;
}

/**
 * What the picker SHOWS, derived from what it SENDS (re-review P1). The select's
 * value is the published explicit pick, or `""` — a leading "device default"
 * option — when there is none, so the display always matches what a spawn will
 * carry: nothing picked shows "device default (A)" and sends no tenant. The
 * repo inference is a hint on its option's label ("B (repo)"), never a
 * pre-selected value; choosing it is an explicit pick like any other.
 */
export function spawnTenantPickerModel(args: {
  override?: string | null;
  inferred?: string | null;
  defaultForNewSessions?: string | null;
  candidates: readonly string[];
}): { value: string; published: string | null; options: SpawnTenantOption[] } {
  const { override, inferred, defaultForNewSessions, candidates } = args;
  const published = explicitSpawnTenant({ override, candidates });
  const options: SpawnTenantOption[] = [
    {
      value: "",
      label: defaultForNewSessions
        ? `device default (${shortTenantId(defaultForNewSessions)})`
        : "device default",
    },
    ...candidates.map((id) => ({
      value: id,
      label: `${shortTenantId(id)}${id === inferred ? " (repo)" : ""}`,
    })),
  ];
  return { value: published ?? "", published, options };
}

/** What `tenant_for_repo` answers: the tagged Rust `CwdTenant`. */
export type RepoTenantAnswer =
  | { state: "resolved"; tenantId: string; repo: string; source: string; observedAt: string }
  | { state: "no_repo" }
  | { state: "repo_unregistered"; repo: string }
  | { state: "several"; repo: string; tenantIds: string[] }
  | { state: "unknown"; reason: string; transient?: boolean };

/** The picker's reading of one `tenant_for_repo` answer. */
export interface RepoTenantHint {
  /** The inferred tenant to label "(repo)" — only for a single owner. */
  inferred: string | null;
  /** Short text shown beside the select; `null` when the inference speaks for
   * itself (a resolved tenant this device is bound to). */
  note: string | null;
  /** The long form, for the note's tooltip. */
  detail: string | null;
}

/**
 * Read a `tenant_for_repo` answer. Every arm that is not a usable single owner
 * becomes a short NOTE rather than nothing — the arms mean different things
 * and "coord could not be asked" must never look like "this repo has no
 * tenant". `undefined` (not asked yet) claims nothing. Tolerates an older
 * runner's bare `string | null` answer. Pure +
 * exported for the unit test.
 */
export function repoTenantHint(answer: unknown, candidates: readonly string[]): RepoTenantHint {
  // Not asked yet (the first read is in flight): claim nothing.
  if (answer === undefined) return { inferred: null, note: null, detail: null };
  if (typeof answer === "string") return { inferred: answer, note: null, detail: null };
  if (!answer || typeof answer !== "object") {
    return {
      inferred: null,
      note: "repo: unknown",
      detail:
        "The runner did not say which tenant this repo belongs to (an older runner answers only a tenant or nothing).",
    };
  }
  const a = answer as Partial<Record<string, unknown>> & { state?: unknown };
  const text = (v: unknown): string => (typeof v === "string" && v ? v : "?");
  switch (a.state) {
    case "resolved": {
      const id = text(a.tenantId);
      if (candidates.includes(id)) return { inferred: id, note: null, detail: null };
      return {
        inferred: id,
        note: `repo: ${shortTenantId(id)} (not paired)`,
        detail: `Repo ${text(a.repo)} belongs to tenant ${id}, which this device is not paired for — a session here would read another tenant's coord.`,
      };
    }
    case "no_repo":
      return {
        inferred: null,
        note: "no repo",
        detail: "The working directory is not inside a git checkout, so no tenant is implied.",
      };
    case "repo_unregistered":
      return {
        inferred: null,
        note: "repo: no tenant",
        detail: `Repo ${text(a.repo)} has no owning tenant in coord's registry.`,
      };
    case "several": {
      const ids = Array.isArray(a.tenantIds)
        ? a.tenantIds.filter((t): t is string => typeof t === "string")
        : [];
      return {
        inferred: null,
        note: `repo: ${ids.length} tenants`,
        detail: `Repo ${text(a.repo)} is registered to several tenants (${ids.join(", ")}) — pick one.`,
      };
    }
    default:
      return {
        inferred: null,
        note: "repo: unknown",
        detail: `Which tenant this repo belongs to could not be determined (${text(a.reason)}). Not the same as "no tenant".`,
      };
  }
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
  /**
   * The last `tenant_for_repo` answer AND the cwd it was for. An answer for a
   * different cwd is never shown: after a move the hint reads `undefined`
   * (nothing claimed) until the new repo's answer arrives, rather than the old
   * repo's note sitting beside the new cwd.
   */
  const [answered, setAnswered] = useState<{ cwd: string; answer: unknown } | null>(null);
  const answer = answered && answered.cwd === (cwd ?? "") ? answered.answer : undefined;
  /**
   * The cwd whose inference the operator has already overridden. Moving to a
   * different repo re-arms inference (the new repo's tenant is a fresh, more
   * relevant default); staying put keeps the operator's choice sticky.
   */
  const [overriddenForCwd, setOverriddenForCwd] = useState<string | null>(null);
  const override = overriddenForCwd === (cwd ?? "") ? spawnTenantId : null;

  // Repo→tenant inference. Never blocks a spawn; every outcome that is not a
  // usable single owner is rendered as a short note (see `repoTenantHint`). A
  // thrown invoke (command missing on an older backend) reads as unknown.
  useEffect(() => {
    if (!showSwitcher) return;
    let cancelled = false;
    void (async () => {
      try {
        const result = await invoke<RepoTenantAnswer | string | null>("tenant_for_repo", {
          repo: null,
          workingDir: cwd ?? null,
        });
        if (!cancelled) setAnswered({ cwd: cwd ?? "", answer: result ?? null });
      } catch (e) {
        if (!cancelled)
          setAnswered({
            cwd: cwd ?? "",
            answer: { state: "unknown", reason: `tenant_for_repo failed: ${e}` },
          });
        logger.warn(`tenant_for_repo failed: ${e}`);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [cwd, showSwitcher]);
  const hint = repoTenantHint(answer, candidates);
  const inferred = hint.inferred;

  // Publish ONLY an explicit pick for the page's spawn handlers (see
  // `pickSpawnTenant`), and DISPLAY exactly that (see `spawnTenantPickerModel`):
  // no pick shows "device default", the repo inference is a label hint. The
  // onChange below and this effect are the only writers of `spawnTenantId`.
  const model = spawnTenantPickerModel({
    override,
    inferred,
    defaultForNewSessions: defaultTenantIdForNewSessions,
    candidates,
  });
  const published = model.published;
  useEffect(() => {
    setSpawnTenantId(published);
  }, [published, setSpawnTenantId]);

  if (!showSwitcher) return null;

  return (
    <label
      className={`flex items-center gap-1 shrink-0 text-[10px] text-[#565f89] ${className ?? ""}`}
      title={
        "Tenant for the NEXT session spawned from this page. " +
        '"device default" sends no tenant, and the runner uses this device\'s default. ' +
        '"(repo)" marks the tenant coord associates with this repo — a hint; pick it to use it. ' +
        "A running session's tenant never changes."
      }
    >
      <Building2 className="w-3 h-3 text-[#7aa2f7]" aria-hidden />
      <select
        data-ui-bridge-id="terminal.spawn-tenant-picker"
        aria-label="Tenant for the next spawned session"
        value={model.value}
        onChange={(e) => {
          setOverriddenForCwd(cwd ?? "");
          setSpawnTenantId(e.target.value || null);
        }}
        className="bg-transparent text-[10px] text-[#7aa2f7] font-mono outline-none cursor-pointer hover:text-[#c0caf5]"
      >
        {model.options.map((option) => (
          <option
            key={option.value || "device-default"}
            value={option.value}
            className="bg-[#1a1b26] text-[#c0caf5]"
          >
            {option.label}
          </option>
        ))}
      </select>
      {hint.note && (
        <span
          data-ui-bridge-id="terminal.spawn-tenant-repo-hint"
          className="font-mono text-[9px] text-[#565f89]"
          title={hint.detail ?? undefined}
        >
          {hint.note}
        </span>
      )}
    </label>
  );
}
