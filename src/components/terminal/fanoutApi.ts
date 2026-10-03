/**
 * Typed client for the runner's `/fanout` routes (plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`,
 * Phases 6–7).
 *
 * The wire shapes mirror `src-tauri/src/fanout/model.rs` (`RunView`,
 * `MemberView`, `MemberCounts`, `ConfigDirPolicy`) and
 * `src-tauri/src/mcp/fanout.rs` (`CreateFanoutRequest`, `CapOutcome`). Every
 * response is the runner's `ApiResponse` envelope `{success, data | error}`.
 *
 * Every call answers a {@link FanoutResult} rather than throwing, and a failed
 * read is a typed `ok: false` carrying the HTTP status and the server's own
 * error text — never an empty list. A route that could not be read is UNKNOWN,
 * and the strip renders it as such.
 *
 * The envelope parsing and the request builder are pure and exported so they
 * are testable under the `node` vitest environment; only {@link fanoutFetch}
 * touches the network.
 */

import { getApiBase, tracedFetch } from "@/lib/runner-api";

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

export type FanoutMemberState = "queued" | "admitted" | "released" | "cancelled" | "refused";

export type FanoutRunState = "active" | "completed";

/** Which Claude account each member launches under. */
export type ConfigDirPolicy = { kind: "bestHeadroom" } | { kind: "fixed"; configDir: string };

export interface FanoutMemberCounts {
  queued: number;
  admitted: number;
  released: number;
  cancelled: number;
  refused: number;
}

export interface FanoutMemberView {
  /** Position in the POSTED list (0-based) — what the release route addresses. */
  index: number;
  /**
   * The member's row in the preview it was created from (0-based), when the
   * creator sent one. Differs from `index` once rows were unticked; absent from
   * a runner build that predates it.
   */
  previewIndex?: number | null;
  title: string;
  prompt: string;
  state: FanoutMemberState;
  terminalId: string | null;
  claudeSessionId: string | null;
  /** Why the member is waiting or was released (a stable wire word, or a spawn error). */
  reason: string | null;
  admittedAt: string | null;
  releasedAt: string | null;
  /** Consecutive refusals of the same kind (absent from older runner builds). */
  refusals?: number;
  /** When a refused member next returns to the queue (its backoff), if waiting on one. */
  nextRetryAt?: string | null;
}

export interface FanoutRunView {
  id: string;
  tenantId: string | null;
  templateSlug: string | null;
  templateVersion: number | null;
  maxConcurrent: number;
  configDirPolicy: ConfigDirPolicy;
  workingDir: string;
  createdAt: string;
  state: FanoutRunState;
  counts: FanoutMemberCounts;
  members: FanoutMemberView[];
}

/** A create or PATCH result: the run, and how its cap relates to the bound. */
export interface FanoutCapOutcome {
  run: FanoutRunView;
  /**
   * This runner's `parallel_fanout` bound the cap was clamped against. It
   * resolves per device (the runner's registry row), not per tenant.
   */
  fanoutBound: number;
  /** What the caller asked for, when the clamp changed it. */
  clampedFrom: number | null;
}

export interface FanoutMemberInput {
  title: string;
  prompt: string;
  /** The row's number in the preview (0-based); stored and echoed as `previewIndex`. */
  previewIndex?: number;
}

/** `POST /fanout` body. The server never re-expands: these members run as-is. */
export interface CreateFanoutRequest {
  tenantId?: string;
  templateSlug?: string;
  templateVersion?: number;
  maxConcurrent?: number;
  configDirPolicy: ConfigDirPolicy;
  workingDir: string;
  members: FanoutMemberInput[];
}

/** The name of the Tauri event carrying a changed run (payload: `FanoutRunView`). */
export const FANOUT_CHANGED_EVENT = "fanout-changed";

// ---------------------------------------------------------------------------
// Result type
// ---------------------------------------------------------------------------

export type FanoutResult<T> =
  | { ok: true; data: T }
  | {
      ok: false;
      /** HTTP status, or `null` when no response was received at all. */
      status: number | null;
      /** The server's own error text, or a description of what failed. */
      error: string;
      /**
       * The envelope's machine-readable `code`, when the server sent one —
       * e.g. {@link FANOUT_LEDGER_NOT_LOADED}.
       */
      code?: string;
    };

/**
 * The `code` on a 503 served while the runner has not loaded its fan-out
 * ledger YET — the boot settle, before the first load ran. Expected and
 * transient: its runs are not known yet, and nothing has failed.
 */
export const FANOUT_LEDGER_NOT_LOADED = "FANOUT_LEDGER_NOT_LOADED";

/**
 * The `code` on a 503 served once a ledger load was attempted and FAILED
 * (PostgreSQL unreadable). Its runs are UNKNOWN.
 */
export const FANOUT_LEDGER_LOAD_FAILED = "FANOUT_LEDGER_LOAD_FAILED";

// ---------------------------------------------------------------------------
// Pure envelope parsing
// ---------------------------------------------------------------------------

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Turn an HTTP status + parsed body into a {@link FanoutResult}.
 *
 * Success requires BOTH a 2xx status AND `success: true` AND a present `data`
 * field: a 200 whose envelope says `success: false`, or that carries no
 * `data`, is a failure — never read as an empty answer. `validate` checks the
 * payload's shape; a payload that fails it is a failure naming the route.
 */
export function parseFanoutEnvelope<T>(
  status: number,
  body: unknown,
  validate: (data: unknown) => data is T,
  what: string,
): FanoutResult<T> {
  const envelopeError = isRecord(body) && typeof body.error === "string" ? body.error : null;
  const code = isRecord(body) && typeof body.code === "string" ? { code: body.code } : {};
  if (status < 200 || status >= 300) {
    return { ok: false, status, error: envelopeError ?? `${what}: HTTP ${status}`, ...code };
  }
  if (!isRecord(body) || body.success !== true) {
    return {
      ok: false,
      status,
      error: envelopeError ?? `${what}: response was not a success envelope`,
      ...code,
    };
  }
  if (!("data" in body) || !validate(body.data)) {
    return { ok: false, status, error: `${what}: response data had an unexpected shape` };
  }
  return { ok: true, data: body.data };
}

/** Structural check for one run — enough to render without crashing. */
export function isFanoutRunView(v: unknown): v is FanoutRunView {
  if (!isRecord(v)) return false;
  return (
    typeof v.id === "string" &&
    typeof v.maxConcurrent === "number" &&
    typeof v.workingDir === "string" &&
    (v.state === "active" || v.state === "completed") &&
    isRecord(v.counts) &&
    Array.isArray(v.members) &&
    v.members.every(
      (m) =>
        isRecord(m) &&
        typeof m.index === "number" &&
        typeof m.title === "string" &&
        typeof m.state === "string",
    )
  );
}

export function isFanoutRunList(v: unknown): v is FanoutRunView[] {
  return Array.isArray(v) && v.every(isFanoutRunView);
}

export function isFanoutCapOutcome(v: unknown): v is FanoutCapOutcome {
  return (
    isRecord(v) &&
    isFanoutRunView(v.run) &&
    typeof v.fanoutBound === "number" &&
    (v.clampedFrom === null || typeof v.clampedFrom === "number")
  );
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

/**
 * One request against a `/fanout` route. A network failure, an unparseable
 * body and a refusal all come back as `ok: false` with the reason.
 */
export async function fanoutFetch<T>(
  path: string,
  init: RequestInit | undefined,
  validate: (data: unknown) => data is T,
  what: string,
): Promise<FanoutResult<T>> {
  let resp: Response;
  try {
    resp = await tracedFetch(`${getApiBase()}${path}`, init);
  } catch (err) {
    return {
      ok: false,
      status: null,
      error: `${what}: runner unreachable (${err instanceof Error ? err.message : String(err)})`,
    };
  }
  let body: unknown = null;
  try {
    body = await resp.json();
  } catch {
    // Leave `body` null; the envelope parser names the failure with the status.
  }
  return parseFanoutEnvelope(resp.status, body, validate, what);
}

const JSON_HEADERS = { "Content-Type": "application/json" };

export function listFanoutRuns(): Promise<FanoutResult<FanoutRunView[]>> {
  return fanoutFetch("/fanout", undefined, isFanoutRunList, "GET /fanout");
}

export function getFanoutRun(id: string): Promise<FanoutResult<FanoutRunView>> {
  return fanoutFetch(
    `/fanout/${encodeURIComponent(id)}`,
    undefined,
    isFanoutRunView,
    "GET /fanout/{id}",
  );
}

export function createFanoutRun(req: CreateFanoutRequest): Promise<FanoutResult<FanoutCapOutcome>> {
  return fanoutFetch(
    "/fanout",
    { method: "POST", headers: JSON_HEADERS, body: JSON.stringify(req) },
    isFanoutCapOutcome,
    "POST /fanout",
  );
}

export function setFanoutMaxConcurrent(
  id: string,
  maxConcurrent: number,
): Promise<FanoutResult<FanoutCapOutcome>> {
  return fanoutFetch(
    `/fanout/${encodeURIComponent(id)}`,
    { method: "PATCH", headers: JSON_HEADERS, body: JSON.stringify({ maxConcurrent }) },
    isFanoutCapOutcome,
    "PATCH /fanout/{id}",
  );
}

/** Cancel every QUEUED member of a run. Running members are never touched. */
export function cancelFanoutQueued(id: string): Promise<FanoutResult<FanoutRunView>> {
  return fanoutFetch(
    `/fanout/${encodeURIComponent(id)}/cancel`,
    { method: "POST" },
    isFanoutRunView,
    "POST /fanout/{id}/cancel",
  );
}

/** Release an ADMITTED member's slot (409 when the member is not admitted). */
export function releaseFanoutMember(
  id: string,
  index: number,
): Promise<FanoutResult<FanoutRunView>> {
  return fanoutFetch(
    `/fanout/${encodeURIComponent(id)}/members/${index}/release`,
    { method: "POST" },
    isFanoutRunView,
    "POST /fanout/{id}/members/{index}/release",
  );
}
