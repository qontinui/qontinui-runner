/**
 * What "Suggest groups" clusters on, per Live row (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * D6, wired in Phase 3).
 *
 * - **Plan slug, repo and branch from the fleet.** coord's fleet listing
 *   carries `workUnitSlug` / `repo` / `branch` per session, joined here on
 *   the HARNESS session id (`claudeCodeSessionId`, never the `coord.sessions`
 *   row id). One page is read when the dialog opens; a session the page does
 *   not carry simply has no fleet keys.
 * - **Repo from the working dir** when the fleet gave none: the last path
 *   segment of the row's project path, which is the repo directory for a
 *   checkout and for an agent worktree (`agent-worktrees/<id>/<repo>`) alike.
 *
 * A missing key is absent, never guessed: the suggester drops a session with
 * no key at all.
 */

import { invoke } from "@tauri-apps/api/core";

import { describeThrown } from "@/lib/utils";

import type { SuggestionSession } from "./sessionGroupSuggestions";
import type { FleetSessionsResponse } from "./useFleetSessions";

/** The fleet keys of one session. */
export interface FleetSuggestionFacts {
  planSlug: string | null;
  repo: string | null;
  branch: string | null;
}

/** The last segment of a working dir, or null for an empty one. */
export function repoFromWorkingDir(path: string | null | undefined): string | null {
  const segments = (path ?? "").replace(/\\/g, "/").split("/").filter(Boolean);
  return segments.length > 0 ? segments[segments.length - 1] : null;
}

export interface SuggestionRow {
  sessionKey: string;
  title: string;
  /** The Claude session id the fleet joins on; `null` = none. */
  claudeSessionId: string | null;
  workingDir: string;
}

/** One suggester input per row: fleet keys first, else repo from the working dir. */
export function buildSuggestionSessions(
  rows: readonly SuggestionRow[],
  fleet: ReadonlyMap<string, FleetSuggestionFacts>,
): SuggestionSession[] {
  return rows.map((row) => {
    const f = row.claudeSessionId ? fleet.get(row.claudeSessionId) : undefined;
    return {
      sessionKey: row.sessionKey,
      title: row.title,
      planSlug: f?.planSlug ?? null,
      repo: f?.repo ?? repoFromWorkingDir(row.workingDir),
      branch: f?.branch ?? null,
    };
  });
}

/** Index a fleet page's THIS-device rows by harness session id. */
export function fleetFactsBySessionId(
  response: Pick<FleetSessionsResponse, "sessions">,
): Map<string, FleetSuggestionFacts> {
  const out = new Map<string, FleetSuggestionFacts>();
  for (const s of response.sessions) {
    if (!s.isCallerDevice || !s.claudeCodeSessionId) continue;
    out.set(s.claudeCodeSessionId, {
      planSlug: s.workUnitSlug,
      repo: s.repo,
      branch: s.branch,
    });
  }
  return out;
}

/** Page size of the one fleet read the dialog makes. */
export const SUGGESTION_FLEET_LIMIT = 200;

/**
 * Read one fleet page for the suggester. A failed read answers an empty map
 * plus the reason, so the dialog still suggests by repo and says what it
 * could not use.
 */
export async function loadFleetSuggestionFacts(): Promise<{
  facts: Map<string, FleetSuggestionFacts>;
  unavailable: string | null;
}> {
  try {
    const response = await invoke<FleetSessionsResponse>("fleet_sessions_list", {
      args: {
        deviceId: null,
        state: null,
        includeClosed: false,
        limit: SUGGESTION_FLEET_LIMIT,
        cursor: null,
      },
    });
    return { facts: fleetFactsBySessionId(response), unavailable: null };
  } catch (e) {
    return {
      facts: new Map(),
      unavailable: `fleet plan/branch keys unavailable: ${describeThrown(e, "fleet_sessions_list failed")}`,
    };
  }
}
