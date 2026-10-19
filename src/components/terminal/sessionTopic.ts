/**
 * A session's TOPIC — what it is about, as opposed to its headline name (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 7, D6). Deterministic, from fields every row already carries; no LLM
 * call.
 *
 * Precedence: the transcript's `workSummaryHint`, else the resume name, else
 * the first prompt. A candidate that merely repeats the headline is skipped —
 * the card's second line exists to say something the first line does not.
 *
 * Pure: no React, no Tauri.
 */

/** Which field the topic came from — the card's tooltip names it. */
export type SessionTopicSource = "work-summary" | "resume-name" | "first-prompt";

export interface SessionTopic {
  /** One line: whitespace (including newlines in a prompt) collapsed. */
  text: string;
  source: SessionTopicSource;
}

export interface SessionTopicFields {
  workSummaryHint: string | null | undefined;
  resumeName: string | null | undefined;
  firstMessagePreview: string | null | undefined;
}

export const SESSION_TOPIC_SOURCE_LABELS: Readonly<Record<SessionTopicSource, string>> = {
  "work-summary": "work summary",
  "resume-name": "resume name",
  "first-prompt": "first prompt",
};

function oneLine(s: string | null | undefined): string {
  return (s ?? "").replace(/\s+/g, " ").trim();
}

/**
 * The topic, or `null` when no field carries one. `headline` (the card's
 * name) is skipped as a candidate, compared case- and whitespace-insensitively.
 */
export function sessionTopic(
  fields: SessionTopicFields,
  headline: string | null = null,
): SessionTopic | null {
  const same = oneLine(headline).toLowerCase();
  const candidates: [string, SessionTopicSource][] = [
    [oneLine(fields.workSummaryHint), "work-summary"],
    [oneLine(fields.resumeName), "resume-name"],
    [oneLine(fields.firstMessagePreview), "first-prompt"],
  ];
  for (const [text, source] of candidates) {
    if (text && text.toLowerCase() !== same) return { text, source };
  }
  return null;
}

/** The topic line's `title`: the full value and where it came from. */
export function sessionTopicTitle(topic: SessionTopic): string {
  return `${topic.text}\n(topic from the ${SESSION_TOPIC_SOURCE_LABELS[topic.source]})`;
}
