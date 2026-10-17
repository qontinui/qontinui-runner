/**
 * UI Bridge ids for the fan-out preview and the fan-out status strip.
 *
 * Every repeated element gets an id of its own: a preview row is suffixed with
 * its preview index, a run's controls with the run's short id, and a member's
 * controls with the run's short id and the member index. A shared id across
 * rows or runs makes `click terminal.fanout-strip-release` ambiguous — the UI
 * Bridge resolves one of them, not the one meant. The singleton containers
 * (`terminal.fanout-strip`, `terminal.fanout-preview`) keep their bare ids.
 *
 * Specs that need "any run's pill" match the `data-fanout-part` attribute
 * ({@link FANOUT_PART_RUN_TOGGLE}) rather than an id.
 */

/** The strip's container id — one per page, never suffixed. */
export const FANOUT_STRIP_ID = "terminal.fanout-strip";

/** `data-fanout-part` on every run's toggle: what a spec matches for "a pill". */
export const FANOUT_PART_RUN_TOGGLE = "run-toggle";

/** The parts of one preview row that carry an id. `""` is the row itself. */
export type PreviewRowPart = "" | "tick" | "expand" | "prompt" | "collisions";

/** `terminal.fanout-preview-row[-part].<previewIndex>` */
export function fanoutPreviewRowId(part: PreviewRowPart, previewIndex: number): string {
  return `terminal.fanout-preview-row${part ? `-${part}` : ""}.${previewIndex}`;
}

/** The parts of one run's pill and panel that carry an id. */
export type StripRunPart =
  | "run"
  | "toggle"
  | "age"
  | "panel"
  | "cap-decrease"
  | "cap-value"
  | "cap-increase"
  | "cancel-queued"
  | "note";

/** The parts of one member row that carry an id. */
export type StripMemberPart = "member" | "release";

/** The shortest prefix length a run key starts at — the strip's own `run.id.slice(0, 8)`. */
const SHORT_RUN_KEY_LEN = 8;

/**
 * A short key per run id, unique among `runIds`: the first 8 characters, grown
 * for any ids sharing that prefix until they differ (the full id at worst).
 */
export function shortRunKeys(runIds: readonly string[]): Map<string, string> {
  const keys = new Map<string, string>();
  const unique = [...new Set(runIds)];
  for (const id of unique) {
    let len = Math.min(SHORT_RUN_KEY_LEN, id.length);
    while (
      len < id.length &&
      unique.some((o) => o !== id && o.slice(0, len) === id.slice(0, len))
    ) {
      len += 1;
    }
    keys.set(id, id.slice(0, len));
  }
  return keys;
}

/** `terminal.fanout-strip-<part>.<runKey>` */
export function fanoutStripRunId(part: StripRunPart, runKey: string): string {
  return `terminal.fanout-strip-${part}.${runKey}`;
}

/** `terminal.fanout-strip-<part>.<runKey>.<memberIndex>` */
export function fanoutStripMemberId(
  part: StripMemberPart,
  runKey: string,
  memberIndex: number,
): string {
  return `terminal.fanout-strip-${part}.${runKey}.${memberIndex}`;
}
