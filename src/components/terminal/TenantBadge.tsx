/**
 * F1 (plan `2026-07-08-runner-multi-tenant-session-ux`) — per-session tenant
 * badge for the zone header.
 *
 * A session's tenant is IMMORTAL: it is stamped at spawn
 * (`Intent.tenant_id`, `session/intent.rs`) and never migrates, so switching
 * the device's active tenant does NOT move a running session. That makes the
 * operator risk *not knowing* which tenant a terminal is acting as — not the
 * switch itself. This badge is therefore display-only; it is deliberately NOT
 * a live switch.
 *
 * **It also says when the session is NOT acting as one tenant** (plan
 * `2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential` P0). A
 * session's tenant is decided by three mechanisms — the coord row stamped at
 * spawn, the runner's data-plane writes, and the coord-mcp credential its
 * memory / prompt-document / gate writes present — and a session labelled
 * tenant B has written to tenant A through the third. A fourth leg (plan
 * `2026-09-20-a-sessions-tenant-follows-its-repo-and-every-coord-answer-names-its-tenant`)
 * names the tenant the repo in the session's working directory belongs to and
 * whether coord's answers came from it. The badge reads the
 * session-info tenancy block and renders a divergence as a divergence, and an
 * unknown credential as unknown, rather than showing the stamped label alone —
 * served policy `ux-priorities` `a-status-signal-must-observe-the-state-it-names`.
 *
 * Renders NOTHING unless the device has more than one binding
 * (`useTenant().showSwitcher`). A single-tenant device has no ambiguity to
 * resolve, and the plan's D12 is explicit that the single-tenant case shows
 * no tenant UI — discoverability without clutter. The tenancy read is only
 * made on a multi-tenant device for the same reason.
 *
 * Styling mirrors the neighbouring non-durable pill in `ZoneLabel` /
 * `CompactZoneCard` (8px tokyo-night chip) so it reads as one badge row.
 */

import { Building2 } from "lucide-react";

import { useTenant } from "@/contexts/TenantContext";

import {
  coordRowSourceText,
  tenantBoundMembership,
  useSessionInfo,
  type SessionTenancy,
  type TenancyRepoExpected,
  type TenantBoundMembership,
  type TenantDisplay,
} from "./useSessionInfo";

/** The chip's discriminating stem: uuids share a long tail. */
function stem(tenantId: string): string {
  return tenantId.length > 8 ? tenantId.slice(0, 8) : tenantId;
}

function sameTenant(a: string | null | undefined, b: string | null | undefined): boolean {
  return !!a?.trim() && !!b?.trim() && a.trim().toLowerCase() === b.trim().toLowerCase();
}

/** `N tenant(s)`, so a single binding does not read as a plural. */
function tenantCount(n: number): string {
  return `${n} tenant${n === 1 ? "" : "s"}`;
}

/**
 * The bound-set half of `tenancy.display`, for the badge's title: the same
 * fact the session-info Tenant row states in its suffix (`boundSuffix`,
 * `useSessionInfo.ts`), switched on the same `tenantBoundMembership` so the
 * two surfaces cannot disagree. `named` is the tenant the badge names; membership is said
 * only when it is the display's own tenant, since `actingInBoundSet` is about
 * that one. Every arm is stated, including a measured "in", so that no badge
 * reads the set by silence. `null` only when the runner sent no display, in
 * which case the badge reads nothing of the set at all. Pure; exported for the
 * unit test.
 */
export function badgeBoundSetText(
  display: TenantDisplay | null | undefined,
  named: string | null | undefined,
): string | null {
  if (!display) return null;
  const membership = tenantBoundMembership(display);
  if (membership === "set-unread") {
    return `The tenants this runner is bound to could not be read (${display.bound.reason ?? "no reason given"}).`;
  }
  // An absent binding store, which is per runner instance: it holds nothing,
  // whatever membership claims.
  if (membership === "no-bound-tenants") return "This runner has no bound tenants.";
  const n = display.bound.tenantIds.length;
  // Membership is the display tenant's: any other named tenant gets the size.
  if (membership === null || !sameTenant(named, display.tenantId)) {
    return `This runner is bound to ${tenantCount(n)}.`;
  }
  const id = stem((named as string).trim());
  const who = `Tenant ${id}`;
  // "the one tenant" on a single binding, "the N tenants" otherwise.
  const set = n === 1 ? "the one tenant" : `the ${tenantCount(n)}`;
  if (membership === "out") {
    return n === 1
      ? `${who} is NOT ${set} this runner is bound to.`
      : `${who} is NOT among ${set} this runner is bound to.`;
  }
  if (membership === "unknown") {
    return n === 1
      ? `Whether tenant ${id} is ${set} this runner is bound to is unknown.`
      : `Whether tenant ${id} is among ${set} this runner is bound to is unknown.`;
  }
  return n === 1
    ? `${who} is ${set} this runner is bound to.`
    : `${who} is 1 of ${set} this runner is bound to.`;
}

/** The named tenant was MEASURED outside a non-empty bound set — the row's
 * `not among N bound`. An unknown set or membership is not this. */
function measuredOutsideBoundSet(
  display: TenantDisplay | null | undefined,
  named: string | null | undefined,
): boolean {
  return (
    !!display && tenantBoundMembership(display) === "out" && sameTenant(named, display.tenantId)
  );
}

/** ` <sentence>` for a title, or "" when the runner sent no display. */
function boundSetClause(
  display: TenantDisplay | null | undefined,
  named: string | null | undefined,
): string {
  const text = badgeBoundSetText(display, named);
  return text ? ` ${text}` : "";
}

/**
 * The fourth leg in words (plan
 * `2026-09-20-a-sessions-tenant-follows-its-repo-and-every-coord-answer-names-its-tenant`
 * Phase 2): the tenant the repo in the session's working directory belongs to,
 * and the latest comparison of a coord answer with it. Every non-resolved arm
 * says what it is — an unknown is never rendered as agreement. Pure; exported
 * for the unit test.
 */
export function repoExpectedText(repo: TenancyRepoExpected | null | undefined): string {
  if (!repo) return "unknown (this runner does not report the repo leg)";
  const named = repo.callerNamedTenantId
    ? `named explicitly: ${repo.callerNamedTenantId} (${repo.callerNamedSource ?? "no source"}); `
    : "";
  let where: string;
  switch (repo.status) {
    case "resolved":
      where =
        `${repo.tenantSlug ? `${repo.tenantSlug} ` : ""}${repo.tenantId ?? "?"} ` +
        `(repo ${repo.repo ?? "?"}, via ${repo.source ?? "?"} at ${repo.observedAt ?? "?"})`;
      break;
    case "no_repo":
      where = "no repo (the working directory is not a git checkout)";
      break;
    case "repo_unregistered":
      where = `none (repo ${repo.repo ?? "?"} is owned by no tenant this device is paired to)`;
      break;
    case "several":
      where = `ambiguous (repo ${repo.repo ?? "?"} is registered to ${repo.tenantIds.length} tenants: ${repo.tenantIds.join(", ")})`;
      break;
    case "pending":
      where = "still resolving";
      break;
    default:
      where = `unknown (${repo.reason ?? "no reason given"})`;
  }
  const verdict = repo.verdict
    ? ` — latest coord answer: ${repo.verdict}${repo.verdictDetail ? ` (${repo.verdictDetail})` : ""}`
    : " — no coord answer compared yet";
  return `${named}${where}${verdict}`;
}

/** Coord's own session row, as this runner registered it or as coord's
 * resume-probe answer named it. Pure. */
function coordRowText(tenancy: SessionTenancy): string {
  const row = tenancy.coordRow;
  if (!row) return "unknown (this runner does not report the coord-row leg)";
  if (row.status === "registered") {
    return `${row.tenantId ?? "?"} (${coordRowSourceText(row.source)})`;
  }
  if (row.status === "not_applicable") {
    return "terminal-hosted — its tenant is what the runner writes under (above)";
  }
  return `unknown (${row.reason ?? "no reason given"})`;
}

export interface TenantBadgeLabel {
  text: string;
  title: string;
  /** The known tenants disagree — rendered as a warning, never as a label. */
  diverged: boolean;
  /** The credential tenant could not be established. */
  credentialUnknown: boolean;
  /** The named tenant was measured OUTSIDE the runner's (non-empty) bound
   * set — the session-info row's `not among N bound`. Rendered in the same
   * attention tone as an unknown credential, never as a plain label. */
  outsideBoundSet: boolean;
}

/**
 * Pure label helper — exported so the unit test can lock the
 * visibility + formatting contract without rendering (the runner's vitest
 * config is `environment: "node"`, no jsdom; the same split `UnzonedChip`
 * uses).
 *
 * Returns `null` whenever the badge must not render:
 *  - the device has <= 1 binding (`showSwitcher` false) — no clutter, and
 *  - nothing names a tenant for this session: no spawn stamp, no
 *    divergence, and no RESOLVED acting tenant in the runner's
 *    `tenancy.display`. The device's *active* tenant is never shown in its
 *    place — the session may have been spawned under a different one.
 *
 * A session whose spawn chose no tenant (the ordinary case on a multi-bound
 * device, where the spawn picker defaults to "device default") used to be
 * hidden here even though its credential resolved a tenant — the same blind
 * spot that made the session-info Tenant row read `unknown` (plan
 * `2026-10-17-session-info-tenant-row-renders-unknown-for-a-resolved-tenant`).
 * It now shows that ACTING tenant, labelled as acting-via-credential rather
 * than as a spawn stamp.
 *
 * Otherwise returns the short chip text (uuid stem) plus a `title` carrying
 * the full id, since tenant ids are uuids and the header has ~8 chars of room.
 *
 * `tenancy` is the session-info tenancy block; `null`/absent (no Claude
 * session yet, still loading, or an older runner) keeps the stamped label
 * alone and claims nothing about the credential.
 */
export function tenantBadgeLabel(
  tenantId: string | undefined | null,
  showSwitcher: boolean,
  tenancy?: SessionTenancy | null,
): TenantBadgeLabel | null {
  if (!showSwitcher) return null;
  const stamped = tenantId?.trim() || tenancy?.row.tenantId?.trim() || null;
  const diverged = tenancy?.diverged === true;
  const display = tenancy?.display ?? null;
  const resolvedActing = display?.status === "resolved" ? display.tenantId : null;
  if (!stamped && !diverged && !resolvedActing) return null;

  const fixed =
    `A session's tenant is fixed at spawn — switching the active tenant ` +
    `only affects future sessions.`;
  const credentialUnknown = tenancy?.credential.status === "unknown";

  if (diverged && tenancy) {
    const credential = tenancy.credential.tenantId;
    // Every leg the runner listed as conflicting, when it listed them — the
    // stamp-vs-credential pair alone misses a coord row stamped elsewhere.
    const conflictStems = [...new Set((display?.conflicts ?? []).map((c) => stem(c.tenantId)))];
    const text =
      conflictStems.length > 1
        ? conflictStems.join("≠")
        : `${stamped ? stem(stamped) : "?"}≠${credential ? stem(credential) : "?"}`;
    const lines = [
      `TENANT MISMATCH — this session does not act as one tenant.`,
      `Spawned for: ${
        stamped ??
        (tenancy.row.spawnDeviceDefaultStatus === "recorded"
          ? `no choice (device default at spawn: ${tenancy.row.spawnDeviceDefaultTenantId ?? "none"})`
          : `no choice (device default at spawn not recorded: ${
              tenancy.row.spawnDeviceDefaultReason ?? "no reason given"
            })`)
      }`,
      `Runner writes: ${tenancy.dataPlane.tenantId ?? tenancy.dataPlane.status}`,
      `coord-mcp writes (memory, prompt documents, gates): ${
        credential ?? `unknown (${tenancy.credential.reason ?? "no reason given"})`
      }`,
      `Repo in the working directory belongs to: ${repoExpectedText(tenancy.repoExpected)}`,
      `Coord session row: ${coordRowText(tenancy)}`,
    ];
    // Membership is the display's acting tenant's, so the line says which leg.
    const boundText = badgeBoundSetText(display, display?.tenantId);
    if (boundText)
      lines.push(`Bound set${display?.tenantId ? " (acting tenant)" : ""}: ${boundText}`);
    return {
      text,
      title: lines.join("\n"),
      diverged: true,
      credentialUnknown,
      outsideBoundSet: measuredOutsideBoundSet(display, display?.tenantId),
    };
  }

  // No stamp, but the runner resolved the tenant the session ACTS as.
  if (!stamped && resolvedActing && display) {
    const how =
      display.source === "credential"
        ? "via its coord credential"
        : display.source === "credential_default_slot"
          ? "via its default-slot credential (the device default binding)"
          : display.source === "data_plane_device"
            ? "via the runner's data plane (the device default binding)"
            : "via the runner's data plane";
    const unconfirmed =
      tenancy?.divergence === "unknown" ? ` Whether every leg agrees could not be compared.` : "";
    // A data-plane answer beside an unreadable credential is not a confident
    // one: the coord-mcp writes' tenant is unknown, so the chip says so.
    const unknownCredential =
      credentialUnknown && tenancy
        ? ` The tenant its coord-mcp writes go to is UNKNOWN (${
            tenancy.credential.reason ?? display.reason ?? "no reason given"
          }).`
        : "";
    return {
      text: credentialUnknown ? `${stem(resolvedActing)}?` : stem(resolvedActing),
      title: `This session acts as tenant ${resolvedActing}${
        display.tenantSlug ? ` (${display.tenantSlug})` : ""
      } ${how}. Its spawn chose no tenant (device default).${boundSetClause(
        display,
        resolvedActing,
      )}${unknownCredential}${unconfirmed} ${fixed}`,
      diverged: false,
      credentialUnknown,
      outsideBoundSet: measuredOutsideBoundSet(display, resolvedActing),
    };
  }

  // `stamped` is non-null here: every null case returned above.
  const acting = stamped as string;
  const outsideBoundSet = measuredOutsideBoundSet(display, acting);
  if (credentialUnknown && tenancy) {
    return {
      text: `${stem(acting)}?`,
      title:
        `This session was spawned for tenant ${acting}, but the tenant its ` +
        `coord-mcp writes go to is UNKNOWN (${tenancy.credential.reason ?? "no reason given"}).` +
        `${boundSetClause(display, acting)} ${fixed}`,
      diverged: false,
      credentialUnknown: true,
      outsideBoundSet,
    };
  }
  // `diverged: false` alone is not agreement: a comparison with an unknown
  // half says so rather than lending the plain label a confirmation.
  const unconfirmed =
    tenancy && tenancy.divergence === "unknown"
      ? ` Whether its coord-mcp writes go to the same tenant could not be compared.`
      : "";
  const repoLine = tenancy
    ? ` Repo in the working directory belongs to: ${repoExpectedText(tenancy.repoExpected)}.`
    : "";
  return {
    text: stem(acting),
    title: `This session is acting as tenant ${acting}.${unconfirmed}${repoLine}${boundSetClause(
      display,
      acting,
    )} ${fixed}`,
    diverged: false,
    credentialUnknown: false,
    outsideBoundSet,
  };
}

/**
 * The badge's UI Bridge id for one zone (plan
 * `2026-11-08-tenant-badge-parity-and-oracle-fixtures` D1). Per-zone, like
 * `terminal-zone-header-<n>` and `terminal-session-info-<field>-<n>`: the
 * runner's DOM scanner registers one element per id, so a shared id made every
 * zone's badge one ambiguous control. Pure; the single source for both
 * `data-ui-bridge-id` and `data-testid`.
 */
export function tenantBadgeElementId(zoneIndex: number): string {
  return `terminal-tenant-badge-${zoneIndex}`;
}

/**
 * Whether the badge can render for a tab that has NO Claude session (plan
 * `2026-11-08-tenant-badge-parity-and-oracle-fixtures` D2): with no session
 * there is no tenancy block to read, so only a spawn stamp names a tenant, and
 * only a multi-bound device (`showSwitcher`) shows tenant UI at all. The
 * single-zone strip uses it so a stamped plain shell gets the badge on a
 * one-terminal page exactly as `ZoneLabel` shows it in a multi-zone header.
 * Pure; mirrors the stamp arm of `tenantBadgeLabel`.
 */
export function tenantBadgeRendersWithoutSession(
  showSwitcher: boolean,
  tenantId: string | undefined | null,
): boolean {
  return showSwitcher && !!tenantId?.trim();
}

/**
 * The runner's derived answer, as attributes a UI Bridge read can compare.
 * `data-tenant-id` is the tab's copy of the spawn stamp, so it is absent on a
 * no-stamp chip that names the acting tenant, and `data-tenant-credential` is
 * one leg rather than the answer.
 *
 * `data-tenant-acting` is withheld whenever the display's tenant came from the
 * spawn stamp (`source: row_stamp`, under `stamp_only` and under a `mismatch`
 * alike): that is a record of what the spawn chose, never an observation of
 * what the session acts as. An absent attribute is therefore "nothing to state
 * here", not "no display": `data-tenant-display-status` alone says which.
 *
 * `data-tenant-bound-membership` is the display's own tenant
 * (`tenancy.display.tenantId`) against the runner's bound set — the value of
 * `tenantBoundMembership`, the classification the session-info row's suffix
 * (`boundSuffix`) switches on: `in`, `out` (the row's `not among
 * N bound`), `unknown`, `no-bound-tenants` (a measured empty set, whatever
 * membership claims) or `set-unread`. Its subject is the ROW's, which is not
 * always the tenant the chip names: a tab stamped for another tenant can read
 * `out` here while its chip keeps the plain tone, because the attention tone
 * is set only when the named tenant is the display's own (`outsideBoundSet`).
 * It is absent only when there is no display, or a NON-EMPTY measured set with
 * no display tenant to place in it.
 * Pure, so its arms are tested without rendering; `TenantBadge.render.test.tsx`
 * pins the spread onto the real element.
 */
export function tenantBadgeDisplayAttrs(tenancy: SessionTenancy | null): {
  "data-tenant-acting": string | undefined;
  "data-tenant-display-status": string | undefined;
  "data-tenant-display-source": string | undefined;
  "data-tenant-bound-membership": TenantBoundMembership | undefined;
} {
  const display = tenancy?.display ?? null;
  return {
    "data-tenant-acting":
      display?.source === "row_stamp" ? undefined : (display?.tenantId ?? undefined),
    "data-tenant-display-status": display?.status ?? undefined,
    "data-tenant-display-source": display?.source ?? undefined,
    "data-tenant-bound-membership": display
      ? (tenantBoundMembership(display) ?? undefined)
      : undefined,
  };
}

interface TenantBadgeProps {
  /** The tenant this session was spawned under (`TerminalTab.tenantId`). */
  tenantId?: string;
  /** The session to read the tenancy block for (`TerminalTab.claudeSessionId`). */
  claudeSessionId?: string;
  /**
   * The zone this badge sits in — the same index the mount site passes its
   * `SessionInfoDropdown`. Required so no mount site can omit it and silently
   * re-create a shared id.
   */
  zoneIndex: number;
  /** Extra classes for per-surface sizing (the compact card uses pills). */
  className?: string;
}

export function TenantBadge({ tenantId, claudeSessionId, zoneIndex, className }: TenantBadgeProps) {
  const { showSwitcher } = useTenant();
  // Read only where the badge can render at all (D12).
  const info = useSessionInfo(showSwitcher ? claudeSessionId : undefined);
  const label = tenantBadgeLabel(tenantId, showSwitcher, info.body?.tenancy ?? null);
  if (!label) return null;

  const tone = label.diverged
    ? "text-[#f7768e] bg-[#f7768e]/15"
    : label.credentialUnknown || label.outsideBoundSet
      ? "text-[#e0af68] bg-[#e0af68]/15"
      : "text-[#7aa2f7] bg-[#7aa2f7]/15";

  return (
    <span
      // UI-Bridge-discoverable identity, one per zone. `data-ui-bridge-id` is
      // the runner's DOM-scan discovery attribute, so a snapshot sees this
      // non-interactive <span> when it renders. The id carries the zone index
      // (`terminal-tenant-badge-<n>`, the per-zone family of
      // `terminal-session-info-<field>-<n>`): one id shared by every zone
      // registered a single ambiguous element. `data-testid` is the same value,
      // as on the session-info dropdown rows.
      data-ui-bridge-id={tenantBadgeElementId(zoneIndex)}
      data-testid={tenantBadgeElementId(zoneIndex)}
      data-tenant-id={tenantId}
      data-tenant-diverged={label.diverged ? "true" : "false"}
      data-tenant-credential={info.body?.tenancy?.credential.tenantId ?? undefined}
      data-tenant-repo-expected={info.body?.tenancy?.repoExpected?.status ?? undefined}
      data-tenant-repo-verdict={info.body?.tenancy?.repoExpected?.verdict ?? undefined}
      {...tenantBadgeDisplayAttrs(info.body?.tenancy ?? null)}
      className={`flex items-center gap-0.5 min-w-[18px] overflow-hidden text-[8px] px-1 py-0 rounded font-mono ${tone} ${
        className ?? ""
      }`}
      title={label.title}
      aria-label={label.title}
    >
      {/* Truncates in a narrow zone header, keeping its icon; `title` and
          `aria-label` carry the full text. Budgeted in
          `HEADER_TRUNCATING_FLOORS_PX`. */}
      <Building2 className="w-2.5 h-2.5 shrink-0" aria-hidden />
      <span className="truncate">{label.text}</span>
    </span>
  );
}
