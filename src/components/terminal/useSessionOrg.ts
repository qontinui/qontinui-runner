/**
 * `useSessionOrg` — the Session Manager's view of the runner's organization
 * store (groups, pins, tags, mute, snooze, last-seen, recently closed, saved
 * views, dismissed suggestions). Plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 2.
 *
 * ## Authority and freshness
 *
 * The runner store (`session_org_get` / `session_org_apply`) is the only
 * authority. This module holds the last state the runner CONFIRMED plus the
 * batches still in flight, and renders `confirmed + pending` through
 * `applyOpsOptimistic` so a click shows at once. Every apply response
 * replaces the confirmed state with the runner's own, so the preview is
 * always reconciled — a refused op simply disappears. The
 * `session-org-changed` event carries every other window's (and every
 * agent's HTTP) change; an event or response older than the confirmed
 * `revision` is dropped, so out-of-order delivery never moves the org back.
 *
 * ## Three states, never a default
 *
 * `loading` (nothing read yet), `unknown` (the runner could not read its file,
 * or could not be asked) and `known`. Neither of the first two may render as
 * "no groups" (served policy `verification-and-evidence`
 * `unknown-must-not-render-as-a-default`).
 *
 * ## One client per webview
 *
 * Like `useLiveClaudeSessionNames`, one ref-counted client serves every mount,
 * so two surfaces never disagree and the event is subscribed once.
 *
 * ## Wiring API (for the Session Manager)
 *
 * - `useSessionOrg()` → `{ view, apply, refresh, importLegacyKeys }`.
 * - `sessionKeyFor(claudeSessionId, pane)` (re-exported) builds member keys.
 * - `memberOf(view, key)` reads one session's organization.
 * - `importLegacyKeys()` runs the one-time `session-manager-pinned` /
 *   `session-manager-labels` import (`sessionOrgImport.ts`) once per webview;
 *   call it only once the old localStorage pins/labels code is deleted.
 */

import { useEffect, useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { describeThrown } from "@/lib/utils";

import { applyOpsOptimistic } from "./sessionOrgOptimistic";
import { runSessionOrgImport, type SessionOrgImportOutcome } from "./sessionOrgImport";
import {
  SESSION_ORG_CHANGED_EVENT,
  type MemberOrg,
  type SessionOrg,
  type SessionOrgApplyResponse,
  type SessionOrgOp,
  type SessionOrgSnapshot,
} from "./sessionOrgTypes";

export { sessionKeyFor, paneKeyString } from "./sessionOrgTypes";
export type { PaneIdentity, SessionOrgOp, SessionOrg, MemberOrg } from "./sessionOrgTypes";

/** What a surface renders. */
export type SessionOrgView =
  | { phase: "loading" }
  | {
      phase: "unknown";
      /** Why the org could not be read (unreadable file, command failure). */
      reason: string;
      /** Where an unreadable file was preserved, when the runner moved it. */
      preservedAt: string | null;
    }
  | {
      phase: "known";
      /** Confirmed state with in-flight batches projected on top. */
      org: SessionOrg;
      /** Batches sent and not yet answered (the "in flight" acknowledgement). */
      pending: number;
      /** The most recent refusal or transport failure, until the next apply. */
      lastError: string | null;
    };

/** The three doors this module needs — injectable for tests. */
export interface SessionOrgTransport {
  get(): Promise<SessionOrgSnapshot>;
  apply(ops: SessionOrgOp[]): Promise<SessionOrgApplyResponse>;
  /** Subscribe to change events; resolves to the unsubscribe function. */
  subscribe(onChange: (snapshot: SessionOrgSnapshot) => void): Promise<() => void>;
}

export const tauriSessionOrgTransport: SessionOrgTransport = {
  get: () => invoke<SessionOrgSnapshot>("session_org_get"),
  apply: (ops) => invoke<SessionOrgApplyResponse>("session_org_apply", { ops }),
  subscribe: (onChange) =>
    listen<SessionOrgSnapshot>(SESSION_ORG_CHANGED_EVENT, (event) => onChange(event.payload)),
};

/** A fresh id for an optimistic `createGroup`, so preview and runner agree. */
function newGroupId(): string {
  return crypto.randomUUID();
}

export interface SessionOrgClient {
  getView(): SessionOrgView;
  subscribe(listener: () => void): () => void;
  /** Begin reading + listening (ref-counted). */
  retain(): void;
  /** Stop when the last holder releases. */
  release(): void;
  refresh(): Promise<void>;
  /**
   * Apply `ops`. Resolves to the runner's per-op results (with any
   * `createGroup` id filled in), or rejects when the runner could not be
   * asked — in which case the preview is withdrawn and the org re-read.
   */
  apply(ops: SessionOrgOp[]): Promise<SessionOrgApplyResponse>;
  /** The one-time legacy localStorage import; runs at most once per client. */
  importLegacyKeys(): Promise<SessionOrgImportOutcome>;
}

/** Build a client over `transport`. Exported for tests; surfaces use the hook. */
export function createSessionOrgClient(transport: SessionOrgTransport): SessionOrgClient {
  let confirmed: SessionOrgSnapshot | null = null;
  let loadError: string | null = null;
  let pending: { id: number; ops: SessionOrgOp[] }[] = [];
  let nextBatchId = 0;
  let lastError: string | null = null;
  let view: SessionOrgView = { phase: "loading" };
  let holders = 0;
  let unsubscribe: (() => void) | null = null;
  let subscribing = false;
  let importRun: Promise<SessionOrgImportOutcome> | null = null;
  const listeners = new Set<() => void>();

  function recompute(): void {
    if (confirmed === null) {
      view =
        loadError === null
          ? { phase: "loading" }
          : { phase: "unknown", reason: loadError, preservedAt: null };
    } else if (confirmed.status === "unknown" || confirmed.org === null) {
      view = {
        phase: "unknown",
        reason: confirmed.reason ?? "the runner reported the organization as unknown",
        preservedAt: confirmed.preservedAt,
      };
    } else {
      const ops = pending.flatMap((b) => b.ops);
      view = {
        phase: "known",
        org: ops.length ? applyOpsOptimistic(confirmed.org, ops) : confirmed.org,
        pending: pending.length,
        lastError,
      };
    }
    for (const l of listeners) l();
  }

  /** Accept `incoming` unless it is older than what we already hold. */
  function accept(incoming: SessionOrgSnapshot): void {
    const held = confirmed?.org?.revision;
    const next = incoming.org?.revision;
    if (held !== undefined && next !== undefined && next < held) return;
    confirmed = incoming;
    loadError = null;
    recompute();
  }

  async function refresh(): Promise<void> {
    try {
      accept(await transport.get());
    } catch (e) {
      // Keep a confirmed org on a failed re-read; only a first read that
      // fails turns `loading` into `unknown`.
      if (confirmed === null) {
        loadError = `could not read the session organization: ${describeThrown(e, "session_org_get failed")}`;
        recompute();
      }
    }
  }

  async function startListening(): Promise<void> {
    if (unsubscribe || subscribing) return;
    subscribing = true;
    try {
      const off = await transport.subscribe(accept);
      if (holders === 0) off();
      else unsubscribe = off;
    } catch {
      // No event channel: this window still converges on its own applies and
      // on refresh(); other windows' changes arrive on the next refresh.
    } finally {
      subscribing = false;
    }
  }

  async function apply(ops: SessionOrgOp[]): Promise<SessionOrgApplyResponse> {
    const filled = ops.map((op) =>
      op.op === "createGroup" && !op.groupId ? { ...op, groupId: newGroupId() } : op,
    );
    const batch = { id: nextBatchId++, ops: filled };
    pending = [...pending, batch];
    lastError = null;
    recompute();
    try {
      const response = await transport.apply(filled);
      pending = pending.filter((b) => b.id !== batch.id);
      const refused = response.results.filter((r) => !r.ok).map((r) => r.error ?? "refused");
      lastError = refused.length ? `refused: ${refused.join(", ")}` : null;
      accept(response.snapshot);
      recompute();
      return response;
    } catch (e) {
      pending = pending.filter((b) => b.id !== batch.id);
      lastError = `not applied: ${describeThrown(e, "session_org_apply failed")}`;
      recompute();
      await refresh();
      throw e;
    }
  }

  return {
    getView: () => view,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    retain() {
      holders += 1;
      if (holders === 1) {
        void startListening();
        void refresh();
      }
    },
    release() {
      holders = Math.max(0, holders - 1);
      if (holders === 0 && unsubscribe) {
        unsubscribe();
        unsubscribe = null;
      }
    },
    refresh,
    apply,
    importLegacyKeys() {
      importRun ??= runSessionOrgImport({ apply });
      return importRun;
    },
  };
}

let defaultClient: SessionOrgClient | null = null;

/** The webview's one client over the Tauri doors. */
export function getSessionOrgClient(): SessionOrgClient {
  defaultClient ??= createSessionOrgClient(tauriSessionOrgTransport);
  return defaultClient;
}

/** One session's organization, or null when it has none (or the org is not known). */
export function memberOf(view: SessionOrgView, sessionKey: string): MemberOrg | null {
  return view.phase === "known" ? (view.org.members[sessionKey] ?? null) : null;
}

export interface UseSessionOrgResult {
  view: SessionOrgView;
  apply: SessionOrgClient["apply"];
  refresh: SessionOrgClient["refresh"];
  importLegacyKeys: SessionOrgClient["importLegacyKeys"];
}

export function useSessionOrg(
  client: SessionOrgClient = getSessionOrgClient(),
): UseSessionOrgResult {
  useEffect(() => {
    client.retain();
    return () => client.release();
  }, [client]);
  const view = useSyncExternalStore(client.subscribe, client.getView, client.getView);
  return {
    view,
    apply: client.apply,
    refresh: client.refresh,
    importLegacyKeys: client.importLegacyKeys,
  };
}
