/**
 * One-time import of the Session Manager's legacy localStorage organization
 * into the runner store (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 2, D3): `session-manager-pinned` (a JSON array of session ids) becomes
 * `setPinned`, and each entry of `session-manager-labels` (session id → one
 * free-text label) becomes that session's tag. The keys are read and removed
 * through `instanceStorage`, the same port-namespaced helper that wrote them.
 *
 * "Once" is a property of the keys, not of a flag: a key is removed only after
 * the runner CONFIRMED every op built from it, so a failed import retries on
 * the next run and a successful one finds nothing to do. The ops are
 * idempotent (pin = set, tag = set union), so two windows importing the same
 * keys concurrently converge on the same org. A key whose value cannot be
 * parsed is left in place and reported — never imported as empty, never
 * deleted unread.
 */

import { instanceStorage } from "@/lib/instance-storage";
import { describeThrown } from "@/lib/utils";

import type { SessionOrgApplyResponse, SessionOrgOp } from "./sessionOrgTypes";

export const LEGACY_PINNED_KEY = "session-manager-pinned";
export const LEGACY_LABELS_KEY = "session-manager-labels";

export interface LegacyStorage {
  getItem(key: string): string | null;
  removeItem(key: string): void;
}

export type SessionOrgImportOutcome =
  | { status: "nothing-to-import"; unreadable: string[] }
  | { status: "imported"; pinned: number; tagged: number; unreadable: string[] }
  | { status: "failed"; reason: string; unreadable: string[] };

interface ImportDeps {
  apply(ops: SessionOrgOp[]): Promise<SessionOrgApplyResponse>;
  storage?: LegacyStorage;
}

function parsePinned(raw: string): string[] | null {
  try {
    const value: unknown = JSON.parse(raw);
    if (!Array.isArray(value) || !value.every((v) => typeof v === "string")) return null;
    return value.map((v: string) => v.trim()).filter((v) => v.length > 0);
  } catch {
    return null;
  }
}

function parseLabels(raw: string): Array<[string, string]> | null {
  try {
    const value: unknown = JSON.parse(raw);
    if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
    const entries = Object.entries(value as Record<string, unknown>);
    if (!entries.every(([, label]) => typeof label === "string")) return null;
    return (entries as Array<[string, string]>)
      .map(([id, label]): [string, string] => [id.trim(), label.trim()])
      .filter(([id, label]) => id.length > 0 && label.length > 0);
  } catch {
    return null;
  }
}

/** Import the legacy keys, then remove the ones the runner confirmed. */
export async function runSessionOrgImport({
  apply,
  storage = instanceStorage,
}: ImportDeps): Promise<SessionOrgImportOutcome> {
  const unreadable: string[] = [];
  const pinnedRaw = storage.getItem(LEGACY_PINNED_KEY);
  const labelsRaw = storage.getItem(LEGACY_LABELS_KEY);

  const pinned = pinnedRaw === null ? null : parsePinned(pinnedRaw);
  if (pinnedRaw !== null && pinned === null) unreadable.push(LEGACY_PINNED_KEY);
  const labels = labelsRaw === null ? null : parseLabels(labelsRaw);
  if (labelsRaw !== null && labels === null) unreadable.push(LEGACY_LABELS_KEY);

  const importable: string[] = [];
  if (pinned !== null) importable.push(LEGACY_PINNED_KEY);
  if (labels !== null) importable.push(LEGACY_LABELS_KEY);
  if (importable.length === 0) return { status: "nothing-to-import", unreadable };

  const ops: SessionOrgOp[] = [];
  if (pinned && pinned.length > 0) {
    ops.push({ op: "setPinned", sessionKeys: pinned, pinned: true });
  }
  for (const [sessionKey, label] of labels ?? []) {
    ops.push({ op: "addTags", sessionKeys: [sessionKey], tags: [label] });
  }

  if (ops.length > 0) {
    let response: SessionOrgApplyResponse;
    try {
      response = await apply(ops);
    } catch (e) {
      return {
        status: "failed",
        reason: describeThrown(e, "session_org_apply failed"),
        unreadable,
      };
    }
    const refused = response.results.filter((r) => !r.ok).map((r) => r.error ?? "refused");
    if (response.snapshot.status !== "known" || refused.length > 0) {
      return {
        status: "failed",
        reason:
          refused.length > 0
            ? `the runner refused: ${refused.join(", ")}`
            : "the runner's organization is unknown",
        unreadable,
      };
    }
  }

  for (const key of importable) storage.removeItem(key);
  return {
    status: "imported",
    pinned: pinned?.length ?? 0,
    tagged: labels?.length ?? 0,
    unreadable,
  };
}
