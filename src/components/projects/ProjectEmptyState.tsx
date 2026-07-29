/**
 * The empty state (plan §9) — and note that this is Stefan's FIRST screen, not
 * an error condition. A fresh install has zero saved projects, so this is the
 * most-seen state the page has, not the rarest.
 *
 * Hence: no "No projects found." The copy names the one action that fixes the
 * state it is describing.
 *
 * "Start something new" is present but disabled: creating a project needs
 * scaffolding that does not exist yet, and it is scoped to the split plan
 * `2026-07-24-runner-new-project-flow` (see the dashboard plan §13). Showing it
 * disabled rather than hiding it is deliberate — it tells the user the capability
 * is coming rather than implying the app can only ever adopt existing folders.
 */

import { FolderSearch } from "lucide-react";

import { useUIComponent } from "@qontinui/ui-bridge";

export interface ProjectEmptyStateProps {
  /** Pick a folder and scan it for projects (`scan_workspace_for_setup`). */
  onChooseFolder: () => void;
  /** True while a scan is in flight. */
  scanning?: boolean;
}

export function ProjectEmptyState({ onChooseFolder, scanning = false }: ProjectEmptyStateProps) {
  useUIComponent({
    id: "projects.empty-state",
    name: "Projects empty state",
    description: "First-run state offering to scan a folder for projects",
    actions: [
      {
        id: "choose-folder",
        label: "Choose a folder",
        description: "Open the folder picker and scan the chosen folder for projects.",
        handler: () => {
          onChooseFolder();
        },
      },
    ],
  });

  return (
    <div
      data-testid="projects-empty-state"
      className="flex h-full flex-col items-center justify-center px-6 text-center"
    >
      <FolderSearch aria-hidden="true" className="mb-4 h-10 w-10 text-muted-foreground" />

      <h2 className="text-xl font-semibold">Let&apos;s find your projects.</h2>
      <p className="mt-2 max-w-md text-sm text-muted-foreground">
        Qontinui can look through a folder on your computer and find the things you&apos;re
        building.
      </p>

      <div className="mt-6 flex flex-wrap items-center justify-center gap-3">
        <button
          type="button"
          onClick={onChooseFolder}
          disabled={scanning}
          className="rounded-md bg-primary px-4 py-2 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-60"
        >
          {scanning ? "Looking…" : "Choose a folder"}
        </button>
        <button
          type="button"
          disabled
          title="Creating a new project is coming soon"
          className="rounded-md border border-border px-4 py-2 text-sm font-medium text-muted-foreground disabled:opacity-60"
        >
          Start something new
        </button>
      </div>
    </div>
  );
}
