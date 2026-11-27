/**
 * The Session Manager query bar's chips, pickers and saved views as editors of
 * the one query string (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * D5 / Phase 4): every chip is a token, clicking it toggles that token, and a
 * chip is lit exactly when its token stands in the query — so what the box
 * shows is the whole filter.
 *
 * Each control's UI Bridge id and declared effect live in
 * {@link SESSION_QUERY_CONTROLS} (served policy `testing`
 * `an-actions-safety-class-is-declared-not-re-derived`). Narrowing the list
 * changes no stored state, so filter controls are `read`; saving or deleting
 * a named view writes the organization store, so those are `write`.
 *
 * Pure: no React, no Tauri.
 */

import type { IREffect } from "@qontinui/ui-bridge";

import type { SessionFacts } from "./sessionFacts";
import type { SavedView, SessionGroup } from "./sessionOrgTypes";
import {
  fieldToken,
  filterSessions,
  hasToken,
  parseQuery,
  setFieldValue,
  toggleToken,
  topLevelFieldValue,
  type QueryContext,
} from "./sessionQuery";

export interface SessionQueryControl {
  id: string;
  effect: IREffect;
}

const control = (name: string, effect: IREffect): SessionQueryControl => ({
  id: `terminal.session-org.${name}`,
  effect,
});

/** Every query-bar control's UI Bridge id and declared effect. */
export const SESSION_QUERY_CONTROLS = {
  root: control("query-bar", "read"),
  input: control("query-input", "read"),
  clear: control("query-clear", "read"),
  errors: control("query-errors", "read"),
  chip: control("query-chip", "read"),
  accountPicker: control("query-account-picker", "read"),
  groupPicker: control("query-group-picker", "read"),
  savedView: control("query-saved-view", "read"),
  saveView: control("query-save-view", "write"),
  saveViewName: control("query-save-view-name", "read"),
  deleteView: control("query-delete-view", "write"),
  unknownTail: control("query-unknown-tail", "read"),
} as const satisfies Record<string, SessionQueryControl>;

export type SessionQueryControlName = keyof typeof SESSION_QUERY_CONTROLS;

/** A per-item control id: the base id plus the item's slug. */
export function sessionQueryControlId(name: SessionQueryControlName, item: string): string {
  return `${SESSION_QUERY_CONTROLS[name].id}-${slugify(item)}`;
}

/** The two attributes every query-bar control renders. */
export function sessionQueryControlAttrs(
  name: SessionQueryControlName,
  item?: string,
): { "data-ui-bridge-id": string; "data-effect": IREffect } {
  return {
    "data-ui-bridge-id":
      item === undefined ? SESSION_QUERY_CONTROLS[name].id : sessionQueryControlId(name, item),
    "data-effect": SESSION_QUERY_CONTROLS[name].effect,
  };
}

/** Lowercase, `[a-z0-9-]` only — stable for ids built from free text. */
export function slugify(s: string): string {
  const slug = s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return slug || "x";
}

// ── Chips ────────────────────────────────────────────────────────────────────

export interface QueryChip {
  /** Stable key — the chip's id suffix. */
  key: string;
  label: string;
  /** The token the chip toggles. */
  token: string;
  /** What the chip selects, for the tooltip. */
  title: string;
}

/** The chip row, in display order: what needs the operator first. */
export const QUERY_CHIPS: readonly QueryChip[] = [
  {
    key: "needs-you",
    label: "Needs you",
    token: "is:needs-you",
    title: "Sessions waiting on your input",
  },
  { key: "working", label: "Working", token: "is:working", title: "Output in the last minute" },
  { key: "idle", label: "Idle", token: "is:idle", title: "Alive and waiting at the prompt" },
  {
    key: "no-output",
    label: "No output",
    token: "is:stalled",
    title: "No output past the threshold with a tool call pending",
  },
  {
    key: "finished",
    label: "Finished",
    token: "is:finished",
    title: "Marked finished (/finish-session)",
  },
  {
    key: "ready-to-close",
    label: "Ready to close",
    token: "is:closable",
    title: "Finished, or exited 0 with every pull request green",
  },
  {
    key: "outside-runner",
    label: "Outside runner",
    token: "where:outside",
    title: "A live Claude process this runner did not start, matched by session id",
  },
  { key: "pinned", label: "Pinned", token: "is:pinned", title: "Pinned sessions" },
];

/** How many rows a chip's token matches, and how many it could not decide. */
export interface ChipCount {
  matched: number;
  unknown: number;
}

/**
 * Each chip's count over `rows`, evaluated as the chip's token alone — so a
 * count says how many sessions the chip WOULD select, independent of the rest
 * of the query. The rows a token cannot decide are counted apart, never
 * folded into either side.
 */
export function chipCounts(
  rows: readonly SessionFacts[],
  ctx: QueryContext,
  chips: readonly QueryChip[] = QUERY_CHIPS,
): Record<string, ChipCount> {
  const out: Record<string, ChipCount> = {};
  for (const chip of chips) {
    const r = filterSessions(chip.token, rows, ctx);
    out[chip.key] = r.ok
      ? { matched: r.matched.length, unknown: r.unknown.length }
      : { matched: 0, unknown: rows.length };
  }
  return out;
}

/** Is the chip lit — does its token stand as a top-level conjunct? */
export function chipActive(query: string, chip: QueryChip): boolean {
  return hasToken(query, chip.token);
}

/** The chip click. */
export function toggleChip(query: string, chip: QueryChip): string {
  return toggleToken(query, chip.token);
}

// ── Pickers ──────────────────────────────────────────────────────────────────

/** The `group:` value that selects ungrouped sessions. */
export const UNGROUPED_VALUE = "none";

/** The account picker's selection, or `null` = any account. */
export function selectedAccount(query: string): string | null {
  return topLevelFieldValue(query, "account");
}

/** Pick an account (`null` = any). */
export function pickAccount(query: string, account: string | null): string {
  return setFieldValue(query, "account", account);
}

/** The group picker's selection — a group title, {@link UNGROUPED_VALUE}, or `null` = any. */
export function selectedGroup(query: string): string | null {
  return topLevelFieldValue(query, "group");
}

/** Pick a group by title, {@link UNGROUPED_VALUE}, or `null` = any. */
export function pickGroup(query: string, group: string | null): string {
  return setFieldValue(query, "group", group);
}

/** The group picker's options, in the store's order. */
export function groupOptions(
  groups: readonly SessionGroup[],
): { value: string; label: string; token: string }[] {
  const sorted = [...groups].sort((a, b) => a.order - b.order);
  return [
    ...sorted.map((g) => ({ value: g.title, label: g.title, token: fieldToken("group", g.title) })),
    { value: UNGROUPED_VALUE, label: "Ungrouped", token: fieldToken("group", UNGROUPED_VALUE) },
  ];
}

// ── Saved views ──────────────────────────────────────────────────────────────

/** Normalize a query for "is this the view on screen" — whitespace only. */
function normalizeQuery(q: string): string {
  return q.trim().replace(/\s+/g, " ");
}

/** A saved view is lit when the box holds exactly its query. */
export function savedViewActive(query: string, view: SavedView): boolean {
  return normalizeQuery(query) === normalizeQuery(view.query);
}

/** Clicking a lit view clears the box; clicking another replaces the query. */
export function applySavedView(query: string, view: SavedView): string {
  return savedViewActive(query, view) ? "" : view.query;
}

/**
 * Why a view cannot be saved as typed, or `null` when it can. A view that
 * does not parse would load as an error, and an empty one selects everything.
 */
export function saveViewRefusal(name: string, query: string): string | null {
  if (name.trim() === "") return "Name the view";
  if (query.trim() === "") return "The query is empty";
  const parsed = parseQuery(query);
  if (!parsed.ok) return "Fix the query before saving it";
  return null;
}
