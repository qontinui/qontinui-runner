/**
 * Addressable Session Manager sidebar toggle.
 *
 * The Session Manager sidebar (`workflowGen.showSidebar`, hosting the live
 * session list AND the "Previous Sessions" view) used to be reachable ONLY by
 * the `Ctrl+Shift+B` keychord (`useKeyboardShortcuts.ts`) or the `/sessions`
 * terminal command (`commands/useTerminalCommands.ts`). A UI-Bridge driver had
 * no control to click: `POST /ui-bridge/ai/find {"query":"Session Manager"}`
 * returned no match, and driving the panel meant synthesizing a `KeyboardEvent`
 * with `keyCode` shims dispatched at three different targets.
 *
 * This is the real, visible button for that state. Both keychord and command
 * remain — this is purely additive.
 *
 * Addressability contract:
 *   - `data-ui-bridge-id` pins the CONTROL id. A hand-written stamp wins over
 *     the SDK's text-derived auto-id (`useAutoRegister.ts`'s `!existingStamp`
 *     guard) and is echoed verbatim as `uiBridgeId` in
 *     `GET /ui-bridge/control/snapshot`, so
 *     `POST /ui-bridge/control/element/terminal.session-manager-toggle/action
 *     {"action":"click"}` opens/closes the sidebar with no `page/evaluate`.
 *     Same convention as `ZoneHoverActions`' `terminal.close-session-<id>` and
 *     `UnzonedChip`'s `terminal.unzoned-chip-toggle`.
 *   - `title="Session Manager"` is what `ai/find` and the snapshot's
 *     `accessibleName` resolve on, so the control is also discoverable by name.
 *
 * Mounted in `CommandBar`'s docked footer (the one always-visible strip of
 * terminal-page chrome — `StatusStrip`, `ConductorStatusStrip` and
 * `UnzonedChip` all self-hide), immediately left of `SpawnTenantPicker`.
 */

import { PanelLeft, PanelLeftClose } from "lucide-react";

import { useTerminalSession } from "./contexts/TerminalSessionContext";

/**
 * Author-controlled control id. Exported so tests (and callers building
 * UI-Bridge action URLs) reference one constant instead of re-typing the
 * string.
 */
export const SESSION_MANAGER_TOGGLE_ID = "terminal.session-manager-toggle";

export function SessionManagerToggle() {
  const { workflowGen } = useTerminalSession();
  const open = workflowGen.showSidebar;
  const Icon = open ? PanelLeftClose : PanelLeft;

  return (
    <button
      type="button"
      // Author-controlled, state-independent id: the auto-derived id would be
      // minted from the (icon-only, therefore empty) label and would not be
      // stable. See the module doc-comment for the full contract.
      data-ui-bridge-id={SESSION_MANAGER_TOGGLE_ID}
      onClick={() => workflowGen.setShowSidebar((v) => !v)}
      aria-pressed={open}
      // `ai/find` and `accessibleName` resolve on this text — keep it exactly
      // the panel's name.
      title="Session Manager"
      aria-label="Session Manager"
      className={`shrink-0 p-0.5 rounded transition-colors hover:bg-[#2a2d3d] ${
        open ? "text-[#7aa2f7]" : "text-[#565f89] hover:text-[#c0caf5]"
      }`}
    >
      <Icon className="w-3 h-3" aria-hidden />
    </button>
  );
}
