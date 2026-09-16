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
 * tenant B has written to tenant A through the third. The badge reads the
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

import { useSessionInfo, type SessionTenancy } from "./useSessionInfo";

/** The chip's discriminating stem: uuids share a long tail. */
function stem(tenantId: string): string {
  return tenantId.length > 8 ? tenantId.slice(0, 8) : tenantId;
}

export interface TenantBadgeLabel {
  text: string;
  title: string;
  /** The known tenants disagree — rendered as a warning, never as a label. */
  diverged: boolean;
  /** The credential tenant could not be established. */
  credentialUnknown: boolean;
}

/**
 * Pure label helper — exported so the unit test can lock the
 * visibility + formatting contract without rendering (the runner's vitest
 * config is `environment: "node"`, no jsdom; the same split `UnzonedChip`
 * uses).
 *
 * Returns `null` whenever the badge must not render:
 *  - the device has <= 1 binding (`showSwitcher` false) — no clutter, and
 *  - the session has no recorded tenant AND nothing disagrees (a tab restored
 *    from a pre-F2 durable record). Showing the *device's* active tenant there
 *    would be a lie, because the restored session may well have been spawned
 *    under a different one.
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
  if (!stamped && !diverged) return null;

  const fixed =
    `A session's tenant is fixed at spawn — switching the active tenant ` +
    `only affects future sessions.`;
  const credentialUnknown = tenancy?.credential.status === "unknown";

  if (diverged && tenancy) {
    const credential = tenancy.credential.tenantId;
    const text = `${stamped ? stem(stamped) : "?"}≠${credential ? stem(credential) : "?"}`;
    const lines = [
      `TENANT MISMATCH — this session does not act as one tenant.`,
      `Spawned for: ${
        stamped ??
        (tenancy.row.spawnDeviceDefaultTenantId
          ? `no choice (device default at spawn: ${tenancy.row.spawnDeviceDefaultTenantId})`
          : "not recorded")
      }`,
      `Runner writes: ${tenancy.dataPlane.tenantId ?? tenancy.dataPlane.status}`,
      `coord-mcp writes (memory, prompt documents, gates): ${
        credential ?? `unknown (${tenancy.credential.reason ?? "no reason given"})`
      }`,
    ];
    return { text, title: lines.join("\n"), diverged: true, credentialUnknown };
  }

  // `stamped` is non-null here: the null case returned above unless diverged.
  const acting = stamped as string;
  if (credentialUnknown && tenancy) {
    return {
      text: `${stem(acting)}?`,
      title:
        `This session was spawned for tenant ${acting}, but the tenant its ` +
        `coord-mcp writes go to is UNKNOWN (${tenancy.credential.reason ?? "no reason given"}). ` +
        fixed,
      diverged: false,
      credentialUnknown: true,
    };
  }
  return {
    text: stem(acting),
    title: `This session is acting as tenant ${acting}. ${fixed}`,
    diverged: false,
    credentialUnknown: false,
  };
}

interface TenantBadgeProps {
  /** The tenant this session was spawned under (`TerminalTab.tenantId`). */
  tenantId?: string;
  /** The session to read the tenancy block for (`TerminalTab.claudeSessionId`). */
  claudeSessionId?: string;
  /** Extra classes for per-surface sizing (the compact card uses pills). */
  className?: string;
}

export function TenantBadge({ tenantId, claudeSessionId, className }: TenantBadgeProps) {
  const { showSwitcher } = useTenant();
  // Read only where the badge can render at all (D12).
  const info = useSessionInfo(showSwitcher ? claudeSessionId : undefined);
  const label = tenantBadgeLabel(tenantId, showSwitcher, info.body?.tenancy ?? null);
  if (!label) return null;

  const tone = label.diverged
    ? "text-[#f7768e] bg-[#f7768e]/15"
    : label.credentialUnknown
      ? "text-[#e0af68] bg-[#e0af68]/15"
      : "text-[#7aa2f7] bg-[#7aa2f7]/15";

  return (
    <span
      // UI-Bridge-discoverable identity. `data-ui-bridge-id` is the runner's
      // DOM-scan discovery attribute (same mechanism the sibling
      // `SpawnTenantPicker` <select> uses, `terminal.spawn-tenant-picker`), so
      // a snapshot / find-by-text sees this badge when it renders — it is a
      // non-interactive <span> and would otherwise be invisible to the bridge.
      // `data-testid` mirrors the neighbouring display badges (`ai-status-badge`).
      data-ui-bridge-id="terminal.tenant-badge"
      data-testid="tenant-badge"
      data-tenant-id={tenantId}
      data-tenant-diverged={label.diverged ? "true" : "false"}
      data-tenant-credential={info.body?.tenancy?.credential.tenantId ?? undefined}
      className={`flex items-center gap-0.5 shrink-0 text-[8px] px-1 py-0 rounded font-mono ${tone} ${
        className ?? ""
      }`}
      title={label.title}
      aria-label={label.title}
    >
      <Building2 className="w-2.5 h-2.5" aria-hidden />
      {label.text}
    </span>
  );
}
