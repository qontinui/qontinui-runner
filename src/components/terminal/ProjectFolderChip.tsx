/**
 * "N terminals are in a different folder" chip — projects-dashboard plan
 * §7.2 step 4.
 *
 * Activating a project must never re-`cd` a running shell (an agent may be
 * mid-edit; re-cwd-ing it is data loss). So instead of silently moving
 * anything, this renders the state `useProjectTerminalReconcile` computes as a
 * soft, dismissible advisory with the two honest choices:
 *
 *   [Move them]  — open replacements at the project root, close the originals
 *                  only once their replacement exists;
 *   [Leave them] — dismiss for this project root.
 *
 * Renders nothing when no project is active, when every live terminal is
 * already at the root, or once dismissed. Visual language matches
 * `CoordWarningBanner` / `DeconflictAdvisoryBanner` (soft amber, top-right
 * advisory column) so the terminal's advisories stack consistently.
 *
 * With no props it follows the active project published on the
 * `project-activated` window event (see `activeProject.ts`); pass
 * `projectRoot` explicitly to drive it directly.
 */

import { FolderInput, X } from "lucide-react";

import { useActiveProjectHint } from "./activeProject";
import { useProjectTerminalReconcile } from "./useProjectTerminalReconcile";

interface ProjectFolderChipProps {
  /** Overrides the active-project hint (mainly for direct/tested use). */
  projectRoot?: string | null;
  /** Display name for the prose; falls back to "the project folder". */
  projectName?: string | null;
}

export function ProjectFolderChip({ projectRoot, projectName }: ProjectFolderChipProps = {}) {
  const hint = useActiveProjectHint();
  const root = projectRoot ?? hint?.path ?? null;
  const name = projectName ?? hint?.name ?? null;

  const { count, moving, error, moveThem, dismiss, shouldPrompt } =
    useProjectTerminalReconcile(root);

  if (!shouldPrompt) return null;

  return (
    <div
      data-ui-bridge-id="terminal.project-folder-chip"
      data-project-root={root ?? undefined}
      data-outside-count={count}
      className="absolute top-24 right-2 z-30 w-[340px]"
    >
      <div className="p-2.5 rounded border bg-[#e0af68]/10 border-[#e0af68]/35 shadow-lg">
        <div className="flex items-start gap-2">
          <FolderInput className="w-3.5 h-3.5 shrink-0 text-[#e0af68] mt-0.5" />
          <div className="text-[11px] text-[#c0caf5] leading-snug flex-1">
            <div>
              {count} terminal{count === 1 ? " is" : "s are"} in a different folder than{" "}
              <span className="font-semibold">{name ?? "the project folder"}</span>.
            </div>
            {error && <div className="mt-1 text-[10px] text-[#f7768e]">{error}</div>}
            <div className="mt-1.5 flex items-center gap-2">
              <button
                type="button"
                data-ui-bridge-id="terminal.project-folder-chip-move"
                disabled={moving}
                onClick={() => void moveThem()}
                className="px-1.5 py-0.5 rounded text-[10px] text-[#e0af68] bg-[#e0af68]/10 hover:bg-[#e0af68]/20 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
              >
                {moving ? "Moving…" : "Move them"}
              </button>
              <button
                type="button"
                data-ui-bridge-id="terminal.project-folder-chip-leave"
                onClick={dismiss}
                className="px-1.5 py-0.5 rounded text-[10px] text-[#565f89] hover:text-[#c0caf5] hover:bg-[#2a2d3d]/50 transition-colors"
              >
                Leave them
              </button>
            </div>
          </div>
          <button
            type="button"
            aria-label="Dismiss folder advisory"
            data-ui-bridge-id="terminal.project-folder-chip-dismiss"
            onClick={dismiss}
            className="text-[#e0af68] hover:text-[#c0caf5] leading-none px-1"
          >
            <X className="w-3 h-3" />
          </button>
        </div>
      </div>
    </div>
  );
}
