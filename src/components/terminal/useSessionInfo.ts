import { useCallback, useSyncExternalStore } from "react";
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
  /** The work-axis finished marker (`/finish-session`), epoch ms; `null` =
   * not marked finished. Orthogonal to `state`. */
  finishedAt: number | null;
  /** Free text recorded with `finishedAt`. */
  finishReason: string | null;
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
  /** `"rebase_land_or_abandoned"` | `"coord_chip_on_open_pr"` |
   * `"pr_state_unobserved"` | `"ref_stale"` | `"head_object_missing"` |
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
  /**
   * Has the reconciler EVER resolved a repo set for this session?
   *
   * `false` ⇒ nothing was ever searched, so an empty ledger asserts NOTHING.
   * Before this field the panel printed `no PRs attributed to this session`
   * identically for a session that genuinely opened none and for one the
   * reconciler silently dropped every tick (its cwd was the workspace parent,
   * which holds the clones but is not itself a git repo) — a confident default
   * standing in for an unknown.
   */
  scanned: boolean;
  /** The repo roots last searched. Empty WITH `scanned: true` ⇒ the working
   * dir resolved to no git repositories at all. */
  scannedRepos: string[];
}

/**
 * Which tenant each half of the session acts as (plan
 * `2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential` P0).
 * Mirrors the Rust `SessionTenancy`; keep the two spellings in step.
 *
 * Three values because three mechanisms decide them — the coord row stamped at
 * spawn, the runner's data-plane writes, and the coord-mcp credential — and a
 * session labelled one tenant has written to another through the third.
 */
/** Mirrors the Rust `TenancyRepoExpected`; keep the spellings in step. */
export interface TenancyRepoExpected {
  /** `"resolved"` | `"no_repo"` | `"repo_unregistered"` | `"several"` |
   * `"pending"` | `"unknown"` (with `reason`). */
  status: string;
  tenantId: string | null;
  /** Display only — coord's own slug for that tenant, once an answer carried it. */
  tenantSlug: string | null;
  /** Every owner when `several`. */
  tenantIds: string[];
  repo: string | null;
  source: string | null;
  observedAt: string | null;
  reason: string | null;
  /** Set when the session's tenant was NAMED — then that is the expectation. */
  callerNamedTenantId: string | null;
  callerNamedSource: string | null;
  /** `"agree"` | `"mismatch"` | `"expected_unknown"` | `"answer_unstamped"` |
   * `"no_repo"`; `null` before the session's first coord tool call. */
  verdict: string | null;
  verdictDetail: string | null;
  verdictAt: string | null;
}

/** One leg's tenant, in a `mismatch` display. */
export interface TenantConflict {
  /** `credential` | `data_plane` | `row_stamp` | `spawn_device_default` |
   * `repo_expected` | `coord_row`. */
  leg: string;
  tenantId: string;
}

/**
 * The runner's ONE derived answer to "which tenant is this session?" —
 * `tenancy.display`, computed in Rust (`commands/session_tenant_display.rs`)
 * so the dropdown row, the zone badge and the UI-Bridge oracle render one fact
 * instead of three re-derivations (plan
 * `2026-10-17-session-info-tenant-row-renders-unknown-for-a-resolved-tenant`).
 * Spellings mirror the Rust struct; the contract fixture
 * `__fixtures__/session-tenancy-display.json` pins both sides.
 */
export interface TenantDisplay {
  /** `resolved` | `mismatch` | `stamp_only` (only the spawn stamp names one) |
   * `unknown` (with `reason`). */
  status: "resolved" | "mismatch" | "stamp_only" | "unknown";
  /** The acting tenant the precedence picked; `null` when `unknown`, and in a
   * `mismatch` where no leg could be picked as the acting one. */
  tenantId: string | null;
  /** Coord's own slug, only from an answer that tenant gave this session. */
  tenantSlug: string | null;
  /** `credential` | `credential_default_slot` | `data_plane` |
   * `data_plane_device` | `row_stamp`. */
  source: string | null;
  /** Why the acting tenant is not (fully) established: set when no tenant was
   * picked (`unknown`, or a `mismatch` with no acting tenant), for
   * `stamp_only`, and — as the credential's reason — for a data-plane source
   * while the credential is unknown. */
  reason: string | null;
  conflicts: TenantConflict[];
  conflictDetail: string | null;
  bound: {
    /** `measured` | `unknown` — an unknown set is never an empty one. */
    status: "measured" | "unknown";
    tenantIds: string[];
    reason: string | null;
  };
  /** `null` — never `false` — when the bound set is unknown. */
  multiBound: boolean | null;
  /** Whether `tenantId` is one of the bound tenants; `null` when unknowable. */
  actingInBoundSet: boolean | null;
}

/** The fifth leg: the tenant coord's own session row was created under, as
 * this runner process registered it or as coord's resume-probe answer named
 * it. */
export interface TenancyCoordRow {
  /** `registered` (the row's tenant is known — see `source`) | `unknown`
   * (with `reason`) | `not_applicable` (the session's coord row IS the hosting
   * terminal's own `SessionRegistry` row, decided by coord id — whether reached
   * through the pinned terminal or a registrar entry auto-bind adopted — and
   * the `dataPlane` leg names the row's own tenant: the one this runner
   * authored the row's create body with, or the one coord's answer to a
   * resume probe named: never a warning, never blocks agreement). */
  status: string;
  tenantId: string | null;
  /** When registered: `registrar` (this runner minted the row) |
   * `resume_probe` (the hosting terminal's resumed row, whose tenant coord's
   * resume-probe answer named and the `dataPlane` leg does not show). */
  source: string | null;
  /** unknown: `not_registered_by_this_process` | `registered_without_tenant` |
   * `adopted_row_tenant_not_observed` | `terminal_row_not_wired` |
   * `terminal_row_tenant_unresolved` | `terminal_row_resume_unconfirmed` |
   * `registrar_state_poisoned` |
   * `row_of_another_terminal` | `registrar_unavailable`;
   * not_applicable: `terminal_plane_row_is_the_data_plane_leg`. */
  reason: string | null;
}

export interface SessionTenancy {
  /** `tenantId: null` ⇒ the spawn chose none — NOT "no tenant". Coord's row
   * then holds what the registrar sent: the machine pin, else the paired
   * default binding (read {@link coordRow} for the value it recorded). */
  row: {
    tenantId: string | null;
    /** The machine's default tenant as read when this session was SPAWNED —
     * the expected tenant of a spawn that chose none, compared only when nothing
     * else names one. Persisted with the session's lifecycle record, so a
     * restart keeps the spawn's value. `null` is ambiguous on its own — read
     * {@link spawnDeviceDefaultStatus}. */
    spawnDeviceDefaultTenantId: string | null;
    /** `"recorded"` (the value above is the spawn's, `null` = no default then) |
     * `"unknown"` (NOT recorded — never compared, never read as "no default"). */
    spawnDeviceDefaultStatus: string;
    /** Why the status is unknown: `not_recorded` | `unparseable`, or
     * `runner_predates_status` for a runner that serves no status field. */
    spawnDeviceDefaultReason: string | null;
    /** The machine's CURRENT default tenant — context only, never compared (a
     * switch re-points future sessions only). */
    currentDeviceDefaultTenantId: string | null;
  };
  dataPlane: {
    /** `"owned"` | `"device"` | `"unresolved"` | `"unknown"`. */
    status: string;
    tenantId: string | null;
    reason: string | null;
  };
  credential: {
    /** `"resolved"` | `"unknown"` — an unknown is never rendered as a default. */
    status: string;
    tenantId: string | null;
    /** `"tenant"` | `"default"`; `null` while unknown. */
    slot: string | null;
    reason: string | null;
    posture: {
      /** `"observed"` | `"unknown"`. */
      status: string;
      value: string | null;
      canAnswer: boolean | null;
      reason: string | null;
    };
  };
  /** The fourth leg: the tenant the repo in the session's spawn workdir
   * belongs to — what every coord answer is compared with — and the latest
   * verdict of that comparison (plan
   * `2026-09-20-a-sessions-tenant-follows-its-repo-and-every-coord-answer-names-its-tenant`
   * Phase 2). `null` when the runner predates the leg — UNKNOWN, not agreement. */
  repoExpected: TenancyRepoExpected | null;
  /** The fifth leg — `null` when the runner predates it (UNKNOWN). */
  coordRow: TenancyCoordRow | null;
  /** The display projection — `null` when the runner predates it, which the
   * Tenant row renders as `unknown — runner predates tenancy.display`. */
  display: TenantDisplay | null;
  /** The tenants that ARE known name more than one tenant. `false` is NOT
   * agreement — read {@link divergence}. */
  diverged: boolean;
  /** `"diverged"` | `"agree"` (credential AND an expected tenant known, and
   * equal) | `"unknown"` (a half of the comparison is unknown). */
  divergence: "diverged" | "agree" | "unknown";
}

export interface SessionInfoBody {
  identity: SessionIdentity;
  name: SessionName;
  account: SessionAccount;
  placement: SessionPlacement;
  lifecycle: SessionLifecycleInfo;
  prs: SessionPrs;
  /** `null` when the envelope carried no well-formed tenancy block — UNKNOWN,
   * not "agrees". */
  tenancy: SessionTenancy | null;
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
  | "spawn-stamp"
  | "task-run"
  | "working-dir"
  | "recorded-page"
  | "recorded-zone"
  | "provider"
  | "origin"
  | "opened-at"
  | "restore-tier"
  | "prs-opened"
  | "prs-landed"
  | "prs-scanned";

/** `terminal-session-info-<field>-<zoneIndex>`; `field` is also `"trigger"` /
 * `"panel"` for the two container elements and `"prs-empty-state"` for the
 * line that says WHY the PR list is empty. Pure — unit-tested. */
export function sessionInfoElementId(
  field: SessionInfoField | "trigger" | "panel" | "prs-empty-state",
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

/**
 * `"not-landed"` is deliberately retained but is now RARE: since the land
 * cascade stopped inferring a negative from a failed ancestry test, the only
 * confident negative is a PR GitHub positively reports open, which renders as
 * `"open"`. The tone survives for legacy rows written by an older build, whose
 * `prState === "closed"` with no verdict still reaches the closed branch below.
 */
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
 * Unknown-reason token → the words on the chip.
 *
 * The backend records WHY a land could not be established as a machine token;
 * rendering `unknown — rebase_land_or_abandoned` in a dropdown puts snake_case
 * in front of a person. Each phrase below says what was actually established,
 * without implying the stronger claim the token replaced.
 *
 * Same fall-through rule as `landSignalLabel`: an UNRECOGNISED reason renders
 * as itself. A newer backend can record a reason this frontend has never heard
 * of, and showing the token is honest where inventing a phrase for it is not.
 * Pure.
 */
export const LAND_UNKNOWN_REASONS: Readonly<Record<string, string>> = {
  rebase_land_or_abandoned: "not on the base branch — may have rebase-landed",
  coord_chip_on_open_pr: "coord says landed, GitHub says open",
  pr_state_unobserved: "GitHub state not observed",
  ref_stale: "local base ref is stale",
  head_object_missing: "head commit not present locally",
  no_base_ref: "no local base ref",
  not_a_repo: "no local checkout",
  unspecified: "reason not recorded",
};

/** Human label for an unknown-land reason. Pure. */
export function landUnknownReasonLabel(reason: string): string {
  return LAND_UNKNOWN_REASONS[reason] ?? reason;
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
  if (unknown)
    return {
      text: `unknown — ${landUnknownReasonLabel(unknown.reason)}`,
      tone: "unknown",
    };
  // `prState === "closed"` reaches here ONLY when the backend recorded a
  // confident `not-landed`, which since 2026-08-26 it does only for a PR
  // GitHub still reports open — so a closed row with no land verdict is an
  // unknown that lost its reason, not a proven negative. Say the weaker
  // thing. (The strong claim used to read "closed, not landed", and it was
  // printed on every coord rebase-fast-forward land: those rewrite the shas,
  // so the ancestry probe backing it could never have passed.)
  if (pr.prState === "closed") return { text: "closed — land unverified", tone: "unknown" };
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
  /** The full text behind a truncated `display` (hover / accessible name);
   * `undefined` ⇒ the display itself. */
  title?: string;
}

/**
 * The recorded `zoneIndex` as the operator reads it — 1-based, matching the
 * trigger label ("zone 3"), with the `-1` sentinel spelled out.
 *
 * `-1` stays a RAW value rather than becoming `unknown`: the record definitely
 * holds it, so what needs spelling is its MEANING, not its presence. It is also
 * deliberately not rendered as a confident "unassigned" — the store cannot tell
 * the frontend ASSERTING that a tab was dragged out of every zone from a writer
 * that simply had no view of the grid, and those are different claims.
 *
 * This is the RECORD's placement, not the live tab's, and the two can disagree:
 * restore filters by the recorded page, so a session whose record drifted onto
 * a page the grid never mounts is silently unrestorable. Surfacing it is what
 * makes that visible before the next restart rather than after it. Pure.
 */
export function formatRecordedZone(zoneIndex: number): string {
  if (!Number.isInteger(zoneIndex)) return UNKNOWN_TEXT;
  return zoneIndex >= 0 ? `Zone ${zoneIndex + 1}` : "none recorded";
}

/** Epoch milliseconds → `YYYY-MM-DD HH:MM:SSZ`; `null` for a non-finite or
 * unset stamp, which renders as `unknown` rather than `1970-01-01`. Pure. */
export function formatEpochMs(ms: number | null): string | null {
  if (ms === null || !Number.isFinite(ms) || ms <= 0) return null;
  return `${new Date(ms).toISOString().slice(0, 19).replace("T", " ")}Z`;
}

/** A uuid's discriminating stem. */
function shortId(id: string): string {
  return id.length > 8 ? id.slice(0, 8) : id;
}

/** How each display source reads after the tenant. */
const DISPLAY_SOURCE_TEXT: Record<string, string> = {
  credential: "via credential",
  credential_default_slot: "via default-slot credential (device default binding)",
  data_plane: "via runner data plane",
  data_plane_device: "via runner data plane (device default binding)",
  row_stamp: "spawn stamp only — acting tenant not observed",
};

/** Display sources that do NOT come from the credential — a `reason` beside
 * one of these is the credential's own unknown reason. */
export const DATA_PLANE_SOURCES: ReadonlySet<string> = new Set(["data_plane", "data_plane_device"]);

/** The Tenant row's text when the runner sends no display. */
export const TENANT_DISPLAY_ABSENT_TEXT = "unknown — runner predates tenancy.display";

/** The bound-set suffix: `· 1 of 4 bound`, withheld on a single binding, and
 * an explicit unknown rather than silence when the set could not be read. */
function boundSuffix(d: TenantDisplay): string {
  if (d.bound.status !== "measured") {
    return ` · bound set unknown — ${d.bound.reason ?? "no reason given"}`;
  }
  if (d.multiBound !== true) return "";
  const n = d.bound.tenantIds.length;
  return d.actingInBoundSet === false ? ` · not among ${n} bound` : ` · 1 of ${n} bound`;
}

/** How a `registered` coord row's tenant became known, in words. Pure. */
export function coordRowSourceText(source: string | null): string {
  if (source === "resume_probe") return "as coord answered this runner's resume probe";
  if (source === "registrar") return "as registered by this runner";
  // An unrecognised or absent source is shown as such, never given a provenance.
  return `source ${source ?? "unstated"}`;
}

/** The Tenant row's hover text: the full acting id, the full bound set, every
 * conflict and coord's own row. */
function tenantRowTitle(d: TenantDisplay, coordRow: TenancyCoordRow | null): string {
  const lines: string[] = [];
  lines.push(
    d.tenantId
      ? `Acting tenant: ${d.tenantId}${d.tenantSlug ? ` (${d.tenantSlug})` : ""} — source ${d.source ?? "?"}`
      : `Acting tenant: unknown — ${d.reason ?? "no reason given"}`,
  );
  if (d.status === "stamp_only") {
    lines.push(
      `Only the spawn stamp names it; the acting tenant is unknown (${d.reason ?? "no reason given"}).`,
    );
  }
  if (d.tenantId && DATA_PLANE_SOURCES.has(d.source ?? "") && d.reason) {
    lines.push(`Credential: unknown — ${d.reason}`);
  }
  for (const c of d.conflicts) lines.push(`Conflict — ${c.leg}: ${c.tenantId}`);
  if (d.conflictDetail) lines.push(`Detail: ${d.conflictDetail}`);
  lines.push(
    d.bound.status === "measured"
      ? `Bound tenants (${d.bound.tenantIds.length}): ${d.bound.tenantIds.join(", ") || "none"}`
      : `Bound tenants: unknown — ${d.bound.reason ?? "no reason given"}`,
  );
  lines.push(
    !coordRow
      ? "Coord session row: unknown — runner predates the leg"
      : coordRow.status === "registered"
        ? `Coord session row: ${coordRow.tenantId ?? "?"} (${coordRowSourceText(coordRow.source)})`
        : coordRow.status === "not_applicable"
          ? "Coord session row: terminal-hosted — its tenant is the data-plane leg above"
          : `Coord session row: unknown — ${coordRow.reason ?? "no reason given"}`,
  );
  return lines.join("\n");
}

/**
 * The Tenant row, fed ONLY by `tenancy.display` — the old rendering read the
 * spawn stamp (`identity.tenantId`) and so said `unknown` for every session
 * whose spawn chose no tenant, however resolved its credential was. Pure.
 */
export function tenantRow(
  tenancy: SessionTenancy | null,
): Pick<InfoRowSpec, "value" | "display" | "title"> {
  const d = tenancy?.display ?? null;
  if (!d) return { value: null, display: TENANT_DISPLAY_ABSENT_TEXT };
  const title = tenantRowTitle(d, tenancy?.coordRow ?? null);
  // Checked BEFORE the no-tenant fallback: a mismatch with no acting tenant is
  // still a mismatch, and its conflict groups are the point.
  if (d.status === "mismatch") {
    // Grouped by tenant, so two legs that AGREE are not printed as unequal.
    const byTenant = new Map<string, string[]>();
    for (const c of d.conflicts) {
      const key = c.tenantId.toLowerCase();
      byTenant.set(key, [...(byTenant.get(key) ?? []), c.leg]);
    }
    const groups = [...byTenant].map(([t, legs]) => `${shortId(t)} (${legs.join(", ")})`);
    const text = groups.length > 0 ? groups.join(" ≠ ") : "(no leg listed)";
    // A verdict-only mismatch (every leg names one tenant; coord answered from
    // another) is explained by the detail, so it is shown inline too.
    const detail = d.conflictDetail ? ` — ${d.conflictDetail}` : "";
    const acting = d.tenantId ? "" : ` — acting tenant unknown — ${d.reason ?? "no reason given"}`;
    return { value: d.tenantId, display: `MISMATCH ${text}${detail}${acting}`, title };
  }
  if (d.status === "unknown" || !d.tenantId) {
    return { value: null, display: `unknown — ${d.reason ?? "no reason given"}`, title };
  }
  const id = shortId(d.tenantId);
  const name = d.tenantSlug ? `${d.tenantSlug} (${id})` : id;
  const via = DISPLAY_SOURCE_TEXT[d.source ?? ""] ?? `via ${d.source ?? "?"}`;
  // A data-plane answer beside an UNREADABLE credential says so, rather than
  // reading as if the credential agreed.
  const credential =
    DATA_PLANE_SOURCES.has(d.source ?? "") && d.reason ? ` · credential unknown — ${d.reason}` : "";
  return {
    value: d.tenantId,
    display: `${name} · ${via}${credential}${boundSuffix(d)}`,
    title,
  };
}

/**
 * The `Spawned for` row: the tenant the spawn STAMPED, labelled as a stamp.
 * A spawn that chose none names the device default it was spawned under — a
 * hypothesis about coord's row, never "the tenant". Pure.
 */
export function spawnStampRow(
  tenancy: SessionTenancy | null,
): Pick<InfoRowSpec, "value" | "display"> {
  if (!tenancy) return { value: null, display: UNKNOWN_TEXT };
  const stamp = tenancy.row.tenantId;
  if (stamp) return { value: stamp, display: stamp };
  return {
    value: null,
    display:
      tenancy.row.spawnDeviceDefaultStatus === "recorded"
        ? `none — device default at spawn: ${tenancy.row.spawnDeviceDefaultTenantId ?? "none"}`
        : `not recorded (${tenancy.row.spawnDeviceDefaultReason ?? "no reason given"})`,
  };
}

/**
 * The panel's labelled rows, one per D1 field group, ALWAYS all eighteen —
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
  const { identity, name, account, placement, lifecycle, prs, tenancy } = body;
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
    { field: "tenant", label: "Tenant", copyable: true, ...tenantRow(tenancy) },
    { field: "spawn-stamp", label: "Spawned for", copyable: true, ...spawnStampRow(tenancy) },
    row("task-run", "Task run", identity.taskRunId, undefined, true),
    row("working-dir", "Working dir", placement.workingDir, undefined, true),
    // The two placement fields the projection has always returned and nothing
    // ever rendered. They are what BOOT RESTORE keys off, so a drift here is
    // invisible until a restart drops the session.
    row("recorded-page", "Recorded page", placement.pageId || null, undefined, true),
    row(
      "recorded-zone",
      "Recorded zone",
      // `-1` IS a value the record holds, so it stays raw; only a missing or
      // non-integer index is an absence. Matches `backendValueForField` in
      // `scripts/uibridge-session-info.test.mjs`, which compares raw to raw.
      Number.isInteger(placement.zoneIndex) ? String(placement.zoneIndex) : null,
      formatRecordedZone(placement.zoneIndex),
    ),
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
    // The row that makes `PRs opened: 0` interpretable. `null` (⇒ `unknown` +
    // `data-session-info-unknown`) is the honest value when nothing was ever
    // searched: a count of zero repos and "we never looked" are different
    // claims and must not render as the same string.
    row(
      "prs-scanned",
      "Repos scanned",
      prsOk && prs.scanned ? String(prs.scannedRepos.length) : null,
      prsOk ? prsScannedDisplay(prs) : prsReason,
    ),
  ];
}

/** What the `Repos scanned` row shows. Pure — unit-tested. */
export function prsScannedDisplay(prs: SessionPrs): string {
  if (!prs.scanned) return NOT_SCANNED_TEXT;
  if (prs.scannedRepos.length === 0) return "0 — no git repos under the working dir";
  return String(prs.scannedRepos.length);
}

/** Shown wherever the reconciler has never resolved a repo set for a session. */
export const NOT_SCANNED_TEXT = "not scanned yet";

/**
 * The panel's empty-PR-list line. Three DIFFERENT sentences for three
 * different claims — "we never looked", "we looked and your working dir holds
 * no repos", and the genuine "you opened no PRs in the repos we searched".
 * Collapsing them into one line is the confident-default-for-unknown failure
 * this panel exists to avoid. Pure — unit-tested.
 */
export function prsEmptyStateText(prs: SessionPrs, workingDir: string | null): string {
  if (!prs.scanned) {
    return `${NOT_SCANNED_TEXT} — the PR reconciler has not resolved this session`;
  }
  if (prs.scannedRepos.length === 0) {
    return `no git repos found under ${workingDir ?? UNKNOWN_TEXT} — nothing was searched`;
  }
  const n = prs.scannedRepos.length;
  return `no PRs attributed to this session (searched ${n} repo${n === 1 ? "" : "s"})`;
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
    scanned: false,
    scannedRepos: [],
  });
  if (!raw || typeof raw !== "object") return unavailable("malformed_prs");
  const v = raw as Partial<SessionPrs>;
  if (v.status !== "ok") return unavailable(str(v.reason) ?? "unspecified");
  const opened = Array.isArray(v.opened) ? v.opened : [];
  const landed = Array.isArray(v.landed) ? v.landed : [];
  const unknown = Array.isArray(v.unknown) ? v.unknown : [];
  // An older runner sends neither field. That is NOT "scanned with no repos" —
  // it is "this runner cannot say", which lands in the same not-scanned bucket
  // rather than being back-filled into a confident claim.
  const scannedRepos = Array.isArray(v.scannedRepos)
    ? v.scannedRepos.filter((r): r is string => typeof r === "string")
    : [];
  return {
    status: "ok",
    reason: null,
    opened,
    landed,
    unknown,
    openCount: num(v.openCount, opened.length),
    landedCount: num(v.landedCount, landed.length),
    unknownCount: num(v.unknownCount, unknown.length),
    scanned: v.scanned === true,
    scannedRepos,
  };
}

/** Tenancy block → typed, or `null` when it is absent or malformed. Pure. */
export function normalizeTenancy(raw: unknown): SessionTenancy | null {
  if (!raw || typeof raw !== "object") return null;
  const v = raw as {
    row?: {
      tenantId?: unknown;
      spawnDeviceDefaultTenantId?: unknown;
      spawnDeviceDefaultStatus?: unknown;
      spawnDeviceDefaultReason?: unknown;
      currentDeviceDefaultTenantId?: unknown;
    };
    dataPlane?: { status?: unknown; tenantId?: unknown; reason?: unknown };
    credential?: {
      status?: unknown;
      tenantId?: unknown;
      slot?: unknown;
      reason?: unknown;
      posture?: { status?: unknown; value?: unknown; canAnswer?: unknown; reason?: unknown };
    };
    repoExpected?: Record<string, unknown> | null;
    coordRow?: unknown;
    display?: unknown;
    diverged?: unknown;
    divergence?: unknown;
  };
  const credentialStatus = str(v.credential?.status);
  const dataPlaneStatus = str(v.dataPlane?.status);
  if (!v.row || !credentialStatus || !dataPlaneStatus || typeof v.diverged !== "boolean") {
    return null;
  }
  const posture = v.credential?.posture;
  // A runner that predates the status field flattened "not recorded" into a
  // null tenant, so a null there is UNKNOWN; a named tenant was recorded.
  const spawnDefaultTenant = str(v.row.spawnDeviceDefaultTenantId);
  const spawnDefaultStatus =
    str(v.row.spawnDeviceDefaultStatus) ?? (spawnDefaultTenant ? "recorded" : UNKNOWN_TEXT);
  const spawnDefaultReason =
    str(v.row.spawnDeviceDefaultReason) ??
    (str(v.row.spawnDeviceDefaultStatus) === null && spawnDefaultStatus === UNKNOWN_TEXT
      ? "runner_predates_status"
      : null);
  // A runner that predates `divergence` can only say `diverged`; its `false`
  // also covered "could not compare", so it reads UNKNOWN, never agreement.
  const divergence: SessionTenancy["divergence"] =
    v.divergence === "diverged" || v.divergence === "agree" || v.divergence === "unknown"
      ? v.divergence
      : v.diverged
        ? "diverged"
        : "unknown";
  return {
    row: {
      tenantId: str(v.row.tenantId),
      spawnDeviceDefaultTenantId: spawnDefaultTenant,
      spawnDeviceDefaultStatus: spawnDefaultStatus,
      spawnDeviceDefaultReason: spawnDefaultReason,
      currentDeviceDefaultTenantId: str(v.row.currentDeviceDefaultTenantId),
    },
    dataPlane: {
      status: dataPlaneStatus,
      tenantId: str(v.dataPlane?.tenantId),
      reason: str(v.dataPlane?.reason),
    },
    credential: {
      status: credentialStatus,
      tenantId: str(v.credential?.tenantId),
      slot: str(v.credential?.slot),
      reason: str(v.credential?.reason),
      posture: {
        status: str(posture?.status) ?? UNKNOWN_TEXT,
        value: str(posture?.value),
        canAnswer: typeof posture?.canAnswer === "boolean" ? posture.canAnswer : null,
        reason: str(posture?.reason),
      },
    },
    repoExpected: normalizeRepoExpected(v.repoExpected),
    coordRow: normalizeCoordRow(v.coordRow),
    display: normalizeTenantDisplay(v.display),
    diverged: v.diverged,
    divergence,
  };
}

/** The fifth tenancy leg → typed, or `null` when absent/malformed. Pure. */
export function normalizeCoordRow(raw: unknown): TenancyCoordRow | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const status = str(r.status);
  if (!status) return null;
  return { status, tenantId: str(r.tenantId), source: str(r.source), reason: str(r.reason) };
}

const DISPLAY_STATUSES = ["resolved", "mismatch", "stamp_only", "unknown"] as const;

/** `tenancy.display` → typed, or `null` when absent/malformed — which the
 * Tenant row renders as an explicit unknown, never as a tenant. Pure. */
export function normalizeTenantDisplay(raw: unknown): TenantDisplay | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const status = DISPLAY_STATUSES.find((s) => s === r.status);
  if (!status) return null;
  // A missing or malformed bound set does NOT discard the display — the acting
  // tenant is still the runner's answer. The set alone reads `unknown`.
  const bound: Record<string, unknown> =
    r.bound && typeof r.bound === "object" && !Array.isArray(r.bound)
      ? (r.bound as Record<string, unknown>)
      : {};
  const boundStatus = bound.status === "measured" ? "measured" : "unknown";
  const strings = (x: unknown): string[] =>
    Array.isArray(x) ? x.filter((t): t is string => typeof t === "string") : [];
  return {
    status,
    tenantId: str(r.tenantId),
    tenantSlug: str(r.tenantSlug),
    source: str(r.source),
    reason: str(r.reason),
    conflicts: Array.isArray(r.conflicts)
      ? r.conflicts.flatMap((c) => {
          const leg = str((c as { leg?: unknown })?.leg);
          const tenantId = str((c as { tenantId?: unknown })?.tenantId);
          return leg && tenantId ? [{ leg, tenantId }] : [];
        })
      : [],
    conflictDetail: str(r.conflictDetail),
    bound: {
      status: boundStatus,
      tenantIds: boundStatus === "measured" ? strings(bound.tenantIds) : [],
      // An unknown set always carries a reason: `malformed` when the block
      // itself was not a well-formed `bound`, `reason_missing` when it said
      // `unknown` and gave none.
      reason:
        str(bound.reason) ??
        (boundStatus === "unknown"
          ? bound.status === "unknown"
            ? "reason_missing"
            : "malformed"
          : null),
    },
    // An unknown set never yields a confident `false`.
    multiBound:
      boundStatus === "measured" && typeof r.multiBound === "boolean" ? r.multiBound : null,
    actingInBoundSet:
      boundStatus === "measured" && typeof r.actingInBoundSet === "boolean"
        ? r.actingInBoundSet
        : null,
  };
}

/** The fourth tenancy leg → typed, or `null` when absent/malformed. Pure. */
export function normalizeRepoExpected(raw: unknown): TenancyRepoExpected | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const status = str(r.status);
  if (!status) return null;
  return {
    status,
    tenantId: str(r.tenantId),
    tenantSlug: str(r.tenantSlug),
    tenantIds: Array.isArray(r.tenantIds)
      ? r.tenantIds.filter((t): t is string => typeof t === "string")
      : [],
    repo: str(r.repo),
    source: str(r.source),
    observedAt: str(r.observedAt),
    reason: str(r.reason),
    callerNamedTenantId: str(r.callerNamedTenantId),
    callerNamedSource: str(r.callerNamedSource),
    verdict: str(r.verdict),
    verdictDetail: str(r.verdictDetail),
    verdictAt: str(r.verdictAt),
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
    tenancy?: unknown;
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
      tenancy: normalizeTenancy(v.tenancy),
    },
  };
}

/**
 * One shared poll per session id.
 *
 * Several components of one terminal tab read the same projection — the
 * session-info dropdown and the tenant badge today. Each used to run its own
 * 60 s `session_info_get` poll, so every zone paid two IPC round-trips (and
 * the runner two tenancy resolutions) per minute for one answer. Subscribers
 * with the same session id now share one entry: one poll, one cached state,
 * released when the last subscriber unmounts.
 */
interface SessionInfoEntry {
  state: SessionInfoState;
  listeners: Set<() => void>;
  timer: ReturnType<typeof setInterval> | null;
  inFlight: boolean;
}

const sessionInfoEntries = new Map<string, SessionInfoEntry>();

function sessionInfoEntry(sid: string): SessionInfoEntry {
  let entry = sessionInfoEntries.get(sid);
  if (!entry) {
    entry = { state: LOADING_STATE, listeners: new Set(), timer: null, inFlight: false };
    sessionInfoEntries.set(sid, entry);
  }
  return entry;
}

async function fetchSessionInfo(sid: string): Promise<void> {
  const entry = sessionInfoEntries.get(sid);
  if (!entry || entry.inFlight) return;
  entry.inFlight = true;
  let next: SessionInfoState;
  try {
    const raw = await invoke<unknown>("session_info_get", { claudeSessionId: sid });
    next = normalizeSessionInfo(raw);
  } catch (err) {
    // A hard Err is an IPC/caller fault, not a data condition. Surface it as
    // an explicit unavailable state rather than keeping a stale projection
    // or silently rendering nothing.
    console.error("session_info_get failed:", err);
    next = { status: "unavailable", reason: "ipc_error", body: null };
  } finally {
    entry.inFlight = false;
  }
  entry.state = next;
  entry.listeners.forEach((notify) => notify());
}

/**
 * Join (or start) the shared poll for `sid`. The poll interval is fixed by the
 * FIRST subscriber: later subscribers with a different `pollMs` share the
 * running timer rather than restarting it.
 */
function subscribeSessionInfo(sid: string, pollMs: number, notify: () => void): () => void {
  const entry = sessionInfoEntry(sid);
  entry.listeners.add(notify);
  if (entry.timer === null) {
    void fetchSessionInfo(sid);
    entry.timer = setInterval(() => void fetchSessionInfo(sid), pollMs);
  }
  return () => {
    entry.listeners.delete(notify);
    if (entry.listeners.size === 0) {
      if (entry.timer !== null) clearInterval(entry.timer);
      entry.timer = null;
      // Only remove the map slot if it is still THIS entry — a fresh
      // subscriber may already have replaced it.
      if (sessionInfoEntries.get(sid) === entry) sessionInfoEntries.delete(sid);
    }
  };
}

/**
 * The state a consumer of `sid` renders: the shared entry's, or LOADING when
 * there is no session id or no entry yet — so a zone switched to another
 * session never shows the previous session's identity. Pure over the store.
 */
export function sessionInfoSnapshot(sid: string | undefined): SessionInfoState {
  return sid ? (sessionInfoEntries.get(sid)?.state ?? LOADING_STATE) : LOADING_STATE;
}

/** Test seam: how many live shared entries exist (one per polled session). */
export function sessionInfoSubscriptionCount(): number {
  return sessionInfoEntries.size;
}

/** Exported for the unit test — subscribe without React. */
export { subscribeSessionInfo };

/**
 * Poll `session_info_get` while mounted. `claudeSessionId` is undefined for
 * plain PTY tabs and not-yet-reconciled spawns — no fetch, and the caller
 * renders no trigger at all (a tab with no Claude session has no session
 * info, which is a different statement from "its session info is unknown").
 *
 * Every mounted consumer of the same session id shares ONE poll — see
 * `SessionInfoEntry`. A zone reassigned to another session reads LOADING until
 * that session's first fetch lands, never the previous session's identity.
 */
export function useSessionInfo(
  claudeSessionId: string | undefined,
  pollMs = 60_000,
): SessionInfoState & { refresh: () => void } {
  const subscribe = useCallback(
    (notify: () => void) =>
      claudeSessionId ? subscribeSessionInfo(claudeSessionId, pollMs, notify) : () => {},
    [claudeSessionId, pollMs],
  );
  const getSnapshot = useCallback(() => sessionInfoSnapshot(claudeSessionId), [claudeSessionId]);
  const state = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  const refresh = useCallback(() => {
    if (claudeSessionId) void fetchSessionInfo(claudeSessionId);
  }, [claudeSessionId]);
  return { ...state, refresh };
}
