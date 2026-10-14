/**
 * Hydration of the Session Manager's `SessionFacts` (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phases 1 and 4), in two hooks because the listing feeds the status model
 * BEFORE the facts are built:
 *
 *  - {@link useSessionInfoList}: ONE `session_info_list` call per refresh for
 *    every row's Claude session id (never one `session_info_get` per row).
 *    `useSessionManager` reads each live row's finished marker from it
 *    (`finishedMarker`) and hands the same listing to the facts.
 *  - {@link useSessionFacts}: ONE `coord_pr_ci_states` call for the PRs the
 *    runner's own ledger has not already settled, then every row's facts.
 *
 * Every failure is a stated state, never an empty one: a failed listing is
 * `{ phase: "unavailable", reason }`, under which every info-derived fact —
 * the finished marker included — is UNKNOWN; a failed CI read leaves `prCi`
 * as it was (or `null`), under which PR CI is UNKNOWN.
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
  /** `useSessionInfoList().info` — the listing the rows were classified with. */
  info: SessionInfoListView;
}

export interface SessionFactsState {
  /** Keyed by `row.sessionId`. */
  facts: ReadonlyMap<string, SessionFacts>;
  /** `null` until the first CI read answers. */
  prCi: ReadonlyMap<string, PrCiRead> | null;
}

export interface SessionInfoListState {
  info: SessionInfoListView;
  /** Re-read the listing now. */
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

/**
 * The batched session-info listing for `ids`, re-read when the set of ids
 * changes and every `pollMs` (default 30 s). A thrown IPC is `unavailable`
 * with its reason — never an empty listing.
 */
export function useSessionInfoList(
  ids: readonly string[],
  pollMs: number = 30_000,
): SessionInfoListState {
  const [info, setInfo] = useState<SessionInfoListView>({ phase: "loading" });
  const seq = useRef(0);
  const idsKey = useMemo(() => [...new Set(ids)].sort().join("\n"), [ids]);

  const refresh = useCallback(() => {
    const mine = ++seq.current;
    const wanted = idsKey === "" ? [] : idsKey.split("\n");
    void (async () => {
      let next: SessionInfoListView;
      try {
        next = await fetchSessionInfoList(wanted);
      } catch (e) {
        next = {
          phase: "unavailable",
          reason: `ipc_error: ${describeThrown(e, "session_info_list failed")}`,
        };
      }
      if (mine === seq.current) setInfo(next);
    })();
  }, [idsKey]);

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, pollMs);
    return () => clearInterval(t);
  }, [refresh, pollMs]);

  return { info, refresh };
}

/** The CI read of a listing that names no open PR: nothing to ask. */
const NO_PR_CI: ReadonlyMap<string, PrCiRead> = new Map();

/** Every row's facts over `info`, with coord's CI read for the open PRs it names. */
export function useSessionFacts({ rows, org, info }: UseSessionFactsParams): SessionFactsState {
  const [prCi, setPrCi] = useState<ReadonlyMap<string, PrCiRead> | null>(null);
  const seq = useRef(0);

  // Re-asked on every listing (each poll), so a PR's CI moves from pending to
  // green without a reload. A listing naming no open PR needs no CI read.
  const wanted = useMemo(() => (info.phase === "ok" ? prsNeedingCi(info) : null), [info]);
  useEffect(() => {
    const mine = ++seq.current;
    if (wanted === null || wanted.length === 0) return;
    fetchPrCiStates(wanted)
      .then((ci) => {
        if (mine === seq.current) setPrCi(ci);
      })
      .catch((e: unknown) => {
        console.error("coord_pr_ci_states failed:", describeThrown(e, "unknown error"));
      });
  }, [wanted]);
  const effectivePrCi = wanted !== null && wanted.length === 0 ? NO_PR_CI : prCi;

  const facts = useMemo(() => {
    const out = new Map<string, SessionFacts>();
    for (const r of rows)
      out.set(r.row.sessionId, buildSessionFacts({ ...r, org, info, prCi: effectivePrCi }));
    return out;
  }, [rows, org, info, effectivePrCi]);

  return { facts, prCi: effectivePrCi };
}
