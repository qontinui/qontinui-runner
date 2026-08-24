/**
 * Which `lastOutputLines` the page-level output tap should publish for one tab.
 *
 * The compact view renders a tab's trailing output lines. Two sources can
 * supply them and they are not equally good:
 *
 *  1. The tab's own xterm buffer, read through `getBufferLines`. Authoritative
 *     — xterm has already resolved cursor motion, line rewrites and full-frame
 *     TUI redraws, so what it holds is what the pane would show.
 *  2. A regex ANSI-strip of the raw chunk, accumulated onto what is already
 *     published. Lossy: an overdrawn frame reads as new lines. It exists for
 *     the case where source 1 CANNOT answer — an UNMOUNTED tab, which has no
 *     xterm buffer at all.
 *
 * ## Why this is a module and not two `if`s in `useSessionStateTracking`
 *
 * It was two `if`s, keyed on whether a buffer reader had been SUPPLIED — and
 * `TerminalSessionContext` supplies one unconditionally, so the reader was
 * always truthy and the fallback was unreachable. An unmounted tab therefore
 * got neither source: the reader ran, found no live ref, returned `[]`, and the
 * code did nothing.
 *
 * That was invisible while an unmounted tab was always fed by the runner's
 * `terminal-activity` digest instead. It stopped being invisible when
 * `unwatched_flush_interval_ms` was wired into the emission path (merge-train
 * plan D3): the runner stands the digest down for a session it now emits
 * `terminal-output` for, so this tap became that tab's ONLY feed — and it was
 * updating the tab's state chip, sparkline and last-output time while its
 * output lines stayed frozen.
 *
 * Keying on an EMPTY READ rather than on a missing reader is what fixes it, and
 * making the choice a pure function is what makes it testable: the runner's
 * vitest config is `environment: "node"`, so a hook cannot be rendered — same
 * rationale as `activityDigestTracking.ts`, `flowControl.ts` and
 * `scrollbackReplay.ts`.
 */

/** How many trailing non-empty lines a tab publishes. Matches the runner's
 *  `ACTIVITY_DIGEST_LINES`, so a tap-fed and a digest-fed tab agree in shape. */
export const OUTPUT_LINES_WINDOW = 20;

/**
 * Strip the escape sequences a terminal emits so raw PTY bytes read as text.
 *
 * Deliberately NOT a full VT parser — it cannot be, which is exactly why it is
 * the fallback rather than the primary. Cursor motion is dropped rather than
 * applied, so a redrawn frame appends instead of replacing.
 */
export function stripAnsi(text: string): string {
  return (
    text
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b\[[0-9;:?>=! ]*[a-zA-Z@`]/g, "") // CSI sequences
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "") // OSC sequences
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b[()#%*+\-./][0-9A-Za-z]/g, "") // Character set designations
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b[NODMEHc789>=<]/g, "") // Simple escape sequences
      // eslint-disable-next-line no-control-regex
      .replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, "") // Stray control chars
      .replace(/\r/g, "")
  );
}

/**
 * The lines to publish for this tab, or `null` when this chunk changes nothing.
 *
 * `bufferLines` is what the tab's xterm buffer yielded — empty for an unmounted
 * tab, and empty is the whole trigger for the fallback. `getExisting` is called
 * ONLY on the fallback path, so a mounted tab pays nothing for it.
 *
 * `null` rather than the unchanged array so the caller can skip the store write
 * entirely: this runs once per tab per animation frame on every live terminal.
 */
export function nextOutputLines(
  bufferLines: readonly string[],
  rawText: string,
  getExisting: () => readonly string[],
  max: number = OUTPUT_LINES_WINDOW,
): string[] | null {
  if (bufferLines.length > 0) return bufferLines.slice(-max);

  const fresh = stripAnsi(rawText)
    .split("\n")
    .filter((l) => l.trim().length > 0);
  if (fresh.length === 0) return null;

  // Collapse only CONSECUTIVE repeats. A prompt reprinted after each command
  // is a real repeat and must survive; the same prompt arriving twice in one
  // redraw is the artifact this drops.
  const deduped: string[] = [];
  for (const line of [...getExisting(), ...fresh]) {
    if (deduped.length === 0 || line !== deduped[deduped.length - 1]) {
      deduped.push(line);
    }
  }
  return deduped.slice(-max);
}
