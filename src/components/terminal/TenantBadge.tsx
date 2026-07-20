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
 * Renders NOTHING unless the device has more than one binding
 * (`useTenant().showSwitcher`). A single-tenant device has no ambiguity to
 * resolve, and the plan's D12 is explicit that the single-tenant case shows
 * no tenant UI — discoverability without clutter.
 *
 * Styling mirrors the neighbouring non-durable pill in `ZoneLabel` /
 * `CompactZoneCard` (8px tokyo-night chip) so it reads as one badge row.
 */

import { Building2 } from "lucide-react";

import { useTenant } from "@/contexts/TenantContext";

/**
 * Pure label helper — exported so the unit test can lock the
 * visibility + formatting contract without rendering (the runner's vitest
 * config is `environment: "node"`, no jsdom; the same split `UnzonedChip`
 * uses).
 *
 * Returns `null` whenever the badge must not render:
 *  - the device has <= 1 binding (`showSwitcher` false) — no clutter, and
 *  - the session has no recorded tenant (a tab restored from a pre-F2
 *    durable record). Showing the *device's* active tenant there would be a
 *    lie, because the restored session may well have been spawned under a
 *    different one.
 *
 * Otherwise returns the short chip text (uuid stem) plus a `title` carrying
 * the full id, since tenant ids are uuids and the header has ~8 chars of room.
 */
export function tenantBadgeLabel(
  tenantId: string | undefined | null,
  showSwitcher: boolean,
): { text: string; title: string } | null {
  if (!showSwitcher) return null;
  const trimmed = tenantId?.trim();
  if (!trimmed) return null;
  // Uuids share a long tail; the first segment is the discriminating part.
  const text = trimmed.length > 8 ? trimmed.slice(0, 8) : trimmed;
  return {
    text,
    title:
      `This session is acting as tenant ${trimmed}. ` +
      `A session's tenant is fixed at spawn — switching the active tenant ` +
      `only affects future sessions.`,
  };
}

interface TenantBadgeProps {
  /** The tenant this session was spawned under (`TerminalTab.tenantId`). */
  tenantId?: string;
  /** Extra classes for per-surface sizing (the compact card uses pills). */
  className?: string;
}

export function TenantBadge({ tenantId, className }: TenantBadgeProps) {
  const { showSwitcher } = useTenant();
  const label = tenantBadgeLabel(tenantId, showSwitcher);
  if (!label) return null;

  return (
    <span
      data-tenant-id={tenantId}
      className={`flex items-center gap-0.5 shrink-0 text-[8px] text-[#7aa2f7] bg-[#7aa2f7]/15 px-1 py-0 rounded font-mono ${
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
