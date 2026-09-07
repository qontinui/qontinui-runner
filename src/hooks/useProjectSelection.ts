/**
 * useProjectSelection
 *
 * Hook for managing project selection state across the application.
 * The selected project is shared between Connection settings and Capture settings.
 * It persists the selection to localStorage and emits events for StatusIndicator.
 */

import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Project } from "../types/auth";
import { instanceStorage } from "@/lib/instance-storage";
import { createLogger } from "@/lib/logger";

const log = createLogger("useProjectSelection");

const SELECTED_PROJECT_STORAGE_KEY = "qontinui-selected-project";

/**
 * The bearer reason codes that mean "this runner has no usable cloud session"
 * rather than "something is broken".
 *
 * `commands::auth::BearerReason` (src-tauri) names four states behind a failed
 * `get_user_projects`; three of them are ordinary and one is a fault:
 *
 * | code | state | ordinary? |
 * |---|---|---|
 * | `no_cognito_session` | never signed in, or signed out | yes |
 * | `cognito_refresh_token_expired` | refresh token revoked — needs an operator sign-in | yes |
 * | `cognito_refresh_failed_no_stored_token` | Cognito outage; self-recovers | yes |
 * | `cognito_access_token_unreadable` | the credential store yielded nothing | **no — a real fault** |
 *
 * `cognito_access_token_unreadable` is deliberately ABSENT from this list. It
 * is the one code that says the store itself misbehaved, and it is exactly what
 * the console-error channel should keep surfacing.
 */
const NO_CLOUD_SESSION_REASON_CODES = [
  "no_cognito_session",
  "cognito_refresh_token_expired",
  "cognito_refresh_failed_no_stored_token",
] as const;

/**
 * Is this `get_user_projects` failure the expected "no cloud session" state?
 *
 * Exported and pure so it can be tested: the console-error channel is consumed
 * as a HEALTH SIGNAL (`/ui-bridge/control/console-errors`), so a
 * mis-classification here does not merely log the wrong severity — it reports a
 * healthy runner as unhealthy. That is the regression the tier-gate arm of this
 * predicate was already written to prevent; this adds the arm it was missing.
 *
 * Three sources of "no cloud session", matched on stable substrings:
 *  1. The tier gate (`require_tier_2_for`) — a Tier 0/1 runner has no account.
 *  2. `"Not authenticated"` — the keychain race the retry loop above gives up on.
 *  3. A bearer reason code — see [`NO_CLOUD_SESSION_REASON_CODES`]. These ride
 *     inside the message `commands::auth::no_bearer_error` renders, e.g.
 *     `Not signed in to Qontinui (no_cognito_session). Sign in via Settings → Account.`
 *
 * Arm 3 is new. Before it, EVERY no-bearer refusal took the `console.error`
 * branch — including the plain not-signed-in case, which is the single most
 * common steady state for a runner with no account. The codes only became
 * matchable when PR #1342 put them in the message and #1379 (`e81e75ddc`) gave
 * them one definition; this is the consumer that makes them earn their keep.
 */
export function isExpectedNoCloudSession(errorMsg: string): boolean {
  return (
    // Matched on the canonical tier-gate sentence (stable across the
    // Tier 0 / Tier 1 wording) plus the auth-race message the retry loop
    // gives up on — both mean "no cloud session", not "broken".
    errorMsg.includes("Qontinui account commands are unavailable") ||
    errorMsg.includes("Not authenticated") ||
    NO_CLOUD_SESSION_REASON_CODES.some((code) => errorMsg.includes(code))
  );
}

/**
 * The one bearer reason code that means "try again in a moment".
 *
 * `RefreshClass::Ok` is the refresher saying *no refresh was needed, or the
 * refresh succeeded — healthy*, and `bearer_reason_code` only reaches
 * [`BearerReason::from_class`] once the bearer has already been found
 * absent-or-blank. So `Ok` + no bearer is precisely the state the retry below
 * was written for: the credential store was written but the read that raced it
 * came back empty. Every other code is a settled answer that a 1–3 s wait
 * cannot change — no session, a revoked refresh token, or a Cognito outage
 * whose own refresher backs off for 15 s minimum.
 */
const RETRYABLE_CREDENTIAL_RACE_REASON_CODE = "cognito_access_token_unreadable";

/**
 * Is this `get_user_projects` failure the keychain-write/read ordering blip
 * that the load below retries through?
 *
 * This gate used to be `errorMsg.includes("Not authenticated")`, and that
 * string is no longer reachable from `get_user_projects`. That command has
 * exactly two auth refusals — the tier gate's "Qontinui account commands are
 * unavailable", and `commands::auth::no_bearer_error`'s
 * `Not signed in to Qontinui (<code>). Sign in via Settings → Account.` —
 * and neither contains it. PR #1342 replaced the bare "Not authenticated"
 * refusal with the reason-coded one and #1379 (`e81e75ddc`) gave the codes one
 * definition; the severity classifier above was re-pointed at them by #1389
 * and THIS gate was not, so the retry stopped firing for the race it exists to
 * cover while two comments — its own, and App.tsx's project-fetch effect —
 * went on asserting that it still did.
 *
 * Deliberately NOT `isExpectedNoCloudSession`'s list: those three codes are the
 * ordinary steady states, and retrying them would spend 6 s of 1 s/2 s/3 s
 * backoff on every project load for a runner that is simply not signed in —
 * the single most common configuration, and the one #1389 just stopped
 * reporting as unhealthy. The two predicates are disjoint by construction, and
 * a test pins that.
 *
 * What this deliberately does NOT cover, stated because it is the honest limit
 * of the re-pointing rather than an oversight: a race that loses the REFRESH
 * TOKEN read too surfaces as `RefreshClass::NoSession` -> `no_cognito_session`,
 * which is byte-identical to the ordinary not-signed-in steady state. Nothing
 * in the message distinguishes them, so no gate here can retry one without
 * retrying the other. That window is held off upstream instead, by App.tsx's
 * `devAutoLoginPending` gate on the project-fetch effect.
 *
 * Dropping the `"Not authenticated"` arm also drops an accident: the only way
 * that substring can still reach here is inside an `HTTP <status>: <body>`
 * render of a web-backend response (`AppError::HttpStatusError` interpolates
 * the body verbatim). Retrying a definitive 4xx three times is the opposite of
 * what that variant documents itself for — it "carries the status code and
 * response body so callers can distinguish retryable (5xx, 429) from
 * definitive (4xx) failures without parsing".
 */
export function isRetryableCredentialRace(errorMsg: string): boolean {
  return errorMsg.includes(RETRYABLE_CREDENTIAL_RACE_REASON_CODE);
}

export interface ProjectSelectionState {
  selectedProjectId: string | null;
  selectedProjectName: string | null;
}

interface UseProjectSelectionReturn {
  projects: Project[];
  selectedProjectId: string | null;
  selectedProjectName: string | null;
  loading: boolean;
  error: string | null;
  setSelectedProject: (projectId: string | null) => void;
  loadProjects: () => Promise<void>;
}

/**
 * Hook to manage project selection across the application.
 * Projects are fetched from the backend and the selection is persisted.
 */
export function useProjectSelection(): UseProjectSelectionReturn {
  const [projects, setProjects] = useState<Project[]>([]);
  const [selectedProjectId, setSelectedProjectIdState] = useState<string | null>(() => {
    // Load from instanceStorage on mount
    const parsed = instanceStorage.getJSON<ProjectSelectionState | null>(
      SELECTED_PROJECT_STORAGE_KEY,
      null,
    );
    return parsed?.selectedProjectId ?? null;
  });
  const [selectedProjectName, setSelectedProjectName] = useState<string | null>(() => {
    const parsed = instanceStorage.getJSON<ProjectSelectionState | null>(
      SELECTED_PROJECT_STORAGE_KEY,
      null,
    );
    return parsed?.selectedProjectName ?? null;
  });
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /**
   * Set the selected project and persist to localStorage
   */
  const setSelectedProject = (projectId: string | null) => {
    setSelectedProjectIdState(projectId);

    // Find the project name
    const project = projects.find((p) => p.id === projectId);
    const projectName = project?.name || null;
    setSelectedProjectName(projectName);

    // Persist to localStorage
    const state: ProjectSelectionState = {
      selectedProjectId: projectId,
      selectedProjectName: projectName,
    };
    instanceStorage.setJSON(SELECTED_PROJECT_STORAGE_KEY, state);

    // Dispatch event for StatusIndicator
    window.dispatchEvent(
      new CustomEvent("project-selection-changed", {
        detail: { projectId, projectName },
      }),
    );

    log.debug("Selected project:", projectId, projectName);
  };

  /**
   * Load projects from the backend.
   *
   * Retries on the credential-store read race only — see
   * [`isRetryableCredentialRace`]. Failures are classified for severity by
   * [`isExpectedNoCloudSession`].
   */
  const loadProjects = useCallback(async () => {
    setLoading(true);
    setError(null);

    const attemptLoad = async (retryCount = 0): Promise<Project[]> => {
      try {
        return await invoke<Project[]>("get_user_projects");
      } catch (err) {
        const errorMsg = err instanceof Error ? err.message : String(err);
        // Retry up to 3 times on the credential-store read race. 500ms was
        // insufficient when the temp runner's auto-login HTTP roundtrip is
        // still in flight when App.tsx's auth-gated effect fires (iter 4
        // manual test); the in-flight auto-login half of that is now held off
        // by App.tsx's `devAutoLoginPending` gate, leaving this to cover the
        // keychain-write/read ordering blip alone.
        //
        // See [`isRetryableCredentialRace`], which owns the gate and is
        // unit-tested. It replaces a `"Not authenticated"` substring match that
        // `get_user_projects` can no longer produce.
        const MAX_AUTH_RETRIES = 3;
        if (retryCount < MAX_AUTH_RETRIES && isRetryableCredentialRace(errorMsg)) {
          const delay = 1000 * (retryCount + 1);
          log.debug(
            `Credential-store read race, retry ${retryCount + 1}/${MAX_AUTH_RETRIES} in ${delay}ms...`,
          );
          await new Promise((resolve) => setTimeout(resolve, delay));
          return attemptLoad(retryCount + 1);
        }
        throw err;
      }
    };

    try {
      const projectList = await attemptLoad();
      setProjects(projectList);
      log.debug("Loaded", projectList.length, "projects");

      // If we have a stored selection, validate it still exists
      if (selectedProjectId) {
        const projectStillExists = projectList.some((p) => p.id === selectedProjectId);
        if (!projectStillExists && projectList.length > 0) {
          // Auto-select first project if stored project no longer exists

          setSelectedProject(projectList[0].id);
        } else if (projectStillExists) {
          // Update name in case it changed
          const project = projectList.find((p) => p.id === selectedProjectId);
          if (project) {
            setSelectedProjectName(project.name);
          }
        }
      } else if (projectList.length > 0) {
        // Auto-select first project if none selected
        setSelectedProject(projectList[0].id);
      }
    } catch (err) {
      const errorMsg = err instanceof Error ? err.message : String(err);

      // A Tier 0/1 (Local / LocalProvider) runner has no Qontinui account, so
      // `get_user_projects` is gated off by design: the backend's
      // `require_tier_2_for` (src-tauri/src/commands/auth.rs) rejects every
      // account command with the canonical "Qontinui account commands are
      // unavailable" AuthError. That is the EXPECTED steady state for local
      // mode, not a fault. Reporting it via console.error poisoned the
      // console-error channel used as a health signal
      // (`/ui-bridge/control/console-errors`), so a perfectly healthy Tier 0/1
      // runner always reported errors. Log it at info; keep console.error for
      // genuine load failures.
      //
      // See [`isExpectedNoCloudSession`], which owns the classification and is
      // unit-tested. It also covers the bearer reason codes, which this arm
      // used to miss entirely — so a plain not-signed-in runner reported a
      // console error on every load.
      const isExpectedLocalMode = isExpectedNoCloudSession(errorMsg);

      if (isExpectedLocalMode) {
        log.info("No Qontinui account session (Tier 0/1 local mode) — skipping cloud project load");
      } else {
        console.error("[PROJECT_SELECTION] Failed to load projects:", err);
      }

      setError(errorMsg);
    } finally {
      setLoading(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- setSelectedProject is stable
  }, [selectedProjectId]);

  // When projects change, update the selected project name if needed.
  // Defer setState off the effect body via microtask so we don't cascade
  // renders during the same commit (set-state-in-effect).
  useEffect(() => {
    if (!selectedProjectId || projects.length === 0) return;
    const project = projects.find((p) => p.id === selectedProjectId);
    if (!project || project.name === selectedProjectName) return;
    let cancelled = false;
    queueMicrotask(() => {
      if (cancelled) return;
      setSelectedProjectName(project.name);
      const state: ProjectSelectionState = {
        selectedProjectId,
        selectedProjectName: project.name,
      };
      instanceStorage.setJSON(SELECTED_PROJECT_STORAGE_KEY, state);
    });
    return () => {
      cancelled = true;
    };
  }, [projects, selectedProjectId, selectedProjectName]);

  return {
    projects,
    selectedProjectId,
    selectedProjectName,
    loading,
    error,
    setSelectedProject,
    loadProjects,
  };
}
