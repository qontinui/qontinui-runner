/**
 * OSC 633 shell integration, as one pane's `TerminalInstance` reports it.
 *
 * A leaf module (no imports) so the parse and the "marks went unparsed" rule
 * are testable under `environment: "node"`, where `TerminalInstance` itself
 * cannot be imported (it pulls `@xterm/addon-canvas`, which touches `self` at
 * module init). `TerminalInstance` is a thin shell over {@link shellMarksSink}.
 */

export type ShellIntegrationEvent =
  | { type: "prompt_start" }
  | { type: "command_ready" }
  | { type: "command_execute" }
  | { type: "command_done"; exitCode: number }
  | { type: "command_line"; command: string }
  | { type: "cwd"; path: string }
  /**
   * `633;P;CommandMarks=1`: this pane's shell emits a command-start mark
   * (633;C) for every command it runs. The runner's bash and zsh integrations
   * announce it with every prompt; PowerShell's only when PSReadLine is loaded,
   * the one place it can emit 633;C at all.
   */
  | { type: "command_marks" }
  /**
   * Synthetic, never on the wire: this window stopped parsing the pane's
   * output — a chunk was dropped unparsed while the pane was hidden, or the
   * pane unmounted (and a remount's scrollback replay is written before the
   * OSC handler is registered). Whatever the pane's shell reported before is
   * no longer known to be its current state.
   */
  | { type: "unobserved" };

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
  if (data === "P;CommandMarks=1") return { type: "command_marks" };
  return null;
}

/** Where one pane's shell-integration events go. */
export interface ShellMarksSink {
  /** A parsed OSC 633 sequence. */
  osc633(data: string): void;
  /** A chunk of the pane's output was dropped unparsed (the hidden tier). */
  outputMissed(): void;
  /** The pane's `TerminalInstance` unmounted. */
  unmounted(): void;
}

/**
 * The sink a `TerminalInstance` feeds. `callback` is read at call time (the
 * component keeps its latest `onShellIntegration` in a ref), so the event goes
 * to whichever page's handler holds the pane NOW.
 */
export function shellMarksSink(
  callback: () => ((event: ShellIntegrationEvent) => void) | undefined,
): ShellMarksSink {
  const emit = (event: ShellIntegrationEvent) => callback()?.(event);
  return {
    osc633: (data) => {
      const event = parseOsc633(data);
      if (event) emit(event);
    },
    outputMissed: () => emit({ type: "unobserved" }),
    unmounted: () => emit({ type: "unobserved" }),
  };
}
