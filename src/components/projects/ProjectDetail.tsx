/**
 * The project detail view (plan §5.2).
 *
 * Reached by clicking a card. Everything it shows comes from the one
 * `ProjectSnapshot` the grid already polls — no second fetch, so opening the
 * detail is instant and cannot disagree with the card that led to it.
 *
 * The derivations live in `projectDigest.ts` (pure, unit-tested); this file is
 * layout, the action wiring, and the UI Bridge surface.
 *
 * Deliberately NOT here — §5.2's "START SOMETHING" box and the SAFETY
 * (checkpoint) row. Both need surfaces this change does not build:
 * "start something" means spawning an AI session bound to the project, and the
 * checkpoint row needs per-project checkpoint listing. Shipping a text box that
 * silently does nothing would be worse than not shipping it, so the sections
 * are absent rather than decorative.
 */

import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ArrowLeft, Loader2 } from "lucide-react";

import { createLogger } from "@/lib/logger";
import { useUIComponent } from "@qontinui/ui-bridge";
import type { SavedProject } from "@/hooks/useSavedProjects";
import type { HealthLevel, ProjectSnapshot, SessionLite } from "./types";
import { isLiveProcessState } from "./types";
import { buildDigest, describeSessionWhen, type DigestLine } from "./projectDigest";
import { isOpenBusy, openProse, type OpenPhase } from "./openProject";

const log = createLogger("ProjectDetail");

const DOT: Record<HealthLevel, string> = {
  green: "bg-emerald-500",
  amber: "bg-amber-500",
  red: "bg-red-500",
  unknown: "bg-muted-foreground/40",
};

const LINE_TONE: Record<DigestLine["tone"], string> = {
  neutral: "text-foreground",
  good: "text-emerald-600 dark:text-emerald-500",
  bad: "text-red-600 dark:text-red-400",
};

export interface ProjectDetailProps {
  project: SavedProject;
  /** `undefined` while the snapshot is still loading. */
  snapshot?: ProjectSnapshot;
  openPhase?: OpenPhase;
  onBack: () => void;
  onOpen: (project: SavedProject) => void;
  onWorkOnIt: (project: SavedProject) => void;
  /** Re-poll after an action that changes process state. */
  onRefresh?: () => void | Promise<void>;
}

export function ProjectDetail({
  project,
  snapshot,
  openPhase,
  onBack,
  onOpen,
  onWorkOnIt,
  onRefresh,
}: ProjectDetailProps) {
  /** Process id currently being stopped/restarted — disables just that row. */
  const [busyProcess, setBusyProcess] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const runProcessAction = useCallback(
    async (command: string, id: string) => {
      setBusyProcess(id);
      setActionError(null);
      try {
        await invoke(command, { id });
        await onRefresh?.();
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        log.error(`${command}(${id}) failed`, msg);
        setActionError(msg);
      } finally {
        setBusyProcess(null);
      }
    },
    [onRefresh],
  );

  useUIComponent({
    id: "projects.detail",
    name: `Project detail: ${project.name}`,
    description: `Detail view for "${project.name}" — digest, running processes, recent work.`,
    actions: [
      {
        id: "back",
        label: "Back to projects",
        description: "Return to the project grid.",
        handler: () => onBack(),
      },
      {
        id: "open",
        label: "Open",
        description: `Start what "${project.name}" needs and show it.`,
        handler: () => onOpen(project),
      },
      {
        id: "work-on-it",
        label: "Work on it",
        description: `Bind a terminal to "${project.name}" and switch to it.`,
        handler: () => onWorkOnIt(project),
      },
    ],
  });

  const digest = buildDigest(snapshot, project.lastOpenedMs);
  const busy = openPhase ? isOpenBusy(openPhase) : false;
  const running = (snapshot?.processes ?? []).filter((p) => isLiveProcessState(p.state));
  const recent: SessionLite[] = snapshot?.recentSessions ?? [];

  return (
    <div className="flex h-full flex-col overflow-auto" data-page-id="project-detail">
      <header className="flex flex-wrap items-center gap-3 border-b border-border px-6 py-4">
        <button
          type="button"
          onClick={onBack}
          aria-label="Back to projects"
          className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          <ArrowLeft aria-hidden="true" className="h-4 w-4" />
        </button>
        <span aria-hidden="true" className="text-2xl leading-none">
          {project.emoji || "📁"}
        </span>
        <div className="min-w-0 flex-1">
          <h1 className="truncate text-lg font-semibold">{project.name}</h1>
          {project.description ? (
            <p className="truncate text-sm text-muted-foreground">{project.description}</p>
          ) : null}
        </div>
        <button
          type="button"
          onClick={() => onOpen(project)}
          disabled={busy}
          className="rounded-md border border-border px-3 py-1.5 text-sm font-medium hover:bg-accent disabled:opacity-60"
        >
          {busy ? "Opening…" : "Open"}
        </button>
        <button
          type="button"
          onClick={() => onWorkOnIt(project)}
          className="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90"
        >
          Work on it
        </button>
      </header>

      {openPhase && openPhase.kind !== "idle" ? (
        <p
          data-testid="project-detail-open-status"
          className={`px-6 pt-3 text-sm ${
            openPhase.kind === "failed" ? "text-red-600 dark:text-red-400" : "text-muted-foreground"
          }`}
        >
          {openProse(openPhase, project.name)}
        </p>
      ) : null}

      <div className="flex flex-col gap-6 p-6">
        {/* SINCE YOU WERE LAST HERE — suppressed entirely on a project that has
            never been opened, where the heading would be a lie. */}
        {digest.since && digest.lines.length > 0 ? (
          <section>
            <h2 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
              Since you were last here ({digest.since})
            </h2>
            <ul className="mt-2 flex flex-col gap-1 text-sm">
              {digest.lines.map((line) => (
                <li key={line.id} className={`flex items-center gap-2 ${LINE_TONE[line.tone]}`}>
                  <span aria-hidden="true" className="text-muted-foreground">
                    •
                  </span>
                  <span className="flex-1">{line.text}</span>
                  {line.id === "questions" ? (
                    <button
                      type="button"
                      onClick={() => onWorkOnIt(project)}
                      className="rounded border border-border px-2 py-0.5 text-xs font-medium hover:bg-accent"
                    >
                      Answer
                    </button>
                  ) : null}
                </li>
              ))}
            </ul>
          </section>
        ) : null}

        <section>
          <h2 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            What&apos;s running
          </h2>
          {!snapshot ? (
            <p className="mt-2 flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 aria-hidden="true" className="h-4 w-4 animate-spin" />
              Checking…
            </p>
          ) : running.length === 0 ? (
            <p className="mt-2 text-sm text-muted-foreground">
              Nothing is running right now. Press Open to start it.
            </p>
          ) : (
            <ul className="mt-2 flex flex-col gap-1">
              {running.map((proc) => (
                <li
                  key={proc.id}
                  data-testid={`project-process-${proc.id}`}
                  className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-md border border-border px-3 py-2 text-sm"
                >
                  <span
                    aria-hidden="true"
                    className={`h-2 w-2 shrink-0 rounded-full ${
                      proc.portHealthy === false ? DOT.amber : DOT.green
                    }`}
                  />
                  <span className="font-medium">{proc.name}</span>
                  {proc.healthPort ? (
                    <span className="text-muted-foreground">
                      http://localhost:{proc.healthPort}
                    </span>
                  ) : null}
                  <span className="flex-1" />
                  <button
                    type="button"
                    disabled={busyProcess === proc.id}
                    onClick={() => void runProcessAction("stop_managed_process", proc.id)}
                    className="rounded border border-border px-2 py-0.5 text-xs font-medium hover:bg-accent disabled:opacity-60"
                  >
                    Stop
                  </button>
                  <button
                    type="button"
                    disabled={busyProcess === proc.id}
                    onClick={() => void runProcessAction("restart_managed_process", proc.id)}
                    className="rounded border border-border px-2 py-0.5 text-xs font-medium hover:bg-accent disabled:opacity-60"
                  >
                    Restart
                  </button>
                </li>
              ))}
            </ul>
          )}
          {actionError ? (
            <p className="mt-1 text-xs text-red-600 dark:text-red-400">{actionError}</p>
          ) : null}
        </section>

        <section>
          <h2 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            Recent work
          </h2>
          {recent.length === 0 ? (
            <p className="mt-2 text-sm text-muted-foreground">Nothing yet.</p>
          ) : (
            <ul className="mt-2 flex flex-col gap-1 text-sm">
              {recent.slice(0, 8).map((s) => (
                <li key={s.id} className="flex flex-wrap items-baseline gap-x-3">
                  <span className="min-w-0 flex-1 truncate">{s.name || "Untitled session"}</span>
                  <span className="text-muted-foreground">{describeSessionWhen(s)}</span>
                </li>
              ))}
            </ul>
          )}
        </section>

        {project.notes ? (
          <section>
            <h2 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
              Notes
            </h2>
            <p className="mt-2 whitespace-pre-wrap text-sm">{project.notes}</p>
          </section>
        ) : null}
      </div>
    </div>
  );
}
