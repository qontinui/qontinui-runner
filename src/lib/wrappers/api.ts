/**
 * Typed client for the runner's `/wrappers/*` HTTP endpoints.
 *
 * All paths are relative to `getApiBase()` — the same base the rest of the
 * runner UI uses for its REST calls. See `runner-api.ts` for how that base is
 * resolved at startup.
 *
 * Endpoints (canonical, Phase 1):
 *   GET    /wrappers
 *   GET    /wrappers/:id
 *   POST   /wrappers/install                  body: { package, version? }
 *   POST   /wrappers/:id/update
 *   DELETE /wrappers/:id
 *   POST   /wrappers/:id/start
 *   POST   /wrappers/:id/stop
 *   GET    /wrappers/:id/status
 *   POST   /wrappers/:id/dispatch             body: { action, params }
 *   GET    /wrappers/:id/credentials
 *   PUT    /wrappers/:id/credentials/:name    body: { value }
 *   DELETE /wrappers/:id/credentials/:name
 *   GET    /wrappers/registry                 (proxied registry.json)
 *
 * Endpoints stubbed in this client because Phase 1 has not wired them yet:
 *   GET    /wrappers/:id/logs
 *   GET    /wrappers/:id/readme
 * They return graceful "coming soon" placeholders so the UI tabs render
 * without error; once Phase 1 wires them, only the body of these helpers
 * needs to change.
 */

import { getApiBase } from "@/lib/runner-api";
import type {
  InstalledWrapper,
  WrapperStatusInfo,
  CredentialEntry,
  DispatchResult,
  RegistryListing,
  RegistryWrapperEntry,
} from "./types";

const REGISTRY_FALLBACK_URL =
  "https://raw.githubusercontent.com/jspinak/wrappers-registry/main/registry.json";

/** Throw if the response isn't OK, surfacing the body for debugging. */
async function ensureOk(res: Response, ctx: string): Promise<Response> {
  if (res.ok) return res;
  let detail = "";
  try {
    detail = await res.text();
  } catch {
    /* ignore */
  }
  throw new Error(`${ctx} failed: ${res.status} ${res.statusText}${detail ? ` — ${detail}` : ""}`);
}

/** List all installed wrappers. */
export async function listWrappers(): Promise<InstalledWrapper[]> {
  const res = await fetch(`${getApiBase()}/wrappers`);
  await ensureOk(res, "GET /wrappers");
  const data = await res.json();
  // Backend may return either a bare array or `{ wrappers: [...] }`.
  if (Array.isArray(data)) return data;
  if (data && Array.isArray((data as { wrappers?: unknown }).wrappers)) {
    return (data as { wrappers: InstalledWrapper[] }).wrappers;
  }
  return [];
}

/** Fetch a single installed wrapper. */
export async function getWrapper(id: string): Promise<InstalledWrapper> {
  const res = await fetch(`${getApiBase()}/wrappers/${encodeURIComponent(id)}`);
  await ensureOk(res, `GET /wrappers/${id}`);
  return res.json();
}

/** Install a wrapper from npm. */
export async function installWrapper(
  packageName: string,
  version?: string,
): Promise<InstalledWrapper> {
  const res = await fetch(`${getApiBase()}/wrappers/install`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ package: packageName, ...(version ? { version } : {}) }),
  });
  await ensureOk(res, `POST /wrappers/install (${packageName})`);
  return res.json();
}

/** Re-install at latest (or pinned version). */
export async function updateWrapper(id: string): Promise<InstalledWrapper> {
  const res = await fetch(`${getApiBase()}/wrappers/${encodeURIComponent(id)}/update`, {
    method: "POST",
  });
  await ensureOk(res, `POST /wrappers/${id}/update`);
  return res.json();
}

/** Uninstall a wrapper. */
export async function uninstallWrapper(id: string): Promise<void> {
  const res = await fetch(`${getApiBase()}/wrappers/${encodeURIComponent(id)}`, {
    method: "DELETE",
  });
  await ensureOk(res, `DELETE /wrappers/${id}`);
}

/** Start the wrapper subprocess. */
export async function startWrapper(id: string): Promise<WrapperStatusInfo> {
  const res = await fetch(`${getApiBase()}/wrappers/${encodeURIComponent(id)}/start`, {
    method: "POST",
  });
  await ensureOk(res, `POST /wrappers/${id}/start`);
  return res.json();
}

/** Stop the wrapper subprocess. */
export async function stopWrapper(id: string): Promise<WrapperStatusInfo> {
  const res = await fetch(`${getApiBase()}/wrappers/${encodeURIComponent(id)}/stop`, {
    method: "POST",
  });
  await ensureOk(res, `POST /wrappers/${id}/stop`);
  return res.json();
}

/** Fetch live wrapper status. */
export async function getWrapperStatus(id: string): Promise<WrapperStatusInfo> {
  const res = await fetch(`${getApiBase()}/wrappers/${encodeURIComponent(id)}/status`);
  await ensureOk(res, `GET /wrappers/${id}/status`);
  return res.json();
}

/** Dispatch an action through the wrapper. */
export async function dispatchAction<T = unknown>(
  id: string,
  action: string,
  params: Record<string, unknown>,
): Promise<DispatchResult<T>> {
  const res = await fetch(`${getApiBase()}/wrappers/${encodeURIComponent(id)}/dispatch`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ action, params }),
  });
  if (!res.ok) {
    let body = "";
    try {
      body = await res.text();
    } catch {
      /* ignore */
    }
    return { error: `dispatch failed: ${res.status} ${res.statusText}${body ? ` — ${body}` : ""}` };
  }
  const json = (await res.json()) as DispatchResult<T>;
  return json;
}

/** List wrapper credentials (names only — values are never returned). */
export async function listCredentials(id: string): Promise<CredentialEntry[]> {
  const res = await fetch(`${getApiBase()}/wrappers/${encodeURIComponent(id)}/credentials`);
  await ensureOk(res, `GET /wrappers/${id}/credentials`);
  const data = await res.json();
  if (Array.isArray(data)) return data;
  if (data && Array.isArray((data as { credentials?: unknown }).credentials)) {
    return (data as { credentials: CredentialEntry[] }).credentials;
  }
  return [];
}

/** Set a credential value. */
export async function setCredential(id: string, name: string, value: string): Promise<void> {
  const res = await fetch(
    `${getApiBase()}/wrappers/${encodeURIComponent(id)}/credentials/${encodeURIComponent(name)}`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ value }),
    },
  );
  await ensureOk(res, `PUT /wrappers/${id}/credentials/${name}`);
}

/** Delete a credential. */
export async function clearCredential(id: string, name: string): Promise<void> {
  const res = await fetch(
    `${getApiBase()}/wrappers/${encodeURIComponent(id)}/credentials/${encodeURIComponent(name)}`,
    { method: "DELETE" },
  );
  await ensureOk(res, `DELETE /wrappers/${id}/credentials/${name}`);
}

/**
 * Fetch the public registry listing. Tries the runner-side proxy first; falls
 * back to fetching the GitHub raw URL directly with a 24h memory cache. The
 * memory cache deliberately doesn't persist — the runner owns the canonical
 * cache; this is just a per-session safety net.
 */
let _registryCache: { at: number; data: RegistryListing } | null = null;
const REGISTRY_TTL_MS = 24 * 60 * 60 * 1000;

export async function listRegistry(opts?: { force?: boolean }): Promise<RegistryWrapperEntry[]> {
  const now = Date.now();
  if (!opts?.force && _registryCache && now - _registryCache.at < REGISTRY_TTL_MS) {
    return _registryCache.data.wrappers;
  }
  // Prefer runner proxy.
  try {
    const res = await fetch(`${getApiBase()}/wrappers/registry`);
    if (res.ok) {
      const data = (await res.json()) as RegistryListing | RegistryWrapperEntry[];
      const listing: RegistryListing = Array.isArray(data) ? { version: 1, wrappers: data } : data;
      _registryCache = { at: now, data: listing };
      return listing.wrappers ?? [];
    }
  } catch {
    /* fall through to direct fetch */
  }
  // Direct fetch fallback.
  try {
    const res = await fetch(REGISTRY_FALLBACK_URL);
    if (res.ok) {
      const data = (await res.json()) as RegistryListing;
      _registryCache = { at: now, data };
      return data.wrappers ?? [];
    }
  } catch {
    /* ignore */
  }
  return [];
}

// ---------------------------------------------------------------------------
// Stubbed endpoints (Phase 1 hadn't wired these as of this UI build).
// ---------------------------------------------------------------------------

/**
 * Fetch wrapper logs (stdout/stderr tail). Stubbed: tries the endpoint, but
 * if it 404s returns an empty array. Phase 1's Logs tab acceptance criteria
 * call out this fallback explicitly.
 */
export async function fetchLogs(
  id: string,
  opts?: { limit?: number },
): Promise<{ available: boolean; lines: string[] }> {
  const params = new URLSearchParams();
  if (opts?.limit) params.set("limit", String(opts.limit));
  const qs = params.toString() ? `?${params}` : "";
  try {
    const res = await fetch(`${getApiBase()}/wrappers/${encodeURIComponent(id)}/logs${qs}`);
    if (res.status === 404) return { available: false, lines: [] };
    if (!res.ok) return { available: false, lines: [] };
    const data = await res.json();
    if (Array.isArray(data)) return { available: true, lines: data as string[] };
    if (typeof data === "object" && data && "lines" in data) {
      return {
        available: true,
        lines: ((data as { lines?: unknown }).lines as string[]) ?? [],
      };
    }
    if (typeof data === "string") return { available: true, lines: data.split(/\r?\n/) };
    return { available: false, lines: [] };
  } catch {
    return { available: false, lines: [] };
  }
}

/**
 * Fetch wrapper README content. Stubbed: returns `{ available: false }` if
 * the endpoint isn't wired yet. AboutTab uses that signal to show a friendly
 * placeholder.
 */
export async function fetchReadme(id: string): Promise<{ available: boolean; content: string }> {
  try {
    const res = await fetch(`${getApiBase()}/wrappers/${encodeURIComponent(id)}/readme`);
    if (res.status === 404) return { available: false, content: "" };
    if (!res.ok) return { available: false, content: "" };
    const ct = res.headers.get("content-type") ?? "";
    if (ct.includes("application/json")) {
      const data = await res.json();
      if (typeof data === "string") return { available: true, content: data };
      if (data && typeof (data as { content?: unknown }).content === "string") {
        return { available: true, content: (data as { content: string }).content };
      }
      return { available: false, content: "" };
    }
    const text = await res.text();
    return { available: true, content: text };
  } catch {
    return { available: false, content: "" };
  }
}
