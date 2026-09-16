/**
 * Code changes a Conductor worker has made, as seen by the grid cell.
 *
 * The source of truth is the runner's `GET /sessions/{id}/file-changes`
 * route (`src-tauri/src/mcp/snapshots.rs`): for every file the session
 * touched it returns the PRE-EDIT snapshot text (`capture_pre_edit_snapshot`,
 * taken the first time the session edited that path, whatever tool did the
 * editing) and the file's CURRENT text. The diff is computed here with jsdiff
 * so the wire carries plain text and the backend stays a reader.
 *
 * Everything in this module is pure except `fetchSessionFileChanges`, so the
 * pairing and rendering decisions are unit-testable without a runner.
 */

import * as Diff from "diff";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

/** Mirrors `SessionFileChange` in `mcp/snapshots.rs` (camelCase on the wire). */
export interface SessionFileChange {
  filePath: string;
  status: "modified" | "unchanged" | "deleted" | "created" | "binary" | "unreadable";
  before: string | null;
  after: string | null;
  beforeBytes: number | null;
  afterBytes: number | null;
  beforeSha256: string | null;
  afterSha256: string | null;
  truncated: boolean;
  takenAt: string | null;
  detail: string | null;
}

export interface SessionFileChangesResponse {
  sessionId: string;
  files: SessionFileChange[];
  readAtMs: number;
}

/**
 * The cell's view of the last read. `error` is the honest arm: the route
 * could not be read, so the list is UNKNOWN — it is never rendered as "no
 * changes". `ok` with an empty `files` is a genuine "nothing snapshotted".
 */
export type FileChangesRead =
  | { status: "loading"; previous: SessionFileChangesResponse | null }
  | { status: "ok"; response: SessionFileChangesResponse }
  | { status: "error"; error: string; atMs: number; previous: SessionFileChangesResponse | null };

export async function fetchSessionFileChanges(
  taskRunId: string,
  signal?: AbortSignal,
): Promise<SessionFileChangesResponse> {
  const url = `${getApiBase()}/sessions/${encodeURIComponent(taskRunId)}/file-changes`;
  const resp = await tracedFetch(url, { signal });
  if (!resp.ok) {
    const body = await resp.text().catch(() => "");
    throw new Error(`HTTP ${resp.status}${body ? `: ${body.slice(0, 200)}` : ""}`);
  }
  const json = (await resp.json()) as Partial<SessionFileChangesResponse>;
  if (!json || !Array.isArray(json.files)) {
    throw new Error("malformed file-changes payload (no `files` array)");
  }
  return {
    sessionId: json.sessionId ?? taskRunId,
    files: json.files,
    readAtMs: typeof json.readAtMs === "number" ? json.readAtMs : Date.now(),
  };
}

export type DiffLineKind = "add" | "del" | "ctx";

export interface DiffLine {
  kind: DiffLineKind;
  text: string;
}

export interface DiffHunk {
  header: string;
  lines: DiffLine[];
}

/** Lines of context around each change in a rendered hunk. */
export const DIFF_CONTEXT_LINES = 3;

/**
 * Unified-diff hunks for a change, or `null` when there is nothing honest to
 * diff (`binary`, `unreadable`, an over-cap side, or `unchanged`). A
 * `created` file diffs from empty; a `deleted` one diffs to empty.
 */
export function diffHunks(change: SessionFileChange): DiffHunk[] | null {
  if (change.status === "binary" || change.status === "unreadable" || change.truncated) {
    return null;
  }
  if (change.status === "unchanged") return null;
  const before = change.before ?? "";
  const after = change.after ?? "";
  if (change.status === "modified" && (change.before === null || change.after === null)) {
    return null;
  }
  const patch = Diff.structuredPatch(change.filePath, change.filePath, before, after, "", "", {
    context: DIFF_CONTEXT_LINES,
  });
  return patch.hunks.map((h) => ({
    header: `@@ -${h.oldStart},${h.oldLines} +${h.newStart},${h.newLines} @@`,
    lines: h.lines.map<DiffLine>((line) => {
      const marker = line[0];
      const text = line.slice(1);
      if (marker === "+") return { kind: "add", text };
      if (marker === "-") return { kind: "del", text };
      return { kind: "ctx", text };
    }),
  }));
}

export interface DiffStat {
  additions: number;
  deletions: number;
}

/** Added/removed line counts across `hunks` (0/0 for a null diff). */
export function diffStat(hunks: DiffHunk[] | null): DiffStat {
  const stat: DiffStat = { additions: 0, deletions: 0 };
  if (!hunks) return stat;
  for (const hunk of hunks) {
    for (const line of hunk.lines) {
      if (line.kind === "add") stat.additions += 1;
      else if (line.kind === "del") stat.deletions += 1;
    }
  }
  return stat;
}

/**
 * One-line label for a change that carries no diff, naming WHY rather than
 * showing an empty hunk list. Returns `null` when a diff can be shown.
 */
export function noDiffReason(change: SessionFileChange): string | null {
  switch (change.status) {
    case "unreadable":
      return `UNKNOWN — ${change.detail ?? "could not be read"}`;
    case "binary":
      return "binary content — no text diff";
    case "unchanged":
      return "identical to the pre-edit snapshot";
    default:
      break;
  }
  if (change.truncated) {
    const sizes = [change.beforeBytes, change.afterBytes]
      .filter((n): n is number => typeof n === "number")
      .map(formatBytes)
      .join(" → ");
    return `too large to diff${sizes ? ` (${sizes})` : ""}`;
  }
  return null;
}

export function formatBytes(n: number): string {
  if (n >= 1_048_576) return `${(n / 1_048_576).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} B`;
}

/** Last two path components, for a compact file label. */
export function shortPath(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/").filter(Boolean);
  if (parts.length <= 2) return parts.join("/") || path;
  return parts.slice(-2).join("/");
}

/**
 * Files worth listing first: anything that differs or could not be read.
 * `unchanged` rows sink to the bottom so a busy worker's real edits are the
 * first thing in view. Stable within each group.
 */
export function orderChanges(files: readonly SessionFileChange[]): SessionFileChange[] {
  const rank = (c: SessionFileChange): number => {
    if (c.status === "unreadable") return 0;
    if (c.status === "unchanged") return 2;
    return 1;
  };
  return [...files].sort((a, b) => rank(a) - rank(b));
}

/** Count of rows a reader would call "changes" (everything but `unchanged`). */
export function countChanged(files: readonly SessionFileChange[]): number {
  return files.filter((c) => c.status !== "unchanged").length;
}
