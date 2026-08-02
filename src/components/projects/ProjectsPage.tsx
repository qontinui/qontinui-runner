/**
 * Projects — the runner's landing page (plan §5.1, §9).
 *
 * Answers the question the Terminal assumes you have already answered: what am
 * I building, and what state is each one in. That is why it is `DEFAULT_TAB_ID`
 * and the first WORKSPACE nav item — the Terminal is where work happens, but it
 * needs you to already know which directory you want.
 *
 * Scope: this is step 2 of the plan's suggested order (grid + empty state +
 * discovery), plus the step-3 hand-off to the terminal. The detail view (§5.2)
 * is a later step; clicking a card is therefore not yet a navigation.
 */

import { useCallback, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { FolderSearch, Loader2 } from "lucide-react";

import { createLogger } from "@/lib/logger";
import { useUIComponent } from "@qontinui/ui-bridge";
import { useSavedProjects, type SavedProject } from "@/hooks/useSavedProjects";
import { setActiveProjectHint } from "@/components/terminal/activeProject";
import { buildFixPrompt } from "./fixThis";
import { useProjectSnapshots } from "./useProjectSnapshots";
import { useProjectOpen } from "./useProjectOpen";
import { sortProjects } from "./cardExtras";
import { ProjectCard } from "./ProjectCard";
import { ProjectDetail } from "./ProjectDetail";
import { ProjectEmptyState } from "./ProjectEmptyState";

const log = createLogger("ProjectsPage");

export interface ProjectsPageProps {
  /**
   * Switch to the Terminal tab. Supplied by `TabContent`; optional so the page
   * can be rendered standalone in a test.
   */
  onNavigateToTerminal?: () => void;
}

export function ProjectsPage({ onNavigateToTerminal }: ProjectsPageProps) {
  const { projects, loading, error, refresh, saveAll } = useSavedProjects();
  const { snapshots, refresh: refreshSnapshots } = useProjectSnapshots(projects);
  const { phases: openPhases, open: openProject, dismiss: dismissOpen } = useProjectOpen();

  const [scanning, setScanning] = useState(false);
  const [scanMessage, setScanMessage] = useState<string | null>(null);
  /**
   * Which project's detail view is showing (§5.2), or `null` for the grid.
   *
   * Held as an id rather than the project object so a background refresh of the
   * registry re-renders the detail with fresh fields instead of pinning a stale
   * copy — and so a project deleted while its detail is open falls back to the
   * grid rather than rendering a ghost.
   */
  const [detailId, setDetailId] = useState<string | null>(null);

  /**
   * "Find on disk" — pick a folder, scan it, and merge anything new into the
   * registry.
   *
   * `discover_projects` already filters out roots the registry knows, so this
   * only ever ADDS. It never removes a project the user has kept, which
   * matters because discovery is heuristic: a scan that failed to recognise a
   * project must not be able to delete it.
   */
  const handleChooseFolder = useCallback(async () => {
    setScanMessage(null);
    let selected: string | null;
    try {
      selected = (await open({ directory: true, multiple: false })) as string | null;
    } catch (err) {
      log.error("Folder picker failed", String(err));
      setScanMessage("Could not open the folder picker.");
      return;
    }
    if (!selected) return;

    setScanning(true);
    try {
      const found = await invoke<SavedProject[]>("discover_projects", {
        workspaceDir: selected,
      });
      if (found.length === 0) {
        setScanMessage("Nothing new in that folder.");
        return;
      }
      await saveAll([...projects, ...found]);
      setScanMessage(found.length === 1 ? "Added 1 project." : `Added ${found.length} projects.`);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      log.error("discover_projects failed", msg);
      setScanMessage(`Could not scan that folder: ${msg}`);
    } finally {
      setScanning(false);
    }
  }, [projects, saveAll]);

  /**
   * Publish the activation. Everything §7.2 asks for downstream —
   * bind/create the project's Terminal page, pin that page's
   * `defaultWorkingDir` to the project root, reinstate `zoneProfile`, and
   * offer to move terminals that sit elsewhere — happens in
   * `useProjectPageActivation` and `ProjectFolderChip`, both of which follow
   * this event. This page never reaches into the terminal tree itself; see
   * `components/terminal/activeProject.ts` for the contract.
   *
   * `terminalPageId` and `zoneProfile` ride along because the terminal side
   * has no way to read the project registry: the hint IS the transport.
   */
  const activateProject = useCallback(
    (project: SavedProject, seed?: { prompt: string; id: string }) => {
      setActiveProjectHint({
        id: project.id,
        name: project.name,
        path: project.path,
        terminalPageId: project.terminalPageId,
        zoneProfile: project.zoneProfile,
        seedPrompt: seed?.prompt,
        seedPromptId: seed?.id,
      });
    },
    [],
  );

  /**
   * Pin / unpin (§5.1).
   *
   * Persists first, then re-reads the registry, rather than toggling local
   * state optimistically: `set_project_pinned` returns the value it actually
   * stored, and a failed write that left the card looking pinned would be a
   * lie the next refresh silently reverts.
   */
  const handleTogglePin = useCallback(
    async (project: SavedProject) => {
      try {
        await invoke<boolean>("set_project_pinned", {
          id: project.id,
          pinned: !(project.pinned ?? false),
        });
        await refresh();
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        log.error("set_project_pinned failed", msg);
        setScanMessage(`Could not pin that project: ${msg}`);
      }
    },
    [refresh],
  );

  /** "Work on it" — activate, then hand off to the Terminal. */
  const handleWorkOnIt = useCallback(
    (project: SavedProject) => {
      activateProject(project);
      onNavigateToTerminal?.();
    },
    [activateProject, onNavigateToTerminal],
  );

  /**
   * "Fix this" (§8.4) — `Work on it`, plus the failure description an agent
   * needs, so Stefan never has to write the bug report himself. He can see the
   * red dot; he cannot say which process died or what it printed.
   *
   * The prompt is composed HERE rather than in the terminal because this is
   * where the snapshot lives — the terminal side has no access to the project
   * registry, let alone its health (`activeProject.ts` states that contract),
   * which is exactly why the seed rides on the activation hint.
   *
   * A fresh `seedPromptId` per click is what keeps one click to one agent: the
   * hint is cached in `instanceStorage` and replayed on reload and on
   * cross-window `storage` events, so without an identity the consumer would
   * re-spawn a session every time the webview reloaded.
   */
  const handleFixThis = useCallback(
    (project: SavedProject) => {
      const prompt = buildFixPrompt({
        projectName: project.name,
        projectPath: project.path,
        snapshot: snapshots[project.id],
      });
      if (!prompt) {
        // The card only offers this on amber/red, so reaching here means the
        // snapshot changed under the click. Degrade to plain activation rather
        // than spawning an agent with nothing to go on.
        log.warn("Fix this: no fix prompt could be composed; falling back to Work on it");
        handleWorkOnIt(project);
        return;
      }
      activateProject(project, { prompt, id: crypto.randomUUID() });
      onNavigateToTerminal?.();
    },
    [activateProject, handleWorkOnIt, onNavigateToTerminal, snapshots],
  );

  /**
   * "Open" — §7.1: start what the project needs, wait for it, then show it in
   * the Preview window.
   *
   * It activates first so the Terminal is already bound to the project by the
   * time the site comes up: the user who then wants to change something is in
   * the right directory without a second click. It does NOT navigate to the
   * Terminal — Open's whole point is that the user stays looking at the
   * dashboard while the prose reports progress, and then at their site.
   */
  const handleOpen = useCallback(
    (project: SavedProject) => {
      activateProject(project);
      void openProject(project, snapshots[project.id]);
    },
    [activateProject, openProject, snapshots],
  );

  // Declared after the callbacks so the action handlers can close over them.
  // Hooks stay above every early return — the loading/error/empty branches
  // below must not change how many hooks this component runs.
  useUIComponent({
    id: "projects.page",
    name: "Projects",
    description: "Grid of saved projects with their live status",
    actions: [
      {
        id: "find-on-disk",
        label: "Find on disk",
        description: "Pick a folder and add any projects found in it to the registry.",
        handler: () => {
          void handleChooseFolder();
        },
      },
      {
        id: "refresh",
        label: "Refresh",
        description: "Re-read the project list and every project's live status.",
        handler: async () => {
          await refresh();
          await refreshSnapshots();
        },
      },
    ],
  });

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-muted-foreground">
        <Loader2 aria-hidden="true" className="h-5 w-5 animate-spin" />
        <span className="text-sm">Loading projects…</span>
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 px-6 text-center">
        <p className="text-sm text-muted-foreground">Could not read your projects.</p>
        <p className="max-w-md text-xs text-muted-foreground/80">{error}</p>
        <button
          type="button"
          onClick={() => void refresh()}
          className="rounded-md border border-border px-3 py-1.5 text-sm font-medium hover:bg-accent"
        >
          Try again
        </button>
      </div>
    );
  }

  // Resolved every render: a project removed while its detail was open simply
  // has no match, and the page falls back to the grid instead of a ghost.
  const detailProject = detailId ? projects.find((p) => p.id === detailId) : undefined;
  if (detailProject) {
    return (
      <ProjectDetail
        project={detailProject}
        snapshot={snapshots[detailProject.id]}
        openPhase={openPhases[detailProject.id]}
        onBack={() => setDetailId(null)}
        onOpen={handleOpen}
        onWorkOnIt={handleWorkOnIt}
        onRefresh={refreshSnapshots}
      />
    );
  }

  if (projects.length === 0) {
    return (
      <div className="h-full" data-page-id="projects">
        <ProjectEmptyState onChooseFolder={() => void handleChooseFolder()} scanning={scanning} />
        {scanMessage ? (
          <p className="pb-6 text-center text-sm text-muted-foreground">{scanMessage}</p>
        ) : null}
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col" data-page-id="projects">
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-6 py-4">
        <h1 className="text-lg font-semibold">Your projects</h1>
        <button
          type="button"
          onClick={() => void handleChooseFolder()}
          disabled={scanning}
          className="flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm font-medium hover:bg-accent disabled:opacity-60"
        >
          {scanning ? (
            <Loader2 aria-hidden="true" className="h-4 w-4 animate-spin" />
          ) : (
            <FolderSearch aria-hidden="true" className="h-4 w-4" />
          )}
          {scanning ? "Looking…" : "Find on disk"}
        </button>
      </header>

      {scanMessage ? (
        <p className="px-6 pt-3 text-sm text-muted-foreground">{scanMessage}</p>
      ) : null}

      <div className="grid flex-1 auto-rows-min grid-cols-[repeat(auto-fill,minmax(18rem,1fr))] gap-4 overflow-auto p-6">
        {sortProjects(projects).map((project) => (
          <ProjectCard
            key={project.id}
            project={project}
            snapshot={snapshots[project.id]}
            openPhase={openPhases[project.id]}
            onOpen={handleOpen}
            onWorkOnIt={handleWorkOnIt}
            onFixThis={handleFixThis}
            onDismissOpen={dismissOpen}
            onShowDetail={setDetailId}
            onTogglePin={(p) => void handleTogglePin(p)}
          />
        ))}
      </div>
    </div>
  );
}
