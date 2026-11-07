/**
 * The terminal work-mode toggle — Grid (every session in a zone) or tmux (one
 * session on screen, the rest live off-screen and reached from the Session
 * Manager). Plan `2026-10-11-runner-terminal-tmux-work-mode`, Phase 4.
 *
 * Mounted in `CommandBar`'s docked footer immediately right of
 * `SessionManagerToggle`, because the two govern the same navigation: in tmux
 * mode the Session Manager is the mode's only navigator, so entering the mode
 * here also opens it (`applyWorkModeRequest`). The `Ctrl+Shift+Y` chord,
 * `/terminal.mode` and the UI Bridge `set-terminal-work-mode` action apply the
 * same rule.
 *
 * Addressability contract (same convention as `SessionManagerToggle`):
 *   - `data-ui-bridge-id` pins the CONTROL id, so
 *     `POST /ui-bridge/control/element/terminal.work-mode-toggle/action
 *     {"action":"click"}` toggles the mode.
 *   - The button's TEXT is the current mode by name ("Grid" / "tmux") and
 *     `data-terminal-work-mode` carries the raw value, so the element is also
 *     the readable mode state — a snapshot shows which mode the page is in
 *     without a separate query.
 *   - `title="Terminal work mode"` is what `ai/find` resolves on.
 *
 * Colours come from the design-system tokens (`brand-primary`, `surface-*`,
 * `text-*`), not literals.
 */

import { LayoutGrid, SquareTerminal } from "lucide-react";

import { useTerminalSession } from "./contexts/TerminalSessionContext";
import { applyWorkModeRequest, workModeLabel } from "./terminalWorkModeControl";

/** Author-controlled control id — tests and UI Bridge callers reference this constant. */
export const TERMINAL_WORK_MODE_TOGGLE_ID = "terminal.work-mode-toggle";

export function TerminalWorkModeToggle() {
  const { zoneLayout, workflowGen } = useTerminalSession();
  const mode = zoneLayout.workMode;
  const tmux = mode === "tmux";
  const Icon = tmux ? SquareTerminal : LayoutGrid;

  return (
    <button
      type="button"
      data-ui-bridge-id={TERMINAL_WORK_MODE_TOGGLE_ID}
      data-terminal-work-mode={mode}
      onClick={() =>
        applyWorkModeRequest(
          {
            workMode: zoneLayout.workMode,
            enterTmuxMode: zoneLayout.enterTmuxMode,
            exitTmuxMode: zoneLayout.exitTmuxMode,
            showSidebar: workflowGen.showSidebar,
            setShowSidebar: workflowGen.setShowSidebar,
          },
          "toggle",
        )
      }
      aria-pressed={tmux}
      // `ai/find` and `accessibleName` resolve on the title; the label adds the
      // current mode so a screen reader hears the state, not just the name.
      title="Terminal work mode"
      aria-label={`Terminal work mode: ${workModeLabel(mode)}`}
      className={`shrink-0 flex items-center gap-1 px-1 py-0.5 rounded text-[10px] font-medium transition-colors hover:bg-surface-hover ${
        tmux ? "bg-brand-primary/15 text-brand-primary" : "text-text-muted hover:text-text-primary"
      }`}
    >
      <Icon className="w-3 h-3" aria-hidden />
      <span>{workModeLabel(mode)}</span>
    </button>
  );
}
