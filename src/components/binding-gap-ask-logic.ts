/**
 * Pure logic for {@link BindingGapAskBanner}.
 *
 * The device-JWT refresher records ONE ask per bound tenant this device holds
 * no credential for (plan
 * `2026-09-20-per-tenant-coord-credentials-and-a-workspace-tenant-pin`,
 * Phase 4). The record on disk IS the ask: the UI reads it with
 * {@link GET_BINDING_GAP_ASKS_CMD} on mount and whenever the refresher emits
 * {@link BINDING_GAP_NUDGE_EVENT}. The event is only a nudge — Tauri `emit` has
 * no replay, so state built from the event alone would miss every ask recorded
 * before the listener registered.
 */

/** Tauri command returning the open asks, or `null` when UNKNOWN. */
export const GET_BINDING_GAP_ASKS_CMD = "get_binding_gap_asks";

/** Event the refresher emits when the set of open asks changes. */
export const BINDING_GAP_NUDGE_EVENT = "autonomy-binding-gap";

export interface BindingGapAsk {
  tenantId: string;
  message: string;
  reason: string;
  command: string;
  caveat: string | null;
  firstSeen: number | null;
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.trim().length > 0 ? v : null;
}

/**
 * Coerce the command's answer into asks. Returns `null` for UNKNOWN (a runner
 * build without the command, a `null` answer, or a non-array) — never an
 * empty list, which would read as "no asks". Entries missing the fields the
 * operator needs (tenant, command) are dropped rather than rendered blank.
 */
export function normalizeBindingGapAsks(raw: unknown): BindingGapAsk[] | null {
  if (!Array.isArray(raw)) return null;
  const asks: BindingGapAsk[] = [];
  for (const item of raw) {
    if (item === null || typeof item !== "object") continue;
    const o = item as Record<string, unknown>;
    const tenantId = str(o.tenant_id);
    const command = str(o.command);
    if (tenantId === null || command === null) continue;
    asks.push({
      tenantId,
      command,
      message: str(o.message) ?? `This device holds no credential for tenant ${tenantId}.`,
      reason: str(o.reason) ?? "",
      caveat: str(o.caveat),
      firstSeen: typeof o.first_seen === "number" ? o.first_seen : null,
    });
  }
  return asks;
}

/**
 * The asks still to show, after the operator's per-session dismissals. A
 * dismissal is keyed on tenant AND lapse (`firstSeen`), so a NEW lapse for the
 * same tenant is shown again.
 */
export function visibleBindingGapAsks(
  asks: BindingGapAsk[] | null,
  dismissed: ReadonlySet<string>,
): BindingGapAsk[] {
  if (asks === null) return [];
  return asks.filter((a) => !dismissed.has(dismissKey(a)));
}

export function dismissKey(ask: BindingGapAsk): string {
  return `${ask.tenantId}@${ask.firstSeen ?? "unknown"}`;
}
