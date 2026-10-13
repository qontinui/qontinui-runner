/**
 * `useSessionFacts` — hydrates the Session Manager query engine's
 * `SessionFacts` for every row (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 4).
 *
 * One refresh is ONE `session_info_list` call for every row's Claude session
 * id (never one `session_info_get` per row), then ONE `coord_pr_ci_states`
 * call for the PRs the runner's own ledger has not already settled. Both
 * re-run when the set of ids changes and every `pollMs`.
 *
 * Every failure is a stated state, never an empty one: a failed listing is
 * `{ phase: "unavailable", reason }`, under which every info-derived fact is
 * UNKNOWN; a failed CI read leaves `prCi` as it was (or `null`), under which
 * PR CI is UNKNOWN.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { describeThrown } from "@/lib/utils";

import {
  buildSessionFacts,
  prCiKey,
  prsNeedingCi,
  type PrCiRead,
  type SessionFacts,
  type SessionFactsSources,
  type SessionInfoListView,
  type WorkingDirBranch,
} from "./sessionFacts";
import { normalizeSessionInfo, type SessionInfoState } from "./useSessionInfo";

/** One row's inputs, minus the shared sources the hook supplies. */
export type SessionFactsRowInput = Omit<SessionFactsSources, "info" | "prCi" | "org">;

export interface UseSessionFactsParams {
  rows: readonly SessionFactsRowInput[];
  org: SessionFactsSources["org"];
  /** Re-read interval; default 30 s. */
  pollMs?: number;
}

export interface SessionFactsState {
  /** Keyed by `row.sessionId`. */
  facts: ReadonlyMap<string, SessionFacts>;
  info: SessionInfoListView;
  /** `null` until the first CI read answers. */
  prCi: ReadonlyMap<string, PrCiRead> | null;
  refresh: () => void;
}

/** Normalize the `session_info_list` response. Pure. */
export function normalizeInfoList(raw: unknown): SessionInfoListView {
  if (!raw || typeof raw !== "object") return { phase: "unavailable", reason: "malformed_listing" };
  const v = raw as {
    status?: unknown;
    reason?: unknown;
    sessions?: unknown;
    notFound?: unknown;
    branches?: unknown;
  };
  if (v.status !== "ok") {
    return {
      phase: "unavailable",
      reason: typeof v.reason === "string" && v.reason ? v.reason : "unspecified",
    };
  }
  if (!Array.isArray(v.sessions)) return { phase: "unavailable", reason: "malformed_listing" };
  const byId = new Map<string, SessionInfoState>();
  for (const env of v.sessions) {
    const state = normalizeSessionInfo(env);
    const id = state.body?.identity.claudeSessionId;
    if (id) byId.set(id, state);
  }
  const notFound = new Set(
    Array.isArray(v.notFound) ? v.notFound.filter((x): x is string => typeof x === "string") : [],
  );
  const branches = new Map<string, WorkingDirBranch>();
  if (Array.isArray(v.branches)) {
    for (const b of v.branches as WorkingDirBranch[]) {
      if (b && typeof b.workingDir === "string") branches.set(b.workingDir, b);
    }
  }
  return { phase: "ok", byId, notFound, branches };
}

/** Index a `coord_pr_ci_states` response by {@link prCiKey}. Pure. */
export function indexPrCi(raw: unknown): Map<string, PrCiRead> {
  const out = new Map<string, PrCiRead>();
  const prs = (raw as { prs?: unknown } | null)?.prs;
  if (!Array.isArray(prs)) return out;
  for (const r of prs as PrCiRead[]) {
    if (r && typeof r.repo === "string" && typeof r.number === "number") {
      out.set(prCiKey(r.repo, r.number), r);
    }
  }
  return out;
}

/** The thin client for `coord_pr_ci_states`. */
export async function fetchPrCiStates(
  prs: readonly { repo: string; number: number }[],
): Promise<Map<string, PrCiRead>> {
  return indexPrCi(await invoke<unknown>("coord_pr_ci_states", { prs }));
}

/** The thin client for `session_info_list`. */
export async function fetchSessionInfoList(ids: readonly string[]): Promise<SessionInfoListView> {
  return normalizeInfoList(await invoke<unknown>("session_info_list", { ids }));
}

export function useSessionFacts({
  rows,
  org,
  pollMs = 30_000,
}: UseSessionFactsParams): SessionFactsState {
  const [info, setInfo] = useState<SessionInfoListView>({ phase: "loading" });
  const [prCi, setPrCi] = useState<ReadonlyMap<string, PrCiRead> | null>(null);
  const seq = useRef(0);

  const idsKey = useMemo(() => {
    const ids = new Set<string>();
    for (const r of rows) if (r.claudeSessionId) ids.add(r.claudeSessionId);
    return [...ids].sort().join("\n");
  }, [rows]);

  const refresh = useCallback(() => {
    const mine = ++seq.current;
    const ids = idsKey === "" ? [] : idsKey.split("\n");
    void (async () => {
      let next: SessionInfoListView;
      try {
        next = await fetchSessionInfoList(ids);
      } catch (e) {
        next = {
          phase: "unavailable",
          reason: `ipc_error: ${describeThrown(e, "session_info_list failed")}`,
        };
      }
      if (mine !== seq.current) return;
      setInfo(next);
      const wanted = prsNeedingCi(next);
      if (wanted.length === 0) {
        setPrCi(new Map());
        return;
      }
      try {
        const ci = await fetchPrCiStates(wanted);
        if (mine === seq.current) setPrCi(ci);
      } catch (e) {
        console.error("coord_pr_ci_states failed:", describeThrown(e, "unknown error"));
      }
    })();
  }, [idsKey]);

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, pollMs);
    return () => clearInterval(t);
  }, [refresh, pollMs]);

  const facts = useMemo(() => {
    const out = new Map<string, SessionFacts>();
    for (const r of rows) out.set(r.row.sessionId, buildSessionFacts({ ...r, org, info, prCi }));
    return out;
  }, [rows, org, info, prCi]);

  return { facts, info, prCi, refresh };
}
