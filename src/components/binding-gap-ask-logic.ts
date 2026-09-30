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

export function dismissKey(ask: BindingGapAsk): string {
  return `${ask.tenantId}@${ask.firstSeen ?? "unknown"}`;
}

// ---------------------------------------------------------------------------
// The per-tenant view — plan
// `2026-09-30-runner-says-connected-while-bound-tenants-have-no-credential-and-offers-only-a-terminal-command`,
// D2/D3. ONE source for the banner's gap list and the Settings card's
// "Workspaces" rows, so the two cannot disagree. The ask records above
// contribute only the per-account dismissal key and the terminal fallback.
// ---------------------------------------------------------------------------

/** Tauri command serving the published binding-gap cell as per-tenant rows. */
export const GET_BINDING_GAPS_CMD = "get_binding_gaps";

/** Tauri command running the attended "Connect all my workspaces" flow. */
export const PAIR_ALL_TENANTS_CMD = "pair_all_tenants";

export type TenantCredentialState = "connected" | "no_credential" | "unknown";

export interface BindingGapRow {
  tenantId: string;
  displayName: string | null;
  state: TenantCredentialState;
}

export interface BindingGapView {
  /** `unknown` means NOTHING was established — every row is then unknown. */
  status: "measured" | "unknown";
  reason: string | null;
  rows: BindingGapRow[];
}

const STATES: readonly TenantCredentialState[] = ["connected", "no_credential", "unknown"];

/**
 * Coerce `get_binding_gaps`' answer. Returns `null` when the command is absent
 * or answered nothing usable — UNKNOWN, never "no gaps".
 *
 * Fails toward `unknown`: an unrecognised status makes the whole view unknown,
 * an unrecognised row state makes that row unknown, and an unknown view forces
 * EVERY row unknown even if a row claims `connected` — a status signal must not
 * render an unmeasured tenant as healthy.
 */
export function normalizeBindingGapView(raw: unknown): BindingGapView | null {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) return null;
  const o = raw as Record<string, unknown>;
  if (!Array.isArray(o.rows)) return null;
  const status = o.status === "measured" ? "measured" : "unknown";
  const rows: BindingGapRow[] = [];
  for (const item of o.rows) {
    if (item === null || typeof item !== "object") continue;
    const r = item as Record<string, unknown>;
    const tenantId = str(r.tenant_id);
    if (tenantId === null) continue;
    const claimed = STATES.find((s) => s === r.state) ?? "unknown";
    rows.push({
      tenantId,
      displayName: str(r.display_name),
      state: status === "measured" ? claimed : "unknown",
    });
  }
  return {
    status,
    reason: str(o.reason) ?? (status === "unknown" ? "the runner did not say why" : null),
    rows,
  };
}

/** Operator-facing words for a row's state. */
export function credentialStateLabel(state: TenantCredentialState): string {
  switch (state) {
    case "connected":
      return "connected";
    case "no_credential":
      return "no credential";
    case "unknown":
      return "unknown";
  }
}

/**
 * Rows the Settings card's "Connect all my workspaces" would pair: every row
 * that is not `connected` — a gap, or a tenant whose state is unknown.
 */
export function rowsNeedingConnect(view: BindingGapView | null): BindingGapRow[] {
  return view === null ? [] : view.rows.filter((r) => r.state !== "connected");
}

/** True only on a MEASURED view with no gap — the banner's auto-dismiss test. */
export function gapsCleared(view: BindingGapView | null): boolean {
  return (
    view !== null &&
    view.status === "measured" &&
    !view.rows.some((r) => r.state === "no_credential")
  );
}

/** One tenant the combined banner offers to connect. */
export interface BindingGapBannerEntry {
  tenantId: string;
  displayName: string | null;
  /** Per-account, per-lapse dismissal key (from the ask record when present). */
  key: string;
  /** The terminal fallback. */
  command: string;
  /** What the terminal path also does (home pointer). */
  caveat: string | null;
}

/**
 * The banner's entries: the view's `no_credential` rows — never re-derived —
 * minus this session's dismissals.
 *
 * `asks` is the ask-record read. Its role here is narrow: `null` (no signed-in
 * account to ask on behalf of, or UNKNOWN) shows no banner, exactly as the
 * ask path always has, and a recorded ask supplies the lapse for the dismissal
 * key plus the terminal command. An UNKNOWN view shows no banner either — the
 * Settings card is where unknown is said.
 */
export function bannerGapEntries(
  view: BindingGapView | null,
  asks: BindingGapAsk[] | null,
  dismissed: ReadonlySet<string>,
): BindingGapBannerEntry[] {
  if (view === null || view.status !== "measured" || asks === null) return [];
  return view.rows
    .filter((r) => r.state === "no_credential")
    .map((r) => {
      const ask = asks.find((a) => a.tenantId === r.tenantId);
      return {
        tenantId: r.tenantId,
        displayName: r.displayName,
        key: `${r.tenantId}@${ask?.firstSeen ?? "unrecorded"}`,
        command: ask?.command ?? `qontinui_profile device pair --tenant-id ${r.tenantId}`,
        caveat: ask?.caveat ?? null,
      };
    })
    .filter((e) => !dismissed.has(e.key));
}

export type TenantPairStatus = "connected" | "skipped" | "failed";

export interface TenantPairResult {
  tenantId: string;
  status: TenantPairStatus;
  skippedReason: string | null;
}

/** Coerce `pair_all_tenants`' `{results}`; `null` when it is not that shape. */
export function normalizePairAllResults(raw: unknown): TenantPairResult[] | null {
  if (raw === null || typeof raw !== "object") return null;
  const results = (raw as Record<string, unknown>).results;
  if (!Array.isArray(results)) return null;
  const out: TenantPairResult[] = [];
  for (const item of results) {
    if (item === null || typeof item !== "object") continue;
    const r = item as Record<string, unknown>;
    const tenantId = str(r.tenant_id);
    if (tenantId === null) continue;
    const status: TenantPairStatus =
      r.status === "connected" || r.status === "skipped" ? r.status : "failed";
    out.push({ tenantId, status, skippedReason: str(r.skipped_reason) });
  }
  return out;
}

/** Operator-facing words for one pair result. */
export function pairResultLabel(r: TenantPairResult): string {
  switch (r.status) {
    case "connected":
      return "connected";
    case "skipped":
      return r.skippedReason === "not_a_member"
        ? "skipped — you are not a member of this workspace"
        : `skipped${r.skippedReason ? ` — ${r.skippedReason}` : ""}`;
    case "failed":
      return `failed${r.skippedReason ? ` — ${r.skippedReason}` : ""}`;
  }
}
