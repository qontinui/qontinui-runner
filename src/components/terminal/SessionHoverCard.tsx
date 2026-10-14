/**
 * The Session Manager's hover card — Chrome's tab hover card, for a session
 * (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 7). Shown on hovering a Live row, and as the body of a rail-mode
 * item, it answers "what is this and why is it in that state" without opening
 * anything: title, topic, repo / branch, PRs (opened / landed / unknown),
 * location word plus page, activity word plus the source that decided it and
 * since when, account, last activity and group.
 *
 * Every field no source established renders the word `Unknown`, with the
 * reason as its tooltip — never a default (served policy
 * `verification-and-evidence` `unknown-must-not-render-as-a-default`).
 *
 * The card is placed beside the row it describes and NEVER over it
 * ({@link placeHoverCard}; served policy `ux-priorities`
 * `no-widget-may-hide-identifying-text`): the operator is pointing at that
 * row, so covering it would hide the very name he is reading.
 *
 * One card is mounted per panel, for the hovered row only — never one per row.
 */

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { sessionCardName } from "./SessionCard";
import type { Fact, PrFact, SessionFacts } from "./sessionFacts";
import type { SessionOrgControl } from "./sessionOrgControls";
import {
  activityText,
  formatSilence,
  locationText,
  type SessionStatus,
} from "./sessionStatusModel";
import { SESSION_TOPIC_SOURCE_LABELS, sessionTopic } from "./sessionTopic";
import type { UnifiedSession } from "./useSessionManager";

// ── Placement (pure) ─────────────────────────────────────────────────────────

export interface Rect {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

export interface Size {
  width: number;
  height: number;
}

export interface HoverCardPlacement {
  side: "right" | "left" | "below" | "above";
  left: number;
  top: number;
  width: number;
  /** The card scrolls past this. */
  maxHeight: number;
}

export const HOVER_CARD_WIDTH = 300;
/** The first-frame height guess, before the card has been measured. */
export const HOVER_CARD_ESTIMATED_HEIGHT = 260;
/** Space between the row and the card. */
export const HOVER_CARD_GAP = 6;
/** Space kept between the card and the viewport edge. */
export const HOVER_CARD_MARGIN = 8;

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(v, hi));
}

/**
 * Where the card goes: to the right of the row (the panel is the leftmost
 * column), else to its left, else below, else above — the first side the
 * card fits on whole. With no side that fits, the side with the most room,
 * the card's height capped to it. On every side the card's rectangle lies
 * wholly outside the row's, so it never covers the row.
 */
export function placeHoverCard(anchor: Rect, card: Size, viewport: Size): HoverCardPlacement {
  const m = HOVER_CARD_MARGIN;
  const g = HOVER_CARD_GAP;
  const fullHeight = Math.max(0, viewport.height - 2 * m);
  const width = Math.min(card.width, Math.max(0, viewport.width - 2 * m));
  const sideTop = clamp(anchor.top, m, Math.max(m, viewport.height - m - card.height));
  const sideMaxHeight = Math.min(card.height, fullHeight);

  if (anchor.right + g + width <= viewport.width - m) {
    return { side: "right", left: anchor.right + g, top: sideTop, width, maxHeight: sideMaxHeight };
  }
  if (anchor.left - g - width >= m) {
    return {
      side: "left",
      left: anchor.left - g - width,
      top: sideTop,
      width,
      maxHeight: sideMaxHeight,
    };
  }
  const left = clamp(anchor.left, m, Math.max(m, viewport.width - m - width));
  const below = Math.max(0, viewport.height - m - (anchor.bottom + g));
  const above = Math.max(0, anchor.top - g - m);
  const placeBelow = below >= card.height || (above < card.height && below >= above);
  if (placeBelow) {
    return {
      side: "below",
      left,
      top: anchor.bottom + g,
      width,
      maxHeight: Math.min(card.height, below),
    };
  }
  const height = Math.min(card.height, above);
  return { side: "above", left, top: anchor.top - g - height, width, maxHeight: height };
}

// ── Model (pure) ─────────────────────────────────────────────────────────────

/** One labelled field. `unknown` fields render the word `Unknown`. */
export interface HoverCardField {
  key:
    | "topic"
    | "repo"
    | "branch"
    | "prs"
    | "location"
    | "activity"
    | "account"
    | "last-activity"
    | "group";
  label: string;
  value: string;
  /** A second, smaller line (the activity's reason and since; the PR list). */
  detail: string | null;
  /** The field's tooltip — the full value, or why it is unknown. */
  title: string;
  unknown: boolean;
}

export interface SessionHoverCardModel {
  sessionId: string;
  title: string;
  fields: HoverCardField[];
}

/** The row fields the card reads — `UnifiedSession` satisfies it. */
export type HoverCardSession = Pick<
  UnifiedSession,
  | "sessionId"
  | "registryName"
  | "resumeName"
  | "displayName"
  | "workSummaryHint"
  | "firstMessagePreview"
  | "accountLabel"
  | "lastModified"
  | "lastMessageTimestamp"
> & { status: SessionStatus };

const UNKNOWN = "Unknown";
const NO_FACTS = "session facts not loaded yet";

function field(
  key: HoverCardField["key"],
  label: string,
  value: string,
  opts: { detail?: string | null; title?: string } = {},
): HoverCardField {
  return {
    key,
    label,
    value,
    detail: opts.detail ?? null,
    title: opts.title ?? value,
    unknown: false,
  };
}

function unknownField(key: HoverCardField["key"], label: string, reason: string): HoverCardField {
  return { key, label, value: UNKNOWN, detail: null, title: reason, unknown: true };
}

function fromFact<T>(
  key: HoverCardField["key"],
  label: string,
  fact: Fact<T> | null,
  render: (v: T) => string,
): HoverCardField {
  if (fact === null) return unknownField(key, label, NO_FACTS);
  if (!fact.known) return unknownField(key, label, fact.reason);
  return field(key, label, render(fact.value));
}

/** "3m ago" / "2h ago" / "just now". */
export function formatAgo(ms: number, now: number): string {
  const d = now - ms;
  if (d < 60_000) return "just now";
  return `${formatSilence(d)} ago`;
}

function formatClock(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

function parseMs(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const ms = new Date(iso).getTime();
  return Number.isFinite(ms) ? ms : null;
}

const PR_CI_WORDS: Readonly<Record<PrFact["ci"], string>> = {
  green: "green",
  pending: "CI pending",
  red: "CI red",
  landed: "landed",
  closed: "closed",
  unknown: "CI unknown",
};

/** "3 opened · 1 landed · 1 unknown", with each PR listed in the detail. */
function prsField(facts: SessionFacts | null): HoverCardField {
  const label = "PRs";
  if (facts === null) return unknownField("prs", label, NO_FACTS);
  if (!facts.prs.known) return unknownField("prs", label, facts.prs.reason);
  const prs = facts.prs.value;
  if (prs.length === 0) return field("prs", label, "none opened");
  const landed = prs.filter((p) => p.ci === "landed").length;
  const unknown = prs.filter((p) => p.ci === "unknown").length;
  const list = prs.map(
    (p) =>
      `${p.repo}#${p.number} ${PR_CI_WORDS[p.ci]}${p.ci === "unknown" && p.reason ? ` (${p.reason})` : ""}`,
  );
  return field("prs", label, `${prs.length} opened · ${landed} landed · ${unknown} unknown`, {
    detail: list.join(", "),
    title: list.join("\n"),
  });
}

function locationField(status: SessionStatus, facts: SessionFacts | null): HoverCardField {
  const label = "Location";
  if (status.location === "unknown") return unknownField("location", label, status.reason);
  const word = locationText(status);
  // A page only means something for a session with a tab in this window.
  if (status.location !== "on-screen" && status.location !== "hidden-here") {
    return field("location", label, word);
  }
  if (facts === null || !facts.page.known) {
    const reason = facts === null ? NO_FACTS : facts.page.known ? "" : facts.page.reason;
    return field("location", label, `${word} · page ${UNKNOWN}`, {
      title: `${word}; page unknown: ${reason}`,
    });
  }
  const page = facts.page.value;
  return page
    ? field("location", label, `${word} · page "${page.name}"`)
    : field("location", label, word, { title: `${word}; no tab in this window` });
}

function activityField(status: SessionStatus, now: number): HoverCardField {
  const label = "Activity";
  if (status.activity === "unknown") return unknownField("activity", label, status.reason);
  const since =
    status.since !== null
      ? `since ${formatClock(status.since)} (${formatAgo(status.since, now)})`
      : `since ${UNKNOWN}`;
  const detail = `${status.reason} · ${since}`;
  return field("activity", label, activityText(status, now), {
    detail,
    title: status.since === null ? `${detail}\nno source states when this began` : detail,
  });
}

function lastActivityField(
  session: HoverCardSession,
  facts: SessionFacts | null,
  now: number,
): HoverCardField {
  const label = "Last activity";
  let at: number | null = null;
  if (facts?.lastActivityAt.known) at = facts.lastActivityAt.value;
  else {
    const fromRow = Math.max(
      parseMs(session.lastMessageTimestamp) ?? -Infinity,
      parseMs(session.lastModified) ?? -Infinity,
    );
    if (Number.isFinite(fromRow)) at = fromRow;
  }
  if (at === null) {
    const reason =
      facts && !facts.lastActivityAt.known ? facts.lastActivityAt.reason : "no activity timestamp";
    return unknownField("last-activity", label, reason);
  }
  return field("last-activity", label, formatAgo(at, now), {
    title: new Date(at).toLocaleString(),
  });
}

function accountField(session: HoverCardSession, facts: SessionFacts | null): HoverCardField {
  const label = "Account";
  if (facts?.account.known && facts.account.value)
    return field("account", label, facts.account.value);
  const fromRow = session.accountLabel.trim();
  if (fromRow) return field("account", label, fromRow);
  return unknownField(
    "account",
    label,
    facts && !facts.account.known ? facts.account.reason : "no account label",
  );
}

/** Build the card's content. Pure. */
export function buildHoverCardModel(
  session: HoverCardSession,
  facts: SessionFacts | null,
  now: number,
): SessionHoverCardModel {
  const title = sessionCardName(session);
  const topic = sessionTopic(session, title);
  return {
    sessionId: session.sessionId,
    title,
    fields: [
      topic
        ? field("topic", "Topic", topic.text, {
            title: `${topic.text}\n(from the ${SESSION_TOPIC_SOURCE_LABELS[topic.source]})`,
          })
        : unknownField("topic", "Topic", "no work summary, resume name or first prompt"),
      fromFact("repo", "Repo", facts?.repo ?? null, (r) => r ?? "none"),
      fromFact("branch", "Branch", facts?.branch ?? null, (b) => b ?? "detached HEAD"),
      prsField(facts),
      locationField(session.status, facts),
      activityField(session.status, now),
      accountField(session, facts),
      lastActivityField(session, facts, now),
      fromFact("group", "Group", facts?.group ?? null, (g) => (g ? g.title : "Ungrouped")),
    ],
  };
}

// ── Component ────────────────────────────────────────────────────────────────

/** The card's UI Bridge id and effect; one card per panel, so no per-row suffix. */
export const SESSION_HOVER_CARD: SessionOrgControl = {
  id: "terminal.session-org.hover-card",
  effect: "read",
};
export const SESSION_HOVER_CARD_ID = SESSION_HOVER_CARD.id;

function viewportSize(): Size {
  if (typeof window === "undefined") return { width: 1280, height: 800 };
  return { width: window.innerWidth, height: window.innerHeight };
}

export interface SessionHoverCardProps {
  model: SessionHoverCardModel;
  /** The hovered row's rectangle, in viewport coordinates. */
  anchor: Rect;
  /** Keeps the card open while the pointer is on it. */
  onPointerEnter?: () => void;
  onPointerLeave?: () => void;
}

export function SessionHoverCard({
  model,
  anchor,
  onPointerEnter,
  onPointerLeave,
}: SessionHoverCardProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState(HOVER_CARD_ESTIMATED_HEIGHT);
  // Measure the rendered height, then re-place: the first frame uses the
  // estimate, the next the real size, so a tall card still never covers the row.
  useLayoutEffect(() => {
    const h = ref.current?.scrollHeight;
    if (h && h !== height) setHeight(h);
  }, [model, height]);
  const placement = placeHoverCard(anchor, { width: HOVER_CARD_WIDTH, height }, viewportSize());

  return (
    <div
      ref={ref}
      role="tooltip"
      data-ui-bridge-id={SESSION_HOVER_CARD_ID}
      data-effect={SESSION_HOVER_CARD.effect}
      data-session-id={model.sessionId}
      data-hover-card-side={placement.side}
      onPointerEnter={onPointerEnter}
      onPointerLeave={onPointerLeave}
      style={{
        position: "fixed",
        left: placement.left,
        top: placement.top,
        width: placement.width,
        maxHeight: placement.maxHeight,
      }}
      className="z-50 overflow-y-auto scrollbar-dark rounded-md border border-[#2a2d3d] bg-[#13141f] shadow-lg px-3 py-2 text-[11px]"
    >
      <div className="text-xs font-medium text-[#c0caf5] break-words mb-1" title={model.title}>
        {model.title}
      </div>
      <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-0.5">
        {model.fields.map((f) => (
          <div key={f.key} className="contents" data-hover-field={f.key}>
            <dt className="text-[#565f89]">{f.label}</dt>
            <dd
              className="min-w-0 break-words"
              title={f.title}
              data-unknown={f.unknown ? "true" : "false"}
            >
              <span className={f.unknown ? "italic text-[#565f89]" : "text-[#a9b1d6]"}>
                {f.value}
              </span>
              {f.detail && <div className="text-[10px] text-[#565f89]">{f.detail}</div>}
            </dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

// ── Hover state ──────────────────────────────────────────────────────────────

/** Delay before the card opens, so sweeping the pointer down the list is quiet. */
export const HOVER_CARD_OPEN_DELAY_MS = 450;
/** Grace to move the pointer from the row onto the card. */
export const HOVER_CARD_CLOSE_DELAY_MS = 150;

export interface HoverTarget {
  sessionId: string;
  anchor: Rect;
}

export interface SessionHoverCardController {
  target: HoverTarget | null;
  /** Spread onto a row (or rail item) to make it open the card. */
  bind: (sessionId: string) => {
    onMouseEnter: (e: React.MouseEvent<HTMLElement>) => void;
    onMouseLeave: () => void;
  };
  /** Spread onto the card, so moving onto it keeps it open. */
  cardHandlers: { onPointerEnter: () => void; onPointerLeave: () => void };
  close: () => void;
}

function rectOf(el: HTMLElement): Rect {
  const r = el.getBoundingClientRect();
  return { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
}

export function useSessionHoverCard(): SessionHoverCardController {
  const [target, setTarget] = useState<HoverTarget | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const clear = () => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = null;
  };
  useEffect(() => clear, []);

  const close = useCallback(() => {
    clear();
    setTarget(null);
  }, []);

  const scheduleClose = useCallback(() => {
    clear();
    timer.current = setTimeout(() => setTarget(null), HOVER_CARD_CLOSE_DELAY_MS);
  }, []);

  const bind = useCallback(
    (sessionId: string) => ({
      onMouseEnter: (e: React.MouseEvent<HTMLElement>) => {
        const anchor = rectOf(e.currentTarget);
        clear();
        timer.current = setTimeout(
          () => setTarget({ sessionId, anchor }),
          HOVER_CARD_OPEN_DELAY_MS,
        );
      },
      onMouseLeave: scheduleClose,
    }),
    [scheduleClose],
  );

  const cardHandlers = {
    onPointerEnter: clear,
    onPointerLeave: scheduleClose,
  };

  return { target, bind, cardHandlers, close };
}
