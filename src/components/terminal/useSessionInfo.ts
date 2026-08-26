import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * ONE read for the zone-header session-info dropdown (plan
 * `2026-08-16-runner-session-info-dropdown-and-restore-verification`, D1/D4).
 *
 * Backed by the `session_info_get` Tauri command, which returns the WHOLE
 * projection in one shot: durable lifecycle record ⨝ live-registry overlay
 * (account + the name the operator actually sees) ⨝ the runner-local PR
 * ledger. The dropdown must NEVER assemble this from several round-trips — a
 * partially-loaded panel whose blank rows are indistinguishable from
 * genuinely-absent data is exactly the ambiguity G5 describes.
 *
 * Every unknown is spelled, never rendered as an empty:
 *  - `available: false` always carries a `reason` → the trigger renders a
 *    muted "unknown" state instead of self-hiding (the G5 defect this module
 *    replaces — the deleted PR-only hook + dropdown returned `null` there).
 *  - `prs.status: "unavailable"` with a reason is a DIFFERENT rendering from
 *    `openCount: 0`.
 *  - a PR whose land verdict could not be evaluated lands in its own
 *    `unknown` bucket with the recorded reason — never folded into a
 *    confident "not landed" (R1).
 *
 * Served policy `verification-and-evidence` `silent-empty-is-unknown`, applied
 * to a UI surface.
 */

/** The four identifiers a session carries (D2) — all four are shown rather
 * than guessing which two the operator meant. */
export interface SessionIdentity {
  claudeSessionId: string;
  terminalId: string;
  fleetSessionHandle: string | null;
  tenantId: string | null;
  taskRunId: string | null;
}

/** The name the operator sees, plus where it came from. `value: null` means
 * no name was ever observed — NOT "the session is unnamed" (R2). */
export interface SessionName {
  value: string | null;
  /** `"operator"` | `"derived"` | `"unknown"`. */
  source: string;
}

export interface SessionAccount {
  label: string | null;
  /** CLI wrapper (`clp`, `clg`, …) — the half of the resume command that
   * names the account. */
  wrapper: string | null;
  configDir: string | null;
}

export interface SessionPlacement {
  pageId: string;
  zoneIndex: number;
  workingDir: string | null;
}

export interface SessionLifecycleInfo {
  /** How much evidence backs this identity: `confirmed` (a provider hook
   * fired) | `transcript` (a transcript exists on disk) | `provisional`
   * (NEITHER — the spawn-time seam's prediction). Server-derived; see the Rust
   * `classify_identity_evidence`. */
  identityEvidence?: IdentityEvidence;
  state: string;
  provider: string;
  origin: string | null;
  /** Epoch MILLISECONDS (`Utc::now().timestamp_millis()` on the Rust side). */
  openedAt: number;
  lastSeenAt: number;
  closedAt: number | null;
  closeReason: string | null;
  confirmed: boolean;
  transcriptExists: boolean;
  restorable: boolean;
  restoredFromBootAt: number | null;
  /** `"resumed"` | `"terminal-only"` | `"failed"`; `null` = never restored,
   * which is NOT the same claim as `"failed"`. */
  restoreTier: string | null;
  /** `null` ⇒ never reported (unknown), not `false`. */
  bypassPermissions: boolean | null;
}

/** One PR this session authored (attributed by the `Session-Id:` trailer). */
export interface SessionPrOpened {
  repo: string;
  prNumber: number;
  branch: string | null;
  openedAt: string | null;
  /** `"open"` | `"closed"` | `"merged"`. */
  prState: string | null;
}

/** One PR some signal PROVED landed. */
export interface SessionPrLanded {
  repo: string;
  prNumber: number;
  landedAt: string | null;
  /** `"github-merge"` | `"ff-land"` | `"coord-label"`. */
  landSignal: string | null;
}

/** One PR whose land verdict could NOT be evaluated — neither landed nor
 * not-landed. Its own bucket, always with the reason. */
export interface SessionPrUnknown {
  repo: string;
  prNumber: number;
  /** `"rebase_land_or_abandoned"` | `"ref_stale"` | `"head_object_missing"` |
   * `"no_base_ref"` | `"not_a_repo"` | `"unspecified"`. */
  reason: string;
}

/**
 * The session's PR ledger, split opened / landed / unevaluable.
 *
 * `opened` and `landed` are deliberately NOT disjoint: a PR opened and landed
 * by the same session appears in both. The counts are therefore labelled
 * `3 opened · 2 landed`, never presented as a partition of one total (D3).
 */
export interface SessionPrs {
  status: "ok" | "unavailable";
  /** Required whenever `status !== "ok"`. */
  reason: string | null;
  opened: SessionPrOpened[];
  landed: SessionPrLanded[];
  unknown: SessionPrUnknown[];
  openCount: number;
  landedCount: number;
  unknownCount: number;
}

export interface SessionInfoBody {
  identity: SessionIdentity;
  name: SessionName;
  account: SessionAccount;
  placement: SessionPlacement;
  lifecycle: SessionLifecycleInfo;
  prs: SessionPrs;
}

/** `"loading"` is a distinct third state: the first read has not returned, so
 * we know neither that the data exists nor that it does not. */
export type SessionInfoStatus = "loading" | "unavailable" | "ok";

export interface SessionInfoState {
  status: SessionInfoStatus;
  /** Present whenever `status === "unavailable"`. */
  reason: string | null;
  body: SessionInfoBody | null;
}

export const LOADING_STATE: SessionInfoState = { status: "loading", reason: null, body: null };

/** Rendered wherever a value was never observed. Never a blank cell: a blank
 * reads as "empty", and empty is a claim the runner did not establish (R2). */
export const UNKNOWN_TEXT = "unknown";

/** The addressable field set (D5). Zone-indexed ids, matching the existing
 * `terminal-zone-header-<n>` convention. */
export type SessionInfoField =
  | "account"
  | "name"
  | "terminal-id"
  | "claude-session-id"
  | "fleet-handle"
  | "tenant"
  | "task-run"
  | "working-dir"
  | "provider"
  | "origin"
  | "opened-at"
  | "restore-tier"
  | "prs-opened"
  | "prs-landed";

/** `terminal-session-info-<field>-<zoneIndex>`; `field` is also `"trigger"` /
 * `"panel"` for the two container elements. Pure — unit-tested. */
export function sessionInfoElementId(
  field: SessionInfoField | "trigger" | "panel",
  zoneIndex: number,
): string {
  return `terminal-session-info-${field}-${zoneIndex}`;
}

/** `owner/name#123` identity key shared by the three PR buckets. Pure. */
export function prKey(pr: { repo: string; prNumber: number }): string {
  return `${pr.repo}#${pr.prNumber}`;
}

/** GitHub URL when the repo is the webhook's `owner/name`; the ledger can also
 * surface a bare checkout name (repo-format skew) — no owner, no link. Pure. */
export function prGithubUrl(pr: { repo: string; prNumber: number }): string | null {
  return pr.repo.includes("/") ? `https://github.com/${pr.repo}/pull/${pr.prNumber}` : null;
}

/** Compact display label: bare repo name + PR number (`runner#663`). Pure. */
export function prLabel(pr: { repo: string; prNumber: number }): string {
  const bare = pr.repo.split("/").pop() ?? pr.repo;
  return `${bare}#${pr.prNumber}`;
}

/**
 * Unlanded first (the actionable rows), newest PR first within each group.
 *
 * Successor to `sortPrsUnmergedFirst`: the ordering key is now "did some
 * signal prove this landed", not GitHub's `merged` boolean — coord
 * fast-forward lands are the majority of this fleet's landings and leave
 * `merged=false` (G3). Pure — unit-tested.
 */
export function sortPrsUnlandedFirst(
  opened: readonly SessionPrOpened[],
  landedKeys: ReadonlySet<string>,
): SessionPrOpened[] {
  return [...opened].sort((a, b) => {
    const aLanded = landedKeys.has(prKey(a));
    const bLanded = landedKeys.has(prKey(b));
    if (aLanded !== bLanded) return aLanded ? 1 : -1;
    if (a.prNumber !== b.prNumber) return b.prNumber - a.prNumber;
    return a.repo.localeCompare(b.repo);
  });
}

export interface PrsSummary {
  openCount: number;
  landedCount: number;
  unknownCount: number;
  /** Every opened PR landed, with nothing unevaluable. Vacuously false on an
   * empty ledger — "no PRs" is not "all landed". */
  allLanded: boolean;
  /** The label. Never a partition: `3 opened · 2 landed` (`· 1 unknown`). */
  text: string;
}

/** Roll-up behind the trigger glyph and the panel header. Pure. */
export function summarizePrs(prs: SessionPrs): PrsSummary {
  if (prs.status !== "ok") {
    return {
      openCount: 0,
      landedCount: 0,
      unknownCount: 0,
      allLanded: false,
      text: `PRs unavailable — ${prs.reason ?? UNKNOWN_TEXT}`,
    };
  }
  const parts = [`${prs.openCount} opened`, `${prs.landedCount} landed`];
  if (prs.unknownCount > 0) parts.push(`${prs.unknownCount} unknown`);
  return {
    openCount: prs.openCount,
    landedCount: prs.landedCount,
    unknownCount: prs.unknownCount,
    allLanded: prs.openCount > 0 && prs.landedCount >= prs.openCount && prs.unknownCount === 0,
    text: parts.join(" · "),
  };
}

/** Tokyo-Night verdict palette, shared with `constants.ts`' state dots. */
export const TONE_COLORS = {
  landed: "#9ece6a",
  pending: "#f7768e",
  partial: "#e0af68",
  empty: "#a9b1d6",
  unknown: "#565f89",
  loading: "#565f89",
} as const;

export type TriggerTone = keyof typeof TONE_COLORS;

export interface TriggerState {
  tone: TriggerTone;
  color: string;
  /** What the glyph shows. `"?"` (unknown) and `"0"` (a known-empty ledger)
   * are deliberately different characters — that is G5's whole point. */
  countText: string;
  /** Human summary for `title` / `aria-label`. Carries its own
   * `Session info…` lead-in, so a consumer that prefixes it AGAIN produces
   * `"Session info (zone 3): Session info: provisional — …"` (D5). */
  summary: string;
  /** The same human sentence with the `Session info` lead-in removed, for
   * consumers that supply their own prefix (e.g. the UI-Bridge registration,
   * which qualifies it with the zone). Keeping both spellings on the state —
   * rather than having each consumer slice the prefix off `summary` — is what
   * makes "exactly one `Session info`" a property of this module instead of a
   * convention every call site has to remember. */
  detail: string;
  /** Machine summary for `data-session-info-summary`. */
  dataSummary: string;
}

/**
 * The UI-Bridge registration label for a zone's session-info trigger.
 * Qualifies the trigger with its zone and states `Session info` EXACTLY ONCE
 * (D5) — pure, so the accessible-name contract is unit-testable without a DOM.
 */
export function sessionInfoTriggerLabel(zoneIndex: number, trigger: TriggerState): string {
  return `Session info (zone ${zoneIndex + 1}): ${trigger.detail}`;
}

/**
 * What the always-rendered trigger looks like. The deleted PR-only dropdown
 * returned `null` on every one of the first three branches below; rendering
 * them is the G5 fix. Pure — unit-tested.
 */
export function deriveTrigger(state: SessionInfoState): TriggerState {
  if (state.status === "loading") {
    return {
      tone: "loading",
      color: TONE_COLORS.loading,
      countText: "…",
      summary: "Session info: loading…",
      detail: "loading…",
      dataSummary: "loading",
    };
  }
  if (state.status === "unavailable" || !state.body) {
    const reason = state.reason ?? UNKNOWN_TEXT;
    return {
      tone: "unknown",
      color: TONE_COLORS.unknown,
      countText: "?",
      summary: `Session info unavailable — ${reason}`,
      detail: `unavailable — ${reason}`,
      dataSummary: `unavailable:${reason}`,
    };
  }

  const { prs } = state.body;
  const summary = summarizePrs(prs);
  const who = state.body.name.value ?? UNKNOWN_TEXT;

  // A provisional identity is reported BEFORE the PR roll-up, because it
  // qualifies the whole panel: if no provider ever started here, the id and
  // account below are a prediction and the PR ledger is about a session that
  // may not exist. Distinguishable from `unavailable` (we could not read) and
  // from a genuine zero — the same three-way honesty the land signal uses.
  if (isProvisionalIdentity(state.body.lifecycle)) {
    return {
      tone: "partial",
      color: TONE_COLORS.partial,
      countText: "~",
      summary: `Session info: provisional — ${PROVISIONAL_NOTE}`,
      detail: `provisional — ${PROVISIONAL_NOTE}`,
      dataSummary: "identity-provisional",
    };
  }

  if (prs.status !== "ok") {
    return {
      tone: "unknown",
      color: TONE_COLORS.unknown,
      countText: "?",
      summary: `Session info: ${who} — ${summary.text}`,
      detail: `${who} — ${summary.text}`,
      dataSummary: `prs-unavailable:${prs.reason ?? UNKNOWN_TEXT}`,
    };
  }

  const tone: TriggerTone =
    summary.unknownCount > 0
      ? "partial"
      : summary.openCount === 0
        ? "empty"
        : summary.allLanded
          ? "landed"
          : "pending";

  return {
    tone,
    color: TONE_COLORS[tone],
    countText: String(summary.openCount),
    summary: `Session info: ${who} — ${summary.text}`,
    detail: `${who} — ${summary.text}`,
    dataSummary: summary.text,
  };
}

export type PrChipTone = "landed" | "unknown" | "not-landed" | "open";

/**
 * Land signal → the words on the chip.
 *
 * The raw signal is a machine token; three of the four read badly in a
 * dropdown (`coord-label` most of all). An UNRECOGNISED signal falls through
 * to itself rather than to a generic "landed" — a newer runner backend can
 * add a signal this frontend has never heard of, and showing the token is
 * honest where inventing a friendly name for it would not be. Pure.
 */
export const LAND_SIGNAL_LABELS: Readonly<Record<string, string>> = {
  "github-merge": "merged",
  "ff-land": "landed (ff)",
  "coord-label": "landed (coord)",
};

/** Human label for a land signal; `null` ⇒ landed by a signal we did not
 * record, which is still a land. Pure. */
export function landSignalLabel(signal: string | null): string {
  if (signal === null) return "landed";
  return LAND_SIGNAL_LABELS[signal] ?? signal;
}

/**
 * The land-signal chip for one opened PR row. The three buckets render as
 * three distinct states; an unevaluable verdict says so WITH its reason
 * rather than being demoted to a confident negative (R1). Pure.
 */
export function prRowChip(
  pr: SessionPrOpened,
  landed: SessionPrLanded | undefined,
  unknown: SessionPrUnknown | undefined,
): { text: string; tone: PrChipTone } {
  if (landed) return { text: landSignalLabel(landed.landSignal), tone: "landed" };
  if (unknown) return { text: `unknown — ${unknown.reason}`, tone: "unknown" };
  // `prState === "closed"` reaches here ONLY when the backend recorded a
  // confident `not-landed`, which since 2026-08-26 it does only for a PR
  // GitHub still reports open — so a closed row with no land verdict is an
  // unknown that lost its reason, not a proven negative. Say the weaker
  // thing. (The strong claim used to read "closed, not landed", and it was
  // printed on every coord rebase-fast-forward land: those rewrite the shas,
  // so the ancestry probe backing it could never have passed.)
  if (pr.prState === "closed")
    return { text: "closed — land unverified", tone: "unknown" };
  return { text: pr.prState ?? "open", tone: "open" };
}

export interface PrPanelRow {
  key: string;
  repo: string;
  prNumber: number;
  /** `runner#663`. */
  label: string;
  /** `null` for a bare checkout-name repo — no owner, no link. */
  url: string | null;
  chip: { text: string; tone: PrChipTone };
}

/**
 * The panel's PR list: every PR in ANY of the three buckets, unlanded first.
 *
 * A landed-or-unknown row with no matching `opened` entry would otherwise be
 * invisible, so it is synthesized here rather than dropped — a row the ledger
 * knows about must not disappear from the surface built to show the ledger.
 * Pure — unit-tested.
 */
export function prPanelRows(prs: SessionPrs): PrPanelRow[] {
  if (prs.status !== "ok") return [];
  const landedBy = new Map(prs.landed.map((p) => [prKey(p), p]));
  const unknownBy = new Map(prs.unknown.map((p) => [prKey(p), p]));

  const opened = [...prs.opened];
  const seen = new Set(opened.map(prKey));
  for (const p of [...prs.landed, ...prs.unknown]) {
    const key = prKey(p);
    if (seen.has(key)) continue;
    seen.add(key);
    opened.push({
      repo: p.repo,
      prNumber: p.prNumber,
      branch: null,
      openedAt: null,
      prState: null,
    });
  }

  return sortPrsUnlandedFirst(opened, new Set(landedBy.keys())).map((pr) => {
    const key = prKey(pr);
    return {
      key,
      repo: pr.repo,
      prNumber: pr.prNumber,
      label: prLabel(pr),
      url: prGithubUrl(pr),
      chip: prRowChip(pr, landedBy.get(key), unknownBy.get(key)),
    };
  });
}

export interface InfoRowSpec {
  field: SessionInfoField;
  label: string;
  /** The RAW value for `data-session-info-value`; `null` ⇒ never observed, in
   * which case the attribute is omitted and `data-session-info-unknown` is
   * set, so a client never has to guess whether `""` meant empty or absent. */
  value: string | null;
  /** What the panel renders — `UNKNOWN_TEXT` when `value` is `null`. */
  display: string;
  /** Ids get a copy button. */
  copyable: boolean;
}

/** Epoch milliseconds → `YYYY-MM-DD HH:MM:SSZ`; `null` for a non-finite or
 * unset stamp, which renders as `unknown` rather than `1970-01-01`. Pure. */
export function formatEpochMs(ms: number | null): string | null {
  if (ms === null || !Number.isFinite(ms) || ms <= 0) return null;
  return `${new Date(ms).toISOString().slice(0, 19).replace("T", " ")}Z`;
}

/**
 * The panel's labelled rows, one per D1 field group, ALWAYS all fourteen —
 * a field with no value renders `unknown`, it does not vanish (R2). Pure —
 * unit-tested, so the UI Bridge id/field contract is checkable without a DOM.
 */
/** Evidence backing a session's identity. `provisional` is the load-bearing
 * one: the runner pins a session id and picks an account for EVERY terminal at
 * spawn, including a plain shell that never runs a provider, so an unqualified
 * id/account would be a confident claim about something that may never exist. */
export type IdentityEvidence = "confirmed" | "transcript" | "provisional";

/** Is this identity merely predicted? Treats an ABSENT field as NOT provisional
 * — an older runner that does not send `identityEvidence` must not have every
 * session relabelled; unknown provenance is not the same as known-provisional. */
export function isProvisionalIdentity(lifecycle: { identityEvidence?: IdentityEvidence }): boolean {
  return lifecycle.identityEvidence === "provisional";
}

/** Suffix appended to identity-derived rows when the identity is provisional.
 * Short enough to sit inline; the panel also carries the full explanation. */
export const PROVISIONAL_SUFFIX = " — provisional";

/** The one-line explanation shown in the panel when identity is provisional. */
export const PROVISIONAL_NOTE =
  "No provider has started in this terminal — the id and account are the " +
  "runner's spawn-time prediction, not an observation.";

export function sessionInfoRows(body: SessionInfoBody): InfoRowSpec[] {
  const { identity, name, account, placement, lifecycle, prs } = body;
  // The id and account come from the spawn-time seam, which fires for every
  // terminal. Without corroboration they are a PREDICTION, and the panel must
  // not print a prediction the way it prints an observation.
  const provisional = isProvisionalIdentity(lifecycle);
  const prsOk = prs.status === "ok";
  const prsReason = `unavailable — ${prs.reason ?? UNKNOWN_TEXT}`;

  const row = (
    field: SessionInfoField,
    label: string,
    value: string | null,
    display?: string,
    copyable = false,
  ): InfoRowSpec => ({
    field,
    label,
    value,
    display: display ?? value ?? UNKNOWN_TEXT,
    copyable,
  });

  return [
    row(
      "account",
      "Account",
      account.label,
      (account.label
        ? account.wrapper
          ? `${account.label} (${account.wrapper})`
          : account.label
        : UNKNOWN_TEXT) + (provisional && account.label ? PROVISIONAL_SUFFIX : ""),
    ),
    row("name", "Name", name.value),
    row("terminal-id", "Terminal id", identity.terminalId, undefined, true),
    row(
      "claude-session-id",
      "Claude id",
      identity.claudeSessionId,
      identity.claudeSessionId
        ? identity.claudeSessionId + (provisional ? PROVISIONAL_SUFFIX : "")
        : UNKNOWN_TEXT,
      true,
    ),
    row("fleet-handle", "Fleet handle", identity.fleetSessionHandle, undefined, true),
    row("tenant", "Tenant", identity.tenantId, undefined, true),
    row("task-run", "Task run", identity.taskRunId, undefined, true),
    row("working-dir", "Working dir", placement.workingDir, undefined, true),
    row("provider", "Provider", lifecycle.provider),
    row("origin", "Origin", lifecycle.origin),
    row(
      "opened-at",
      "Opened",
      String(lifecycle.openedAt),
      formatEpochMs(lifecycle.openedAt) ?? UNKNOWN_TEXT,
    ),
    row("restore-tier", "Restore tier", lifecycle.restoreTier),
    row(
      "prs-opened",
      "PRs opened",
      prsOk ? String(prs.openCount) : null,
      prsOk ? String(prs.openCount) : prsReason,
    ),
    row(
      "prs-landed",
      "PRs landed",
      prsOk ? String(prs.landedCount) : null,
      prsOk ? String(prs.landedCount) : prsReason,
    ),
  ];
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.length > 0 ? v : null;
}

function num(v: unknown, fallback: number): number {
  return typeof v === "number" && Number.isFinite(v) ? v : fallback;
}

function normalizePrs(raw: unknown): SessionPrs {
  const unavailable = (reason: string): SessionPrs => ({
    status: "unavailable",
    reason,
    opened: [],
    landed: [],
    unknown: [],
    openCount: 0,
    landedCount: 0,
    unknownCount: 0,
  });
  if (!raw || typeof raw !== "object") return unavailable("malformed_prs");
  const v = raw as Partial<SessionPrs>;
  if (v.status !== "ok") return unavailable(str(v.reason) ?? "unspecified");
  const opened = Array.isArray(v.opened) ? v.opened : [];
  const landed = Array.isArray(v.landed) ? v.landed : [];
  const unknown = Array.isArray(v.unknown) ? v.unknown : [];
  return {
    status: "ok",
    reason: null,
    opened,
    landed,
    unknown,
    openCount: num(v.openCount, opened.length),
    landedCount: num(v.landedCount, landed.length),
    unknownCount: num(v.unknownCount, unknown.length),
  };
}

/**
 * Envelope → state. A malformed or non-`available` payload becomes an
 * `unavailable` state WITH a reason — there is no branch that produces a
 * bare empty projection, because "no such session" and "the store is
 * unreachable" must not render identically.
 */
export function normalizeSessionInfo(raw: unknown): SessionInfoState {
  if (!raw || typeof raw !== "object")
    return { status: "unavailable", reason: "malformed_envelope", body: null };
  const v = raw as {
    available?: boolean;
    reason?: unknown;
    identity?: SessionIdentity;
    name?: SessionName;
    account?: SessionAccount;
    placement?: SessionPlacement;
    lifecycle?: SessionLifecycleInfo;
    prs?: unknown;
  };
  if (v.available !== true) {
    return { status: "unavailable", reason: str(v.reason) ?? "unspecified", body: null };
  }
  if (!v.identity || !v.name || !v.account || !v.placement || !v.lifecycle) {
    return { status: "unavailable", reason: "malformed_envelope", body: null };
  }
  return {
    status: "ok",
    reason: null,
    body: {
      identity: v.identity,
      name: { value: str(v.name.value), source: str(v.name.source) ?? UNKNOWN_TEXT },
      account: v.account,
      placement: v.placement,
      lifecycle: v.lifecycle,
      prs: normalizePrs(v.prs),
    },
  };
}

/**
 * Poll `session_info_get` while mounted. `claudeSessionId` is undefined for
 * plain PTY tabs and not-yet-reconciled spawns — no fetch, and the caller
 * renders no trigger at all (a tab with no Claude session has no session
 * info, which is a different statement from "its session info is unknown").
 */
export function useSessionInfo(
  claudeSessionId: string | undefined,
  pollMs = 60_000,
): SessionInfoState & { refresh: () => void } {
  // Cache keyed by session id: when the zone is reassigned to another
  // session, render falls back to LOADING until that session's fetch lands —
  // no setState-in-effect reset, no flash of the previous session's identity.
  const [cached, setCached] = useState<{ sid: string; data: SessionInfoState } | null>(null);
  const inFlight = useRef(false);

  const fetchInfo = useCallback(async () => {
    if (!claudeSessionId || inFlight.current) return;
    inFlight.current = true;
    try {
      const raw = await invoke<unknown>("session_info_get", { claudeSessionId });
      setCached({ sid: claudeSessionId, data: normalizeSessionInfo(raw) });
    } catch (err) {
      // A hard Err is an IPC/caller fault, not a data condition. Surface it as
      // an explicit unavailable state rather than keeping a stale projection
      // or silently rendering nothing.
      console.error("session_info_get failed:", err);
      setCached({
        sid: claudeSessionId,
        data: { status: "unavailable", reason: "ipc_error", body: null },
      });
    } finally {
      inFlight.current = false;
    }
  }, [claudeSessionId]);

  useEffect(() => {
    if (!claudeSessionId) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- fetchInfo is async; setCached fires after the IPC await, never synchronously in the effect body
    void fetchInfo();
    const timer = setInterval(() => void fetchInfo(), pollMs);
    return () => clearInterval(timer);
  }, [claudeSessionId, fetchInfo, pollMs]);

  const state = claudeSessionId && cached?.sid === claudeSessionId ? cached.data : LOADING_STATE;
  return { ...state, refresh: () => void fetchInfo() };
}
