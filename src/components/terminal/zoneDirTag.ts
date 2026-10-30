/**
 * The directory tag the command palette's "Auto-tag zones by working
 * directory" adds to a zone: the last segment of the tab's working directory.
 *
 * It used to be parsed out of `tab.title`, which only worked because the OSC
 * 0/2 relay renamed the tab to the shell's cwd. Since plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`
 * D3 the OSC string lands on `tab.activityTitle` and the title keeps the
 * session's name, so the title no longer carries a directory. `workingDir`
 * (set at spawn, updated by the shell-integration `cwd` event) is the field
 * that names it. When it is unknown, the old title parse runs over the OSC
 * title (`activityTitle`), else the tab title.
 */
import type { TerminalTab } from "./useTerminalManager";

/** The last non-empty segment of a `/`- or `\`-separated path, or `null`. */
function lastPathSegment(path: string | undefined): string | null {
  if (!path) return null;
  const seg = path
    .split(/[/\\]/)
    .map((s) => s.trim())
    .filter(Boolean)
    .pop();
  return seg ?? null;
}

/** The pre-D3 parse: a trailing path segment, else what follows a colon, else the whole title. */
function dirFromTitle(title: string): string {
  const pathMatch = title.match(/[/\\]([^/\\]+)\s*$/);
  if (pathMatch) return pathMatch[1];
  const colonMatch = title.match(/:\s*(.+)/);
  if (colonMatch) {
    const afterColon = colonMatch[1].trim();
    const lastSeg = afterColon.match(/[/\\]([^/\\]+)\s*$/);
    return lastSeg ? lastSeg[1] : afterColon;
  }
  return title;
}

/** The zone's directory tag for `tab`, or `null` when nothing names one. */
export function zoneDirTag(
  tab: Pick<TerminalTab, "workingDir" | "activityTitle" | "title">,
): string | null {
  const fromDir = lastPathSegment(tab.workingDir);
  if (fromDir) return fromDir;
  const title = (tab.activityTitle?.trim() || tab.title || "").trim();
  if (!title) return null;
  const tag = dirFromTitle(title).trim();
  return tag.length > 0 ? tag : null;
}
