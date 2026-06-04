/**
 * scrollKeys — pure mapping from a keyboard event to a terminal scroll action,
 * matching VS Code's integrated-terminal scroll keybindings.
 *
 * VS Code defaults (Windows/Linux; macOS uses ⌘ for Ctrl and ⌥⌘ for the line
 * variants — handled here by accepting Cmd as an alias for Ctrl):
 *
 *   Scroll page up / down    Shift+PageUp     / Shift+PageDown
 *   Scroll line up / down    Ctrl+Alt+PageUp  / Ctrl+Alt+PageDown
 *   Scroll to top / bottom   Ctrl+Home        / Ctrl+End
 *   Prev / next command      Ctrl+Up          / Ctrl+Down
 *
 * Wired into the per-terminal `attachCustomKeyEventHandler` in
 * `TerminalInstance`, which scrolls the focused terminal's scrollback and
 * swallows the key (so it isn't forwarded to the PTY) — VS Code's
 * `commandsToSkipShell` behavior for these bindings.
 *
 * Leaf module (no xterm / DOM imports beyond the `KeyboardEvent` shape) so the
 * mapping is unit-testable without the WASM backend — same rationale as
 * `wheelScroll.ts` / `consumeInputChunk.ts`.
 */

/** A resolved scroll intent. `amount` sign: positive = DOWN, negative = UP. */
export type ScrollAction =
  | { kind: "lines"; amount: number }
  | { kind: "pages"; amount: number }
  | { kind: "top" }
  | { kind: "bottom" }
  | { kind: "prevCommand" }
  | { kind: "nextCommand" };

/** The subset of `KeyboardEvent` the mapping depends on. */
export interface KeyLike {
  key: string;
  shiftKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

/**
 * Map a keydown event to a VS Code-parity terminal scroll action, or `null`
 * when the combo isn't a scroll shortcut (so the caller leaves the key alone).
 * The caller must gate on `keydown` — this does not inspect event type.
 */
export function matchScrollShortcut(e: KeyLike): ScrollAction | null {
  const { key, shiftKey, ctrlKey, altKey, metaKey } = e;
  // Accept Cmd as a Ctrl alias so macOS ⌘Home/⌘End and ⌥⌘PageUp/Down also work.
  const cmdOrCtrl = ctrlKey || metaKey;

  // Page scroll — Shift+PageUp / Shift+PageDown (Shift only).
  if (shiftKey && !ctrlKey && !altKey && !metaKey) {
    if (key === "PageUp") return { kind: "pages", amount: -1 };
    if (key === "PageDown") return { kind: "pages", amount: 1 };
  }

  // Line scroll — Ctrl+Alt+PageUp / Ctrl+Alt+PageDown (⌥⌘ on macOS).
  if (cmdOrCtrl && altKey && !shiftKey) {
    if (key === "PageUp") return { kind: "lines", amount: -1 };
    if (key === "PageDown") return { kind: "lines", amount: 1 };
  }

  // Top / bottom — Ctrl+Home / Ctrl+End (⌘ on macOS), no Alt/Shift.
  // Command navigation — Ctrl+Up / Ctrl+Down (⌘ on macOS): jump between
  // shell prompts (VS Code "Scroll to Previous/Next Command", driven by
  // OSC 633;A shell-integration marks).
  if (cmdOrCtrl && !altKey && !shiftKey) {
    if (key === "Home") return { kind: "top" };
    if (key === "End") return { kind: "bottom" };
    if (key === "ArrowUp") return { kind: "prevCommand" };
    if (key === "ArrowDown") return { kind: "nextCommand" };
  }

  return null;
}
