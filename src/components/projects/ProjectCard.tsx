/**
 * One project card in the grid (plan §5.1).
 *
 * Purely presentational — it receives its snapshot rather than fetching one,
 * so the grid controls concurrency and the card can be rendered in a test
 * without a Tauri runtime.
 *
 * Deliberately absent, per §5.1: path, port, branch, framework version. Those
 * live in the detail view. A card that lists a filesystem path is a card
 * written for a developer, and Stefan is not one.
 */

import { FolderOpen, MessageCircleQuestion, Pin, PinOff } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";

import { useUIComponent } from "@qontinui/ui-bridge";
import type { SavedProject } from "@/hooks/useSavedProjects";
import type { HealthLevel, ProjectSnapshot } from "./types";
import { isLiveProcessState } from "./types";
import { formatRelativeActivity } from "./relativeTime";
import { isOpenBusy, openProse, type OpenPhase } from "./openProject";
import { formatWeeklySpend } from "./cardExtras";

/**
 * Traffic-light colours. `unknown` is intentionally grey and not amber: a
 * project that declares no processes is not "degraded", there is simply
 * nothing to be healthy about (plan §8.4).
 */
const HEALTH_DOT: Record<HealthLevel, string> = {
  green: "bg-emerald-500",
  amber: "bg-amber-500",
  red: "bg-red-500",
  unknown: "bg-muted-foreground/40",
};

/** What the card's status area shows, derived from the snapshot. */
export interface CardStatus {
  /**
   * True when the snapshot has not arrived yet. The card must render
   * "Checking…" in this case rather than "Not running" — those are different
   * facts, and conflating them tells the user their project is stopped when
   * all that has happened is that a Postgres query is still in flight.
   */
  pending: boolean;
  runningCount: number;
  questionCount: number;
  level: HealthLevel;
  /** Tooltip text for the status dot; `undefined` when there is nothing to say. */
  reason?: string;
  /** "2 days ago", or `null` when the project has never been opened. */
  worked: string | null;
}

/**
 * Pure derivation behind the card — extracted so it can be tested. The runner's
 * vitest config is `environment: "node"` (no jsdom), so the JSX itself is not
 * renderable in a test; the logic worth pinning lives here.
 */
export function deriveCardStatus(
  project: SavedProject,
  snapshot: ProjectSnapshot | undefined,
  now = Date.now(),
): CardStatus {
  // `lastOpenedMs` is the registry's own record of the last activation and is
  // the fallback when no snapshot has loaded — it is always present locally,
  // so a card shows a plausible recency line immediately instead of blanking.
  const worked = formatRelativeActivity(snapshot?.lastActivityMs ?? project.lastOpenedMs, now);

  if (!snapshot) {
    return { pending: true, runningCount: 0, questionCount: 0, level: "unknown", worked };
  }

  return {
    pending: false,
    runningCount: snapshot.processes.filter((p) => isLiveProcessState(p.state)).length,
    questionCount: snapshot.questions.length,
    level: snapshot.health?.level ?? "unknown",
    reason: snapshot.health?.reason || undefined,
    worked,
  };
}

export interface ProjectCardProps {
  project: SavedProject;
  /** `undefined` while the snapshot is still loading — NOT "nothing running". */
  snapshot?: ProjectSnapshot;
  /** Where this project's `Open` run is, if one has been started (§7.1). */
  openPhase?: OpenPhase;
  /** Open the project: start its processes and show its front page (§7.1). */
  onOpen: (project: SavedProject) => void;
  /** Bind a terminal to the project root and go there (§7.2). */
  onWorkOnIt: (project: SavedProject) => void;
  /** Clear a finished/failed `Open` phase. */
  onDismissOpen?: (projectId: string) => void;
  /** Show the project's detail view (§5.2). */
  onShowDetail?: (projectId: string) => void;
  /** Pin/unpin — pinned projects sort first and earn a sidebar row (§5.1). */
  onTogglePin?: (project: SavedProject) => void;
}

export function ProjectCard({
  project,
  snapshot,
  openPhase,
  onOpen,
  onWorkOnIt,
  onDismissOpen,
  onShowDetail,
  onTogglePin,
}: ProjectCardProps) {
  useUIComponent({
    id: `projects.card-${project.id}`,
    name: `Project card: ${project.name}`,
    description: `Card for the saved project "${project.name}"`,
    actions: [
      {
        id: "open",
        label: "Open",
        description: `Activate "${project.name}" and show it.`,
        handler: () => {
          onOpen(project);
        },
      },
      {
        id: "work-on-it",
        label: "Work on it",
        description: `Bind a terminal to "${project.name}" and switch to it.`,
        handler: () => {
          onWorkOnIt(project);
        },
      },
      {
        id: "show-detail",
        label: "Show detail",
        description: `Open the detail view for "${project.name}".`,
        handler: () => {
          onShowDetail?.(project.id);
        },
      },
      {
        id: "toggle-pin",
        label: project.pinned ? "Unpin" : "Pin",
        description: `${project.pinned ? "Unpin" : "Pin"} "${project.name}" — pinned projects sort first.`,
        handler: () => {
          onTogglePin?.(project);
        },
      },
    ],
  });

  const { pending, runningCount, questionCount, level, reason, worked } = deriveCardStatus(
    project,
    snapshot,
  );

  const busy = openPhase ? isOpenBusy(openPhase) : false;
  const spend = formatWeeklySpend(snapshot?.spend7dUsd);
  // The raw diagnostic behind the prose: the live stderr tail while starting,
  // the captured one after a failure.
  const openDetail =
    openPhase?.kind === "starting"
      ? openPhase.stderrTail
      : openPhase?.kind === "failed"
        ? openPhase.detail
        : undefined;

  return (
    <div
      data-testid={`project-card-${project.id}`}
      className="flex flex-col rounded-lg border border-border bg-card p-4 text-card-foreground shadow-sm transition-shadow hover:shadow-md"
    >
      <div className="flex items-start gap-3">
        <span aria-hidden="true" className="text-2xl leading-none">
          {project.emoji || "📁"}
        </span>
        {onTogglePin ? (
          <button
            type="button"
            onClick={() => onTogglePin(project)}
            aria-label={project.pinned ? "Unpin project" : "Pin project"}
            title={project.pinned ? "Unpin" : "Pin to the top"}
            className={`order-last shrink-0 rounded p-1 ${
              project.pinned
                ? "text-foreground"
                : "text-muted-foreground/50 hover:text-muted-foreground"
            }`}
          >
            {project.pinned ? (
              <Pin aria-hidden="true" className="h-4 w-4" />
            ) : (
              <PinOff aria-hidden="true" className="h-4 w-4" />
            )}
          </button>
        ) : null}
        <div className="min-w-0 flex-1">
          {/* The name is the affordance for the detail view (§5.2). The whole
              card is deliberately NOT clickable: it carries two primary
              buttons, and a card-wide click target makes those ambiguous. */}
          <h3 className="truncate font-semibold" title={project.name}>
            {onShowDetail ? (
              <button
                type="button"
                onClick={() => onShowDetail(project.id)}
                className="max-w-full truncate text-left hover:underline"
              >
                {project.name}
              </button>
            ) : (
              project.name
            )}
          </h3>
          {project.description ? (
            <p className="mt-0.5 line-clamp-2 text-sm text-muted-foreground">
              {project.description}
            </p>
          ) : null}
        </div>
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1 text-sm">
        {pending ? (
          // Placeholder of the same height so the card does not jump when the
          // snapshot arrives.
          <span className="text-muted-foreground">Checking…</span>
        ) : (
          <span className="flex items-center gap-1.5" title={reason}>
            <span
              aria-hidden="true"
              className={`h-2 w-2 shrink-0 rounded-full ${HEALTH_DOT[level]}`}
            />
            <span className={runningCount > 0 ? "text-foreground" : "text-muted-foreground"}>
              {runningCount > 0 ? "Running" : "Not running"}
            </span>
          </span>
        )}

        {questionCount > 0 ? (
          <span className="flex items-center gap-1.5 text-amber-600 dark:text-amber-500">
            <MessageCircleQuestion aria-hidden="true" className="h-4 w-4 shrink-0" />
            {questionCount === 1 ? "1 question" : `${questionCount} questions`}
          </span>
        ) : null}
      </div>

      <p className="mt-1 flex flex-wrap items-center gap-x-3 text-sm text-muted-foreground">
        <span>{worked ? `Worked on ${worked}` : "Not opened yet"}</span>
        {/* Spend is omitted when NOT MEASURED, but shown at $0.00 — "it cost
            nothing" is a real answer to "is this costing me money?". */}
        {spend ? <span title="Agent spend over the last 7 days">{spend}</span> : null}
      </p>

      {/* §7.1: "surfaced as prose, not a spinner". The stderr tail is the
          reason — a non-developer has to be able to see WHY it is slow. */}
      {openPhase && openPhase.kind !== "idle" ? (
        <div
          data-testid={`project-open-status-${project.id}`}
          className="mt-3 rounded-md border border-border bg-muted/40 p-2 text-sm"
        >
          <div className="flex items-start gap-2">
            <p
              className={
                openPhase.kind === "failed" ? "flex-1 text-red-600 dark:text-red-400" : "flex-1"
              }
            >
              {openProse(openPhase, project.name)}
            </p>
            {!busy && onDismissOpen ? (
              <button
                type="button"
                onClick={() => onDismissOpen(project.id)}
                aria-label="Dismiss"
                className="shrink-0 rounded px-1 text-muted-foreground hover:text-foreground"
              >
                ×
              </button>
            ) : null}
          </div>
          {openDetail ? (
            <pre className="mt-1 max-h-24 overflow-auto whitespace-pre-wrap break-words text-xs text-muted-foreground">
              {openDetail}
            </pre>
          ) : null}
          {/* §7.1's named escape hatch. The Preview window is the default
              because it is a real webview and therefore a UI Bridge target;
              the browser is there for when the operator wants their own. */}
          {openPhase.kind === "done" ? (
            <button
              type="button"
              onClick={() => void openUrl(openPhase.url)}
              className="mt-1 text-xs text-muted-foreground underline hover:text-foreground"
            >
              Open in browser instead
            </button>
          ) : null}
        </div>
      ) : null}

      <div className="mt-4 flex gap-2">
        <button
          type="button"
          onClick={() => onOpen(project)}
          disabled={busy}
          className="flex-1 rounded-md border border-border px-3 py-1.5 text-sm font-medium hover:bg-accent hover:text-accent-foreground disabled:opacity-60"
        >
          {busy ? "Opening…" : "Open"}
        </button>
        <button
          type="button"
          onClick={() => onWorkOnIt(project)}
          className="flex-1 rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90"
        >
          Work on it
        </button>
      </div>
    </div>
  );
}

/** Icon re-exported so the empty state and the grid header stay visually paired. */
export { FolderOpen as ProjectsIcon };
