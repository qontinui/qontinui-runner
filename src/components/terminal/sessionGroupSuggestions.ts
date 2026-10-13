/**
 * Suggested groups — the Session Manager's deterministic answer to Chrome's
 * "Organize tabs" (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * D6 and Phase 6).
 *
 * Sessions carry exact topic keys Chrome tabs do not, so clustering needs no
 * model call: every suggestion is reproducible from its inputs, and the same
 * sessions always produce the same suggestions with the same ids.
 *
 * ## The rule
 *
 * Only UNGROUPED sessions are considered. Each one is assigned to exactly ONE
 * key — its highest-priority one, in tier order:
 *
 *   1. `plan`   — the work-unit / plan slug
 *   2. `pr`     — a pull request (repo-qualified when the repo is known)
 *   3. `branch` — repo plus branch
 *   4. `repo`   — repo alone
 *
 * A session is never re-filed under a weaker key because its strongest one
 * did not reach two members, nor because that suggestion was dismissed: the
 * strongest key is the session's topic, and a weaker grouping would contradict
 * it. Clusters of fewer than 2 members are dropped, and so are dismissed ones.
 *
 * ## Identity, name and color
 *
 * - `id` is derived from the key alone, so a
 *   dismissal recorded in the store (`dismissSuggestion`) is honoured on every
 *   later refresh. It is `<tier>:<value>` — `plan:2026-10-13-x` — the shape
 *   the store's own tests record.
 * - `name` is short: a plan stem loses its `YYYY-MM-DD-` prefix and is cut at
 *   a word boundary, and any name is capped at the store's 120-char title
 *   limit; `keyValue` keeps the full value for a tooltip.
 * - `colorRole` mirrors the store's own `next_color_role` rule
 *   (`session_org_store.rs`), applied as though every suggestion before this
 *   one had been accepted: the next role no existing group uses, else
 *   `ROLES[groupCount % 6]`. The accept ops carry the role explicitly, so what
 *   the preview shows is what the store records.
 *
 * Pure: no React, no I/O.
 */

import {
  SESSION_GROUP_COLOR_ROLES,
  type SessionGroupColorRole,
  type SessionOrg,
  type SessionOrgOp,
} from "./sessionOrgTypes";

/** The four key tiers, highest priority first. */
export const SUGGESTION_KEY_KINDS = ["plan", "pr", "branch", "repo"] as const;

export type SuggestionKeyKind = (typeof SUGGESTION_KEY_KINDS)[number];

/** One session as the suggester sees it. Every key is optional. */
export interface SuggestionSession {
  /** The organization store key (`sessionKeyFor`). */
  sessionKey: string;
  /** What the preview lists the session as. */
  title: string;
  /** Work-unit / plan slug (a plan stem). */
  planSlug?: string | null;
  /** Pull requests the session opened or works on. */
  prNumbers?: readonly number[] | null;
  repo?: string | null;
  branch?: string | null;
}

/** The slice of the org the suggester reads. */
export type SuggestionOrg = Pick<SessionOrg, "groups" | "members" | "dismissedSuggestions">;

export interface GroupSuggestion {
  /** Stable, derived from the key; the value `dismissSuggestion` records. */
  id: string;
  keyKind: SuggestionKeyKind;
  /** The full key value (plan slug, `repo#n`, `repo:branch`, repo). */
  keyValue: string;
  /** The proposed group title. */
  name: string;
  colorRole: SessionGroupColorRole;
  /** Sorted by session key. */
  members: { sessionKey: string; title: string }[];
  /** The one batch that accepts this suggestion. */
  acceptOps: SessionOrgOp[];
}

/** Longest proposed name, in chars, before it is cut at a word boundary. */
export const SUGGESTION_NAME_MAX_CHARS = 32;

const DATE_PREFIX = /^\d{4}-\d{2}-\d{2}-/;

interface SessionKey {
  kind: SuggestionKeyKind;
  value: string;
  name: string;
}

function clean(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

/**
 * A plan stem without its date prefix, cut at the last `-` that fits so a
 * word is never split; an unbreakable stem is cut hard. Either cut ends in `…`.
 */
export function shortenPlanStem(stem: string, maxChars = SUGGESTION_NAME_MAX_CHARS): string {
  const bare = stem.replace(DATE_PREFIX, "") || stem;
  if (bare.length <= maxChars) return bare;
  const room = bare.slice(0, maxChars - 1);
  const cut = room.lastIndexOf("-");
  return `${cut > 0 ? room.slice(0, cut) : room}…`;
}

function validPrs(prs: readonly number[] | null | undefined): number[] {
  return [...new Set((prs ?? []).filter((n) => Number.isInteger(n) && n > 0))].sort(
    (a, b) => a - b,
  );
}

function prValue(repo: string | null, pr: number): string {
  return repo ? `${repo}#${pr}` : `#${pr}`;
}

/**
 * A session's single strongest key, or null when it has none. Among several
 * PRs the one shared with the most other candidates wins (ties: lowest
 * number), so two sessions on the same PR land together even when one of them
 * also touched an older PR.
 */
function strongestKey(
  session: SuggestionSession,
  prCounts: ReadonlyMap<string, number>,
): SessionKey | null {
  const plan = clean(session.planSlug);
  if (plan) return { kind: "plan", value: plan, name: shortenPlanStem(plan) };

  const repo = clean(session.repo);
  const prs = validPrs(session.prNumbers);
  if (prs.length > 0) {
    let best = prs[0];
    for (const pr of prs) {
      const count = prCounts.get(prValue(repo, pr)) ?? 0;
      if (count > (prCounts.get(prValue(repo, best)) ?? 0)) best = pr;
    }
    return { kind: "pr", value: prValue(repo, best), name: `PR #${best}` };
  }

  const branch = clean(session.branch);
  if (repo && branch) {
    const value = `${repo}:${branch}`;
    return { kind: "branch", value, name: value };
  }
  if (repo) return { kind: "repo", value: repo, name: repo };
  return null;
}

/** The store's `next_color_role`, over a running list of used roles. */
function nextColorRole(used: readonly SessionGroupColorRole[]): SessionGroupColorRole {
  return (
    SESSION_GROUP_COLOR_ROLES.find((r) => !used.includes(r)) ??
    SESSION_GROUP_COLOR_ROLES[used.length % SESSION_GROUP_COLOR_ROLES.length]
  );
}

export function suggestionId(kind: SuggestionKeyKind, value: string): string {
  return `${kind}:${value}`;
}

/** The store refuses a group title longer than this (`MAX_LABEL_CHARS`). */
const GROUP_TITLE_MAX_CHARS = 120;

function capTitle(name: string): string {
  return name.length <= GROUP_TITLE_MAX_CHARS
    ? name
    : `${name.slice(0, GROUP_TITLE_MAX_CHARS - 1)}…`;
}

/**
 * Suggested groups for the ungrouped sessions in `sessions`, highest tier
 * first, then by key value. A session already in a group, or appearing twice,
 * contributes once or not at all.
 */
export function suggestGroups(
  sessions: readonly SuggestionSession[],
  org: SuggestionOrg,
): GroupSuggestion[] {
  const seen = new Set<string>();
  const candidates: SuggestionSession[] = [];
  for (const s of sessions) {
    const key = s.sessionKey.trim();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    if (org.members[key]?.groupId != null) continue;
    candidates.push(s);
  }

  const prCounts = new Map<string, number>();
  for (const s of candidates) {
    if (clean(s.planSlug)) continue;
    const repo = clean(s.repo);
    for (const pr of validPrs(s.prNumbers)) {
      const value = prValue(repo, pr);
      prCounts.set(value, (prCounts.get(value) ?? 0) + 1);
    }
  }

  const clusters = new Map<string, { key: SessionKey; members: SuggestionSession[] }>();
  for (const s of candidates) {
    const key = strongestKey(s, prCounts);
    if (!key) continue;
    const id = suggestionId(key.kind, key.value);
    const cluster = clusters.get(id);
    if (cluster) cluster.members.push(s);
    else clusters.set(id, { key, members: [s] });
  }

  const dismissed = new Set(org.dismissedSuggestions);
  const kept = [...clusters.entries()]
    .filter(([id, c]) => c.members.length >= 2 && !dismissed.has(id))
    .sort(([, a], [, b]) => {
      const tier =
        SUGGESTION_KEY_KINDS.indexOf(a.key.kind) - SUGGESTION_KEY_KINDS.indexOf(b.key.kind);
      return tier !== 0 ? tier : compareStrings(a.key.value, b.key.value);
    });

  const usedRoles = org.groups.map((g) => g.colorRole);
  return kept.map(([id, { key, members }]) => {
    const colorRole = nextColorRole(usedRoles);
    usedRoles.push(colorRole);
    const sorted = [...members]
      .sort((a, b) => compareStrings(a.sessionKey.trim(), b.sessionKey.trim()))
      .map((m) => ({ sessionKey: m.sessionKey.trim(), title: m.title }));
    return {
      id,
      keyKind: key.kind,
      keyValue: key.value,
      name: capTitle(key.name),
      colorRole,
      members: sorted,
      acceptOps: [
        {
          op: "createGroup",
          title: capTitle(key.name),
          colorRole,
          sessionKeys: sorted.map((m) => m.sessionKey),
        },
      ],
    };
  });
}

/** The one batch that accepts every suggestion in `suggestions`. */
export function acceptAllOps(suggestions: readonly GroupSuggestion[]): SessionOrgOp[] {
  return suggestions.flatMap((s) => s.acceptOps);
}

/** The op that remembers a dismissal. */
export function dismissOp(suggestion: GroupSuggestion): SessionOrgOp {
  return { op: "dismissSuggestion", key: suggestion.id };
}

/** Code-unit order, so the result never depends on the host locale. */
function compareStrings(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}
