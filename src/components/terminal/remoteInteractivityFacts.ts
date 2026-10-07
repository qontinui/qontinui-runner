/**
 * The two per-session interactivity facts coord serves on every fleet row
 * (plan `2026-09-20-remote-session-interactivity-is-a-query-and-both-halves-hold`,
 * A2): `readableRemotely` (bytes the session produced reached a source's pane —
 * measured at the SOURCE) and `writableRemotely` (bytes a source sent were
 * written into its PTY — measured at the TARGET), plus `interactiveSurface`.
 *
 * Pure rendering decisions, so every arm is a unit test (the runner's vitest
 * runs in `node`, with no DOM to render into).
 *
 * The honesty rules this module exists to hold:
 *  - `unknown` is rendered DISTINCTLY from `failed`, with its reason: "nobody
 *    has looked" and "somebody looked and it broke" are different facts.
 *  - A fact coord did not SERVE (an older coord, before the field existed) is
 *    `null` here and renders NOTHING — never "failed", and never a guess.
 *  - A stale fact carries its original `observedAt`, so the age of the last
 *    truth is shown rather than dropped.
 */

import { formatRelativeTime } from "../../lib/formatting";

export type InteractivityState = "ok" | "failed" | "unknown";
export type InteractivityVia = "traffic" | "probe";
export type InteractiveSurface = "runner_pty" | "none" | "not_runner_hosted" | "unknown";

/** coord's `Fact` object, camelCase on the wire. */
export interface InteractivityFact {
  state: InteractivityState;
  /** When the round trip completed; null when there is no observation. */
  observedAt: string | null;
  sourceDeviceId: string | null;
  via: InteractivityVia | null;
  /** A closed `unknown` code, or the wire refusal code of a `failed`. */
  reason: string | null;
}

/** How a fact renders: a tone for colour, a short label, and a tooltip. */
export interface FactView {
  tone: "ok" | "failed" | "unknown";
  label: string;
  title: string;
  /** Machine-readable, for a UI Bridge driver: the served state. */
  state: InteractivityState;
  reason: string | null;
}

/** Operator wording for coord's closed `unknown` vocabulary. */
const UNKNOWN_REASON_TEXT: Record<string, string> = {
  unprobed: "not measured yet",
  stale: "last measurement is stale",
  held_by_other_source: "held by another device",
  target_unreachable: "target unreachable",
  events_unreadable: "coord could not read its observations",
  target_predates_input_ack: "target predates input acknowledgements",
};

function isFact(v: unknown): v is InteractivityFact {
  if (typeof v !== "object" || v === null) return false;
  const s = (v as { state?: unknown }).state;
  return s === "ok" || s === "failed" || s === "unknown";
}

/**
 * Render one fact. `null` when coord served none (an older coord) or served
 * something that is not a fact object — the caller renders nothing for it.
 */
export function describeFact(half: "read" | "write", fact: unknown): FactView | null {
  if (!isFact(fact)) return null;
  const at = fact.observedAt ? formatRelativeTime(fact.observedAt) : null;
  const via = fact.via ? ` via ${fact.via}` : "";
  const seen = fact.observedAt ? ` at ${fact.observedAt}` : "";
  const reason = fact.reason ?? null;
  switch (fact.state) {
    case "ok":
      return {
        tone: "ok",
        label: `${half} ok${at ? ` ${at}` : ""}`,
        title: `${half === "read" ? "Readable" : "Writable"} remotely — measured${via}${seen}`,
        state: "ok",
        reason,
      };
    case "failed":
      return {
        tone: "failed",
        label: `${half} failed${reason ? `: ${reason}` : ""}`,
        title: `${half === "read" ? "Reading" : "Writing"} remotely FAILED${
          reason ? ` (${reason})` : ""
        }${via}${seen}`,
        state: "failed",
        reason,
      };
    case "unknown": {
      const why = reason
        ? Object.prototype.hasOwnProperty.call(UNKNOWN_REASON_TEXT, reason)
          ? UNKNOWN_REASON_TEXT[reason]
          : reason
        : "no reason given";
      // A stale fact keeps its time: the age of the last truth is the point.
      const age = reason === "stale" && at ? ` (${at})` : "";
      return {
        tone: "unknown",
        label: `${half} unknown${age}`,
        title: `Whether this session is ${
          half === "read" ? "readable" : "writable"
        } remotely is UNKNOWN — ${why}${seen}`,
        state: "unknown",
        reason,
      };
    }
  }
}

const SURFACE_TEXT: Record<InteractiveSurface, string> = {
  runner_pty: "runner PTY",
  none: "not interactive",
  not_runner_hosted: "not runner-hosted",
  unknown: "surface unknown",
};

/** The surface label, or null when coord served no (recognised) surface. */
export function describeSurface(surface: unknown): string | null {
  if (typeof surface !== "string") return null;
  return Object.prototype.hasOwnProperty.call(SURFACE_TEXT, surface)
    ? SURFACE_TEXT[surface as InteractiveSurface]
    : null;
}

/**
 * Whether the response comes from a coord that serves interactivity at all.
 * `false` = an older coord: render no facts and run no probe (it has no door
 * to record into).
 */
export function servesInteractivity(
  response: { interactivityEventsPresent?: unknown } | null,
): boolean {
  return typeof response?.interactivityEventsPresent === "boolean";
}

/** Tailwind colour per tone — `unknown` is visibly neither green nor red. */
export const FACT_TONE_CLASS: Record<FactView["tone"], string> = {
  ok: "text-[#9ece6a]",
  failed: "text-[#f7768e]",
  unknown: "text-[#e0af68]",
};

/**
 * The remote devices a Fleet view should probe: every device seen that is not
 * this machine. Probing this machine's own sessions goes through no relay and
 * measures nothing a user would reach remotely.
 */
export function devicesToProbe(
  devices: ReadonlyArray<{ deviceId: string; isCallerDevice: boolean }>,
): string[] {
  const out: string[] = [];
  for (const d of devices) {
    if (!d.isCallerDevice && !out.includes(d.deviceId)) out.push(d.deviceId);
  }
  return out;
}
