/**
 * The `Open` state machine (plan §7.1) — effectful half.
 *
 * Runs the three steps §7.1 asks for:
 *
 *   1. **Start what it needs.** For each bound process that is not already
 *      live, `start_managed_process`. That command does not return until the
 *      process is READY or has died — `ProcessCaptureManager::await_ready`
 *      polls `health_port` where one is configured and otherwise waits out a
 *      post-spawn settle window, and on an early exit it returns the captured
 *      stderr tail as the error. So step 2 of §7.1 ("wait for ready") needs no
 *      separate poll here; it is inside the call we are already awaiting.
 *   2. **Show progress while that blocks.** A parallel 1.5s poll of
 *      `get_process_output` keeps the last stderr lines under the prose, which
 *      is the whole reason §7.1 says prose and not a spinner.
 *   3. **Show it.** `open_project_preview` points the per-project Preview
 *      webview window at the front page.
 *
 * The decisions live in `openProject.ts` (pure, unit-tested); this file is the
 * IPC plumbing and the React state around it.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { createLogger } from "@/lib/logger";
import type { SavedProject } from "@/hooks/useSavedProjects";
import type { ProjectSnapshot } from "./types";
import { describeStartFailure, planOpen, stderrTailOf, type OpenPhase } from "./openProject";

const log = createLogger("useProjectOpen");

/** Cadence of the stderr tail poll that runs alongside a blocking start. */
const STDERR_POLL_MS = 1500;
/** How many output lines to pull per poll — matches `ProcessManagerTab`. */
const STDERR_PULL_LINES = 80;

interface OutputLine {
  stream: string;
  line: string;
}

export interface UseProjectOpen {
  /** Phase per project id. Absent means idle. */
  phases: Record<string, OpenPhase>;
  /** Run the Open sequence for a project. Safe to call twice — the second call is ignored while the first is in flight. */
  open: (project: SavedProject, snapshot?: ProjectSnapshot) => Promise<void>;
  /** Clear a terminal (done/failed) phase — the card's "dismiss". */
  dismiss: (projectId: string) => void;
}

export function useProjectOpen(): UseProjectOpen {
  const [phases, setPhases] = useState<Record<string, OpenPhase>>({});
  /**
   * Ids with a run in flight. A ref, not state: the guard has to be readable
   * synchronously inside `open` — a state read there would see the value from
   * the render that created the callback and would let a double-click start
   * the same process twice.
   */
  const inFlight = useRef<Set<string>>(new Set());
  /** Set on unmount so no in-flight run writes state into a dead component. */
  const unmounted = useRef(false);

  useEffect(() => {
    unmounted.current = false;
    return () => {
      unmounted.current = true;
    };
  }, []);

  const setPhase = useCallback((projectId: string, phase: OpenPhase) => {
    if (unmounted.current) return;
    setPhases((prev) => ({ ...prev, [projectId]: phase }));
  }, []);

  const dismiss = useCallback((projectId: string) => {
    setPhases((prev) => {
      if (!(projectId in prev)) return prev;
      const next = { ...prev };
      delete next[projectId];
      return next;
    });
  }, []);

  const open = useCallback(
    async (project: SavedProject, snapshot?: ProjectSnapshot) => {
      if (inFlight.current.has(project.id)) return;

      const plan = planOpen({
        frontPageUrl: project.frontPageUrl,
        processIds: project.processIds ?? [],
        snapshot,
      });

      if (plan.blockedReason || !plan.url) {
        setPhase(project.id, {
          kind: "failed",
          message: plan.blockedReason ?? "This project doesn't have an address to open yet.",
          needsFrontPage: plan.needsFrontPage,
        });
        return;
      }

      inFlight.current.add(project.id);
      try {
        for (const [index, target] of plan.toStart.entries()) {
          setPhase(project.id, {
            kind: "starting",
            processId: target.id,
            processName: target.name,
            step: index + 1,
            total: plan.toStart.length,
          });

          // Tail stderr WHILE the start blocks. `start_managed_process` does
          // not resolve until ready-or-dead, so without this the user watches
          // a frozen line for however long the build takes.
          const stopTail = pollStderrTail(target.id, (tail) => {
            if (!tail) return;
            setPhase(project.id, {
              kind: "starting",
              processId: target.id,
              processName: target.name,
              step: index + 1,
              total: plan.toStart.length,
              stderrTail: tail,
            });
          });

          try {
            await invoke("start_managed_process", { id: target.id });
          } catch (err) {
            log.error(`start_managed_process(${target.id}) failed`, String(err));
            setPhase(project.id, describeStartFailure(target.name, err));
            return;
          } finally {
            stopTail();
          }
        }

        setPhase(project.id, { kind: "opening", url: plan.url });
        try {
          await invoke<boolean>("open_project_preview", {
            projectId: project.id,
            url: plan.url,
            title: project.name,
          });
        } catch (err) {
          const raw = err instanceof Error ? err.message : String(err);
          log.error("open_project_preview failed", raw);
          setPhase(project.id, {
            kind: "failed",
            // Everything the site needs IS running at this point — only the
            // window failed — so the message says that rather than implying
            // the project is broken.
            message: `${project.name} is running, but the preview window would not open.`,
            detail: raw.trim() || undefined,
          });
          return;
        }

        setPhase(project.id, { kind: "done", url: plan.url });
      } finally {
        inFlight.current.delete(project.id);
      }
    },
    [setPhase],
  );

  return { phases, open, dismiss };
}

/**
 * Poll a process's stderr tail until the returned stop function is called.
 *
 * Best-effort by design: a transient IPC error leaves the previous tail in
 * place rather than blanking it, the same posture `ProcessManagerTab` takes.
 * The poll is fire-and-forget — it must never reject into the caller's
 * `await`, which is the one thing that could turn a cosmetic failure into a
 * failed Open.
 */
function pollStderrTail(processId: string, onTail: (tail: string) => void): () => void {
  let stopped = false;

  const tick = async () => {
    if (stopped) return;
    try {
      const lines = await invoke<OutputLine[]>("get_process_output", {
        id: processId,
        tail: STDERR_PULL_LINES,
      });
      if (!stopped) onTail(stderrTailOf(lines));
    } catch {
      // Ignore — see the doc comment.
    }
  };

  void tick();
  const timer = setInterval(() => void tick(), STDERR_POLL_MS);
  return () => {
    stopped = true;
    clearInterval(timer);
  };
}
