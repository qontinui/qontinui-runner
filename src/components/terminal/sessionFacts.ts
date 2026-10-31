/**
 * `SessionFacts` — everything the Session Manager query engine
 * (`sessionQuery.ts`) can ask about one session, each fact allowed to be
 * UNKNOWN (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4).
 *
 * Built by {@link buildSessionFacts} from the sources the panel already holds:
 *
 *  - the row (`UnifiedSession`) with its Phase 1 two-axis status;
 *  - the Phase 2 organization store (group, tags, pin, mute, snooze, last
 *    seen) — UNKNOWN for every org fact while the store is loading or
 *    unreadable, never "ungrouped" / "not pinned";
 *  - the batched `session_info_list` listing (`useSessionFacts`): repo via the
 *    working dir, branch, the PR ledger, account, the finished marker;
 *  - coord's PR CI state (`coord_pr_ci_states`) for `is:closable`;
 *  - the Fleet row (device, coord `session_status`, work-unit slug), when the
 *    session has one.
 *
 * A fact no source covers is `{ known: false, reason }` — the query engine
 * then evaluates any predicate over it to UNKNOWN and the row lands in the
 * "Unknown for this filter (N)" tail rather than being silently included or
 * excluded (served policy `verification-and-evidence`
 * `unknown-must-not-render-as-a-default`).
 *
 * Pure: no React, no Tauri.
 */

import type { SessionActivity, SessionLocation, SessionStatus } from "./sessionStatusModel";
import type { SessionGroupColorRole, SessionOrg } from "./sessionOrgTypes";
import { unseenVerdict, type UnseenVerdict } from "./sessionUnseen";
import type { SessionInfoState } from "./useSessionInfo";

// ── Facts ────────────────────────────────────────────────────────────────────

/** A value, or the statement that no source established it. */
export type Fact<T> = { known: true; value: T } | { known: false; reason: string };

export function known<T>(value: T): Fact<T> {
  return { known: true, value };
}

export function unknownFact<T>(reason: string): Fact<T> {
  return { known: false, reason };
}

/** CI state of one PR — mirrors Rust `PrCiRead.state` (`pr_ci_state.rs`). */
export type PrCiState = "green" | "pending" | "red" | "landed" | "closed" | "unknown";

/** One PR the session authored, with what is known of its CI. */
export interface PrFact {
  repo: string;
  number: number;
  ci: PrCiState;
  /** coord `rebase_block_disposition == author_acts`; `null` = not served. */
  authorActs: boolean | null;
  /** Why `ci` is `unknown`, when it is. */
  reason: string | null;
}

export interface SessionGroupFact {
  id: string;
  title: string;
  colorRole: SessionGroupColorRole;
}

export interface SessionPageFact {
  id: string;
  name: string;
}

export interface SessionFacts {
  /** The panel row's id (`UnifiedSession.sessionId`). */
  rowId: string;
  /** The organization-store key (`sessionKeyFor`). */
  sessionKey: string;
  /** The Claude session id, when the row has one — the `text:` join key. */
  claudeSessionId: string | null;

  /** The headline name (`title:` / `name:` match only this). */
  title: string;
  /** What the session is about (`workSummaryHint`, else the resume name). */
  topic: string;
  /** First and last message previews. */
  preview: string;
  /** The other fields bare words have always matched (names, project, id). */
  extraText: readonly string[];

  location: Fact<SessionLocation>;
  activity: Fact<SessionActivity>;
  /** Exit code for an exited row; `null` when not exited or not known. */
  exitCode: number | null;
  /** The work-axis finished marker. */
  finished: Fact<boolean>;

  pinned: Fact<boolean>;
  muted: Fact<boolean>;
  /** Epoch ms; `null` = not snoozed. */
  snoozedUntil: Fact<number | null>;
  /** Epoch ms of the last focus; `null` = never focused. */
  lastSeenAt: Fact<number | null>;
  /**
   * The unseen-activity dot (Phase 7): activity changed or output arrived
   * after the last focus. The SAME verdict the card's dot renders
   * (`useSessionUnseen`), so `is:unseen` filters exactly the dotted rows.
   */
  unseen: Fact<boolean>;
  /** `null` = ungrouped. */
  group: Fact<SessionGroupFact | null>;
  tags: Fact<readonly string[]>;

  account: Fact<string | null>;
  repo: Fact<string | null>;
  /** `null` = detached HEAD. */
  branch: Fact<string | null>;
  prs: Fact<readonly PrFact[]>;
  /** Work-unit / plan slug. */
  workUnit: Fact<string | null>;
  device: Fact<string | null>;
  /** The Terminal page holding the session's tab; `null` = no tab here. */
  page: Fact<SessionPageFact | null>;

  /** Epoch ms. */
  startedAt: Fact<number>;
  /** Epoch ms of the last output / transcript record. */
  lastActivityAt: Fact<number>;
  messageCount: Fact<number>;
}

// ── Sources ──────────────────────────────────────────────────────────────────

/** The row fields facts are built from — `UnifiedSession` satisfies it. */
export interface SessionFactsRow {
  sessionId: string;
  source: "transcript" | "tab";
  displayName: string;
  resumeName: string | null;
  registryName: string | null;
  firstMessagePreview: string | null;
  lastMessagePreview: string;
  workSummaryHint: string;
  projectPath: string;
  projectLabel: string;
  accountLabel: string;
  messageCount: number;
  startedAt: string | null;
  lastModified: string;
  lastMessageTimestamp: string;
  status: SessionStatus;
  remoteDeviceLabel: string | null;
}

/** One working dir's branch — mirrors Rust `WorkingDirBranch`. */
export interface WorkingDirBranch {
  workingDir: string;
  status: "ok" | "detached" | "unknown";
  branch: string | null;
  reason: string | null;
}

/** One PR's CI read — mirrors Rust `PrCiRead`. */
export interface PrCiRead {
  repo: string;
  number: number;
  state: PrCiState;
  authorActs: boolean | null;
  reason: string | null;
  confidence: string | null;
  observedAt: number;
}

/**
 * The `session_info_list` listing as `useSessionFacts` holds it. `byId` holds
 * each listed session's normalized envelope; `notFound` the ids the
 * lifecycle store has no record of.
 */
export type SessionInfoListView =
  | { phase: "loading" }
  | { phase: "unavailable"; reason: string }
  | {
      phase: "ok";
      byId: ReadonlyMap<string, SessionInfoState>;
      notFound: ReadonlySet<string>;
      branches: ReadonlyMap<string, WorkingDirBranch>;
    };

/** The org store as the builder needs it — `SessionOrgView` satisfies it. */
export type SessionOrgSource =
  | { phase: "loading" }
  | { phase: "unknown"; reason: string }
  | { phase: "known"; org: SessionOrg };

/** A coord Fleet row for this session. */
export interface SessionFleetSource {
  deviceLabel: string | null;
  sessionStatus: string | null;
  workUnitSlug: string | null;
  repo: string | null;
  branch: string | null;
}

export interface SessionFactsSources {
  row: SessionFactsRow;
  sessionKey: string;
  claudeSessionId: string | null;
  org: SessionOrgSource;
  info: SessionInfoListView;
  /** CI reads keyed by {@link prCiKey}; `null` = not fetched yet. */
  prCi: ReadonlyMap<string, PrCiRead> | null;
  fleet?: SessionFleetSource | null;
  /** This session's tab page here; `null` = no tab here. */
  page: SessionPageFact | null;
  /** The label of THIS device, for local rows' `device:`; absent = unknown. */
  thisDeviceLabel?: string | null;
  /** Epoch ms of the tab's last PTY output, when a tab exists. */
  lastOutputAt?: number | null;
  /**
   * The unseen verdict the panel's dot renders (`useSessionUnseen().verdictOf`).
   * Absent = derived here from the store's `lastSeenAt` and the row's last
   * activity alone, with no in-process observation (a never-focused session
   * is then UNKNOWN, not unseen).
   */
  unseen?: UnseenVerdict;
}

/**
 * Epoch ms of a row's last activity: its last transcript record, its file's
 * modification, the tab's last output — the newest; `null` = none known.
 * The facts' `lastActivityAt` and the unseen dot both read it.
 */
export function rowLastActivityAt(
  row: Pick<SessionFactsRow, "lastMessageTimestamp" | "lastModified">,
  lastOutputAt?: number | null,
): number | null {
  const ms = Math.max(
    parseMs(row.lastMessageTimestamp) ?? -Infinity,
    parseMs(row.lastModified) ?? -Infinity,
    lastOutputAt ?? -Infinity,
  );
  return Number.isFinite(ms) ? ms : null;
}

/** An unseen verdict as a fact: `unknown` stays UNKNOWN with its reason. */
export function unseenFact(verdict: UnseenVerdict): Fact<boolean> {
  return verdict.state === "unknown"
    ? unknownFact(verdict.reason)
    : known(verdict.state === "unseen");
}

/** The key PR CI reads are indexed by. */
export function prCiKey(repo: string, number: number): string {
  return `${repo}#${number}`;
}

// ── Builder ──────────────────────────────────────────────────────────────────

function parseMs(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const ms = new Date(iso).getTime();
  return Number.isFinite(ms) ? ms : null;
}

/** Last path segment of a working dir — the repo a worktree checks out. */
export function repoOfPath(path: string | null | undefined): string | null {
  if (!path) return null;
  const parts = path.split(/[\\/]+/).filter((p) => p.length > 0);
  return parts.length > 0 ? parts[parts.length - 1] : null;
}

function nonEmpty(s: string | null | undefined): string | null {
  const t = s?.trim();
  return t ? t : null;
}

type InfoLookup = { kind: "ok"; state: SessionInfoState } | { kind: "none"; reason: string };

function lookupInfo(info: SessionInfoListView, id: string | null): InfoLookup {
  if (id === null) return { kind: "none", reason: "no Claude session id" };
  if (info.phase === "loading") return { kind: "none", reason: "session info not read yet" };
  if (info.phase === "unavailable")
    return { kind: "none", reason: `session info unavailable: ${info.reason}` };
  const state = info.byId.get(id);
  if (state) return { kind: "ok", state };
  return {
    kind: "none",
    reason: info.notFound.has(id) ? "no lifecycle record" : "not in the session info listing",
  };
}

function orgFacts(
  org: SessionOrgSource,
  key: string,
): Pick<SessionFacts, "pinned" | "muted" | "snoozedUntil" | "lastSeenAt" | "group" | "tags"> {
  if (org.phase !== "known") {
    const reason =
      org.phase === "loading"
        ? "organization store not read yet"
        : `organization store unknown: ${org.reason}`;
    return {
      pinned: unknownFact(reason),
      muted: unknownFact(reason),
      snoozedUntil: unknownFact(reason),
      lastSeenAt: unknownFact(reason),
      group: unknownFact(reason),
      tags: unknownFact(reason),
    };
  }
  const member = org.org.members[key];
  const group = member?.groupId ? org.org.groups.find((g) => g.id === member.groupId) : undefined;
  return {
    pinned: known(member?.pinned ?? false),
    muted: known(member?.muted ?? false),
    snoozedUntil: known(member?.snoozedUntil ?? null),
    lastSeenAt: known(member?.lastSeenAt ?? null),
    group: known(group ? { id: group.id, title: group.title, colorRole: group.colorRole } : null),
    tags: known(member?.tags ?? []),
  };
}

function prFacts(
  state: SessionInfoState,
  prCi: ReadonlyMap<string, PrCiRead> | null,
): Fact<readonly PrFact[]> {
  const prs = state.body?.prs;
  if (!prs) return unknownFact("no PR ledger");
  if (prs.status !== "ok")
    return unknownFact(`PR ledger unavailable: ${prs.reason ?? "unspecified"}`);
  const byKey = new Map<string, PrFact>();
  const ci = (repo: string, number: number): Pick<PrFact, "ci" | "authorActs" | "reason"> => {
    if (prCi === null) return { ci: "unknown", authorActs: null, reason: "PR CI not read yet" };
    const read = prCi.get(prCiKey(repo, number));
    if (!read) return { ci: "unknown", authorActs: null, reason: "PR CI not read" };
    return { ci: read.state, authorActs: read.authorActs, reason: read.reason };
  };
  for (const pr of prs.opened) {
    const ledger: PrCiState | null =
      pr.prState === "merged" ? "landed" : pr.prState === "closed" ? "closed" : null;
    const fromCoord = ci(pr.repo, pr.prNumber);
    byKey.set(prCiKey(pr.repo, pr.prNumber), {
      repo: pr.repo,
      number: pr.prNumber,
      ...fromCoord,
      // The ledger's terminal states stand; coord is asked only about open PRs.
      ci: ledger ?? fromCoord.ci,
      reason: ledger ? null : fromCoord.reason,
    });
  }
  for (const pr of prs.landed) {
    const k = prCiKey(pr.repo, pr.prNumber);
    const prev = byKey.get(k);
    byKey.set(k, {
      repo: pr.repo,
      number: pr.prNumber,
      ci: "landed",
      authorActs: prev?.authorActs ?? null,
      reason: null,
    });
  }
  for (const pr of prs.unknown) {
    const k = prCiKey(pr.repo, pr.prNumber);
    if (!byKey.has(k))
      byKey.set(k, { repo: pr.repo, number: pr.prNumber, ...ci(pr.repo, pr.prNumber) });
  }
  const list = [...byKey.values()];
  // An empty ledger asserts nothing until the reconciler has looked.
  if (list.length === 0 && !prs.scanned) return unknownFact("PR scan never ran for this session");
  return known(list);
}

/**
 * The work-axis finished marker of one live session, from its lifecycle
 * record in the `session_info_list` listing: `true` / `false` when the record
 * carries `finishedAt`, `null` (UNKNOWN) otherwise — no Claude session id, the
 * listing loading or unavailable, no lifecycle record, an unavailable
 * envelope, or a runner build whose record omits the field. A missing source
 * is never "not finished". Pure.
 */
export function finishedMarker(info: SessionInfoListView, id: string | null): boolean | null {
  if (id === null || info.phase !== "ok") return null;
  const finishedAt = info.byId.get(id)?.body?.lifecycle.finishedAt;
  if (typeof finishedAt === "number") return true;
  return finishedAt === null ? false : null;
}

/** Build one session's facts. Pure. */
export function buildSessionFacts(src: SessionFactsSources): SessionFacts {
  const { row, info, fleet } = src;
  const status = row.status;
  const infoHit = lookupInfo(info, src.claudeSessionId);
  const body = infoHit.kind === "ok" ? infoHit.state.body : null;
  const infoReason =
    infoHit.kind === "none"
      ? infoHit.reason
      : `session info unavailable: ${infoHit.state.reason ?? "unspecified"}`;

  const title = row.registryName ?? row.resumeName ?? row.displayName;
  const topic = nonEmpty(row.workSummaryHint) ?? row.resumeName ?? "";
  const preview = [row.firstMessagePreview ?? "", row.lastMessagePreview].filter(Boolean).join(" ");

  const location: Fact<SessionLocation> =
    status.location === "unknown" ? unknownFact(status.reason) : known(status.location);
  const activity: Fact<SessionActivity> =
    status.activity === "unknown" ? unknownFact(status.reason) : known(status.activity);

  const marker = finishedMarker(info, src.claudeSessionId);
  let finished: Fact<boolean>;
  if (marker !== null) finished = known(marker);
  else if (fleet?.sessionStatus) finished = known(fleet.sessionStatus === "finished");
  else if (status.activity === "finished") finished = known(true);
  else
    finished = unknownFact(body ? "the lifecycle record carries no finished marker" : infoReason);

  const workingDir = body?.placement.workingDir ?? null;
  const repoName =
    nonEmpty(fleet?.repo) ?? repoOfPath(workingDir) ?? repoOfPath(nonEmpty(row.projectPath));
  const repo: Fact<string | null> = repoName
    ? known(repoName)
    : unknownFact("no working dir or project path");

  let branch: Fact<string | null>;
  if (nonEmpty(fleet?.branch)) branch = known(nonEmpty(fleet?.branch));
  else if (!workingDir) branch = unknownFact(body ? "no working dir" : infoReason);
  else if (info.phase !== "ok") branch = unknownFact(infoReason);
  else {
    const b = info.branches.get(workingDir);
    if (!b) branch = unknownFact("working dir not probed");
    else if (b.status === "ok") branch = known(b.branch);
    else if (b.status === "detached") branch = known(null);
    else branch = unknownFact(`branch unknown: ${b.reason ?? "unspecified"}`);
  }

  const prs =
    infoHit.kind === "ok" && body
      ? prFacts(infoHit.state, src.prCi)
      : unknownFact<readonly PrFact[]>(infoReason);

  const accountLabel = nonEmpty(body?.account.label) ?? nonEmpty(row.accountLabel);
  const account: Fact<string | null> = accountLabel
    ? known(accountLabel)
    : unknownFact("no account label");

  const workUnit: Fact<string | null> = fleet
    ? known(nonEmpty(fleet.workUnitSlug))
    : unknownFact("no fleet row carries a work unit for this session");

  let device: Fact<string | null>;
  if (fleet) device = known(nonEmpty(fleet.deviceLabel));
  else if (row.remoteDeviceLabel) device = known(row.remoteDeviceLabel);
  else if (src.thisDeviceLabel !== undefined && src.thisDeviceLabel !== null)
    device = known(src.thisDeviceLabel);
  else device = unknownFact("this device's label not known");

  const started = parseMs(row.startedAt) ?? (body ? body.lifecycle.openedAt : null);
  const lastActivity = rowLastActivityAt(row, src.lastOutputAt);
  const org = orgFacts(src.org, src.sessionKey);
  const unseen = unseenFact(
    src.unseen ??
      unseenVerdict({
        lastSeenAt: org.lastSeenAt,
        lastActivityAt: lastActivity,
        observation: undefined,
        focused: false,
      }),
  );

  return {
    rowId: row.sessionId,
    sessionKey: src.sessionKey,
    claudeSessionId: src.claudeSessionId,
    title,
    topic,
    preview,
    extraText: [
      row.displayName,
      row.registryName ?? "",
      row.resumeName ?? "",
      row.projectLabel,
      row.sessionId,
      row.workSummaryHint,
    ].filter((s) => s.length > 0),
    location,
    activity,
    exitCode: status.exitCode,
    finished,
    ...org,
    unseen,
    account,
    repo,
    branch,
    prs,
    workUnit,
    device,
    page: known(src.page),
    startedAt: started !== null ? known(started) : unknownFact("start time not recorded"),
    lastActivityAt:
      lastActivity !== null ? known(lastActivity) : unknownFact("no activity timestamp"),
    messageCount:
      row.source === "transcript" ? known(row.messageCount) : unknownFact("no transcript yet"),
  };
}

/**
 * The open PRs coord must be asked about — those the runner's own ledger has
 * not already settled as merged / closed / landed. Feed to
 * `coord_pr_ci_states`.
 */
export function prsNeedingCi(info: SessionInfoListView): { repo: string; number: number }[] {
  if (info.phase !== "ok") return [];
  const out = new Map<string, { repo: string; number: number }>();
  for (const state of info.byId.values()) {
    const prs = state.body?.prs;
    if (!prs || prs.status !== "ok") continue;
    const landed = new Set(prs.landed.map((p) => prCiKey(p.repo, p.prNumber)));
    for (const pr of [...prs.opened, ...prs.unknown]) {
      const k = prCiKey(pr.repo, pr.prNumber);
      const settled =
        landed.has(k) || ("prState" in pr && (pr.prState === "merged" || pr.prState === "closed"));
      if (!settled) out.set(k, { repo: pr.repo, number: pr.prNumber });
    }
  }
  return [...out.values()];
}
