/**
 * `useProjectPageActivation` — turn "a project was activated" into the terminal
 * state that project expects (projects-dashboard plan §7.2 steps 1–3).
 *
 * Everything it needs already existed and was simply never called from
 * anywhere: `resolveProjectPage` / `ensureProjectPage` (step 1, page binding
 * with dangling-id tolerance), `TerminalPageConfig.defaultWorkingDir` +
 * `resolveSpawnWorkingDir` (step 2, so every FUTURE terminal on the page opens
 * at the project root), and `useZoneProfileRestore` (step 3). This hook is the
 * missing caller.
 *
 * It is mounted in `App.tsx`, beside `useTerminalPages()`, because that is
 * where the page roster lives — the Projects page is a sibling tab and must
 * not reach into the terminal tree (`activeProject.ts` states that contract).
 * Activation therefore travels as the `project-activated` window event, the
 * same idiom as `terminal-open-page` and `navigate-to-active`.
 *
 * Step 4 ("never re-`cd` a live shell") is deliberately NOT here: it belongs
 * to `useProjectTerminalReconcile` / `ProjectFolderChip`, which follow the same
 * event and offer the operator [Move them] / [Leave them]. Nothing in this file
 * touches a running terminal.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { createLogger } from "@/lib/logger";
import { subscribeActiveProject, type ActiveProjectHint } from "./activeProject";
import { cancelZoneProfileRestore, requestZoneProfileRestore } from "./useZoneProfileRestore";

const log = createLogger("useProjectPageActivation");

/** What an activation asks the terminal surface to do. */
export interface ProjectActivationPlan {
  /** `SavedProject.id` — the key the resolved binding is persisted under. */
  projectId: string;
  /** Page name to use when a page has to be created. Never blank. */
  projectName: string;
  /** The project root; becomes the page's `defaultWorkingDir`. */
  projectRoot?: string;
  /** The page id we believe is PERSISTED for this project, if any. */
  boundPageId?: string;
  /** Zone profile name to reinstate, if the project names one. */
  zoneProfile?: string;
}

/** Trim + treat blank as absent — the same convention `useTerminalPages` uses. */
function cleanOptional(v: string | null | undefined): string | undefined {
  const t = v?.trim();
  return t ? t : undefined;
}

/**
 * Pure: read an activation hint into a plan, or `null` when there is nothing
 * to bind.
 *
 * `sessionBindings` is the id this session already resolved for a project. It
 * exists because the hint is built from the Projects page's IN-MEMORY project
 * list, which does not re-read `settings.json` after we persist a freshly
 * minted binding: without this fallback, a second "Work on it" click on a
 * project that had no page would mint a SECOND page. The hint still wins when
 * it carries an id, because a populated `terminalPageId` came from an actual
 * `list_saved_projects` read and is therefore the authoritative persisted
 * value.
 *
 * Returns `null` for a deactivation (`hint === null`) and for a hint with no
 * `id`: the id is what the resolved binding gets persisted under, so without
 * it there is nothing durable to bind and minting a page would leak an
 * unreachable tab on every click.
 *
 * Exported so the precedence rules are unit-testable without React or Tauri.
 */
export function planProjectActivation(
  hint: ActiveProjectHint | null,
  sessionBindings: ReadonlyMap<string, string>,
): ProjectActivationPlan | null {
  const projectId = cleanOptional(hint?.id);
  if (!hint || !projectId) return null;
  return {
    projectId,
    projectName: cleanOptional(hint.name) ?? "Project",
    projectRoot: cleanOptional(hint.path),
    boundPageId: cleanOptional(hint.terminalPageId) ?? sessionBindings.get(projectId),
    zoneProfile: cleanOptional(hint.zoneProfile),
  };
}

/**
 * Pure: does the resolved page id need writing back to `settings.json`?
 *
 * Only when it differs from what the project record already holds. A DANGLING
 * binding resolves back to its own id (`resolveProjectPage` re-creates the
 * page under it rather than minting a fresh one), so the common
 * cleared-localStorage case correctly writes nothing.
 */
export function shouldPersistBinding(plan: ProjectActivationPlan, resolvedPageId: string): boolean {
  return !!resolvedPageId && resolvedPageId !== plan.boundPageId;
}

export interface ProjectPageActivationDeps {
  /** `useTerminalPages().activePageId` — what the page switch has settled on. */
  activePageId: string;
  /** `useTerminalPages().ensureProjectPage`. */
  ensureProjectPage: (opts: {
    boundPageId?: string | null;
    projectName: string;
    defaultWorkingDir?: string | null;
  }) => { pageId: string; created: boolean };
}

export function useProjectPageActivation({
  activePageId,
  ensureProjectPage,
}: ProjectPageActivationDeps): void {
  // `projectId → pageId` resolved during THIS session. See
  // `planProjectActivation` for why the in-memory hint is not enough.
  const sessionBindings = useRef<Map<string, string>>(new Map());

  // A zone-profile restore waiting for its page to become active. It cannot be
  // fired inline: `useApplyZoneProfile` writes the layout of whatever page is
  // ACTIVE at call time, so restoring before the switch lands would reshape the
  // page the operator is leaving.
  const [pendingProfile, setPendingProfile] = useState<{
    pageId: string;
    profileName: string;
  } | null>(null);

  // `ensureProjectPage` is a `useCallback` in a parent that re-renders often;
  // reading it through a ref keeps the subscription registered exactly once,
  // so an activation can never be dropped during a re-subscribe window.
  const latestEnsure = useRef(ensureProjectPage);
  useEffect(() => {
    latestEnsure.current = ensureProjectPage;
  }, [ensureProjectPage]);

  const activate = useCallback((hint: ActiveProjectHint | null) => {
    const plan = planProjectActivation(hint, sessionBindings.current);
    if (!plan) {
      // Deactivation (or a hint with no id): nothing to bind, and any queued
      // restore from the previous project must not survive it.
      cancelZoneProfileRestore();
      setPendingProfile(null);
      return;
    }

    // Steps 1 + 2 in one call: resolve (creating on a dangling/absent id) the
    // project's page, write the project root as its `defaultWorkingDir`, and
    // make it active. Never throws — an activation must not be failable.
    const { pageId } = latestEnsure.current({
      boundPageId: plan.boundPageId,
      projectName: plan.projectName,
      defaultWorkingDir: plan.projectRoot,
    });
    sessionBindings.current.set(plan.projectId, pageId);

    if (shouldPersistBinding(plan, pageId)) {
      // Fire-and-forget: the binding is an optimization (it keeps the project
      // on the same page across restarts), not a precondition for anything we
      // just did. A failed write costs a re-minted page next boot, so it is
      // worth logging and not worth blocking on.
      void invoke("set_project_terminal_page", { id: plan.projectId, pageId }).catch((err) => {
        log.warn(
          "Could not persist the project → terminal page binding",
          `${plan.projectId}: ${err instanceof Error ? err.message : String(err)}`,
        );
      });
    }

    // Step 3 — armed here, fired by the effect below once `activePageId` has
    // caught up.
    if (plan.zoneProfile) {
      setPendingProfile({ pageId, profileName: plan.zoneProfile });
    } else {
      cancelZoneProfileRestore();
      setPendingProfile(null);
    }
  }, []);

  useEffect(() => subscribeActiveProject(activate), [activate]);

  useEffect(() => {
    if (!pendingProfile) return;
    // Wait for the page switch to land: `requestZoneProfileRestore` is applied
    // to the ACTIVE page's zone layout.
    if (activePageId !== pendingProfile.pageId) return;
    requestZoneProfileRestore(pendingProfile);
    // A restore is one-shot: disarming it is what stops the NEXT
    // `activePageId` change from re-applying the same profile. The cascading
    // render this costs is one, and it converges immediately (the guard above
    // returns on the re-run) — the same exception `useTerminalPages`'
    // active-page normalization takes.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setPendingProfile(null);
  }, [activePageId, pendingProfile]);
}
