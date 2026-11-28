/**
 * OSC 633 shell integration, as one pane's `TerminalInstance` reports it.
 *
 * Free of xterm and Tauri imports so the parse, the replay summary and the
 * "marks went unparsed" rule are testable under `environment: "node"`, where
 * `TerminalInstance` itself cannot be imported (it pulls `@xterm/addon-canvas`,
 * which touches `self` at module init). `TerminalInstance` is a thin shell over
 * {@link shellMarksSink}.
 */

/**
 * The capability payload (`633;P;CommandMarks=1`): this prompt's shell marks
 * every typed command line with a command start (633;C). Pinned, byte for byte,
 * against the three shell-integration scripts by a source scan in
 * `src-tauri/src/session/spawn_prompt.rs`.
 */
export const COMMAND_MARKS_CAPABILITY = "P;CommandMarks=1";

/**
 * The prompt-state half of a pane's shell integration: which of the two marks
 * this window last parsed from it — `prompt_start` (633;A, the shell drew its
 * prompt) or `command_execute` (633;C, the shell started a typed command).
 */
export type ShellPromptMark = "prompt_start" | "command_execute";

export type ShellIntegrationEvent =
  | { type: "prompt_start" }
  | { type: "command_ready" }
  | { type: "command_execute" }
  | { type: "command_done"; exitCode: number }
  | { type: "command_line"; command: string }
  | { type: "cwd"; path: string }
  /**
   * `633;P;CommandMarks=1`, sent with a prompt (before its 633;A): THIS prompt's
   * shell marks every typed command line with 633;C. The runner's bash
   * integration sends it on bash >= 4.4 (C comes from PS0), its zsh integration
   * always (C from preexec), PowerShell only with PSReadLine and never at a
   * nested prompt. It is per prompt: a prompt without it is not covered.
   */
  | { type: "command_marks" }
  /**
   * Synthetic, never on the wire: this window stopped parsing the pane's
   * output, or lost some of it — a chunk dropped unparsed while the pane was
   * hidden, a resync whose ring no longer held the missed bytes, a spliced
   * resync, or the pane unmounting. What its shell reported before is no
   * longer known to be its current state.
   */
  | { type: "unobserved" }
  /**
   * Synthetic, never on the wire: the prompt state a mount's scrollback-ring
   * replay ends in — the replay is contiguous with the live stream, so its last
   * A/C (and whether that prompt carried the capability) is current. Only the
   * marks are re-derived; a replay's E/D/cwd are history, never re-reported.
   */
  | {
      type: "replayed_marks";
      mark: ShellPromptMark | undefined;
      announcesCommandStarts: boolean;
      /**
       * A capability the replay ended on before its prompt's 633;A (the ring
       * stopped between bash's PROMPT_COMMAND entries): it carries over to the
       * live A that follows.
       */
      pendingAnnounce: boolean;
    };

/** One OSC 633 payload (the bytes after `633;`) as an event, or `null` for one this window does not read. */
export function parseOsc633(data: string): ShellIntegrationEvent | null {
  if (data === "A") return { type: "prompt_start" };
  if (data === "B") return { type: "command_ready" };
  if (data === "C") return { type: "command_execute" };
  if (data.startsWith("D")) {
    const code = parseInt(data.slice(2), 10);
    return { type: "command_done", exitCode: isNaN(code) ? 0 : code };
  }
  if (data.startsWith("E;")) {
    return { type: "command_line", command: data.slice(2).replace(/\\x3b/g, ";") };
  }
  if (data.startsWith("P;Cwd=")) return { type: "cwd", path: data.slice(6) };
  if (data === COMMAND_MARKS_CAPABILITY) return { type: "command_marks" };
  return null;
}

/** Where one pane's shell-integration events go. */
export interface ShellMarksSink {
  /** A parsed OSC 633 sequence. */
  osc633(data: string): void;
  /**
   * Output of this pane that this window will never parse (dropped while
   * hidden, overrun out of the ring, spliced out of a resync, painted over by a
   * reconnect). Clears the marks now AND once more behind whatever the backend
   * has already queued: xterm parses asynchronously, so a chunk written before
   * the loss can still deliver its 633;A after the first clear. During a
   * mount's replay it also spoils the replay's summary: the lost bytes are
   * newer than the ring, so the ring's last mark is not current.
   */
  outputLost(): void;
  /**
   * A chunk dropped unparsed while the pane is hidden: {@link outputLost}, but
   * the clear behind the write queue is queued only when one queued during the
   * SAME hidden spell is pending — a hidden pane drops a chunk per event, and
   * nothing it drops is written, so that clear already sits behind everything
   * written before the drops. A pane shown in between ({@link resumed}) has
   * written since (its resync replays the drops), so the next drop queues its
   * own.
   */
  outputDropped(): void;
  /**
   * The pane is shown again: writes resume (the reveal resync first), so a
   * clear queued before them no longer sits behind everything written, and the
   * next {@link outputDropped} must queue a new one.
   */
  resumed(): void;
  /** The pane's `TerminalInstance` unmounted. */
  unmounted(): void;
  /**
   * A mount's ring replay starts: until {@link endReplay}, parsed sequences
   * update only a private summary of the marks — nothing else is reported.
   */
  beginReplay(): void;
  /**
   * The replay (and whatever was drained behind it) has been PARSED: report
   * the summary as one `replayed_marks`. Call it from a write callback queued
   * after the replay, so it runs once xterm's asynchronous parse reached it.
   */
  endReplay(): void;
}

/**
 * The sink a `TerminalInstance` feeds. `callback` is read at call time (the
 * component keeps its latest `onShellIntegration` in a ref), so the event goes
 * to whichever page's handler holds the pane NOW. `queueBehindWrites(cb)` must
 * run `cb` after every write already handed to the backend has been parsed —
 * `backend.write(new Uint8Array(0), cb)`.
 */
export function shellMarksSink(
  callback: () => ((event: ShellIntegrationEvent) => void) | undefined,
  queueBehindWrites: (cb: () => void) => void,
): ShellMarksSink {
  const emit = (event: ShellIntegrationEvent) => callback()?.(event);
  let replay: {
    mark: ShellPromptMark | undefined;
    pendingAnnounce: boolean;
    announced: boolean;
    lost: boolean;
  } | null = null;
  const unobserved = () => emit({ type: "unobserved" });
  // Hidden drops coalesce their clear behind the write queue (see
  // `outputDropped`); an explicit loss always queues its own, at its position.
  // The pending coalesced clear, by number (0: none). `resumed` forgets it, so
  // a clear queued before the pane was shown stands for no later drop — and,
  // when it runs, it cannot release a newer one's slot.
  let pendingDropClear = 0;
  let dropClears = 0;
  const lost = () => {
    if (replay !== null) replay.lost = true;
    unobserved();
  };
  return {
    osc633: (data) => {
      const event = parseOsc633(data);
      if (!event) return;
      if (replay === null) {
        emit(event);
        return;
      }
      // Replaying: fold the marks into the summary, report nothing.
      if (event.type === "command_marks") {
        replay.pendingAnnounce = true;
      } else if (event.type === "prompt_start") {
        replay.mark = "prompt_start";
        replay.announced = replay.pendingAnnounce;
        replay.pendingAnnounce = false;
      } else if (event.type === "command_execute") {
        replay.mark = "command_execute";
        replay.pendingAnnounce = false;
      }
    },
    outputLost: () => {
      lost();
      queueBehindWrites(unobserved);
    },
    outputDropped: () => {
      lost();
      if (pendingDropClear === 0) {
        const clear = ++dropClears;
        pendingDropClear = clear;
        queueBehindWrites(() => {
          if (pendingDropClear === clear) pendingDropClear = 0;
          unobserved();
        });
      }
    },
    resumed: () => {
      pendingDropClear = 0;
    },
    unmounted: () => {
      replay = null;
      unobserved();
    },
    beginReplay: () => {
      replay = { mark: undefined, pendingAnnounce: false, announced: false, lost: false };
    },
    endReplay: () => {
      if (replay === null) return;
      const { mark, announced, pendingAnnounce, lost } = replay;
      replay = null;
      // Output lost while the replay was in flight is newer than the ring: the
      // summary would report a mark that is no longer the pane's last.
      emit(
        lost
          ? {
              type: "replayed_marks",
              mark: undefined,
              announcesCommandStarts: false,
              pendingAnnounce: false,
            }
          : {
              type: "replayed_marks",
              mark,
              announcesCommandStarts: mark === "prompt_start" && announced,
              pendingAnnounce,
            },
      );
    },
  };
}
