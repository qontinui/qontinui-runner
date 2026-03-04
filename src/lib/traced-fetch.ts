/**
 * Traced fetch wrapper for cross-service trace propagation.
 *
 * Injects `X-Trace-Id` headers into outgoing requests to correlate
 * frontend API calls with backend spans and error events.
 *
 * Usage:
 *   import { tracedFetch, setCurrentTraceId } from "@/lib/traced-fetch";
 *   setCurrentTraceId("abc12345");
 *   const response = await tracedFetch("/api/task-runs");
 */

const TRACE_ID_HEADER = "X-Trace-Id";

let currentTraceId: string | null = null;

/** Set the current trace ID for all subsequent fetches. */
export function setCurrentTraceId(traceId: string | null): void {
  currentTraceId = traceId;
}

/** Get the current trace ID. */
export function getCurrentTraceId(): string | null {
  return currentTraceId;
}

/** Generate a short random trace ID (8 hex chars). */
export function generateTraceId(): string {
  const bytes = new Uint8Array(4);
  crypto.getRandomValues(bytes);
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * Fetch wrapper that injects `X-Trace-Id` header for trace propagation.
 *
 * If a response includes `X-Trace-Id` and we don't have one set,
 * we capture it for subsequent requests.
 */
export async function tracedFetch(
  url: string,
  init?: RequestInit
): Promise<Response> {
  const headers = new Headers(init?.headers);

  if (currentTraceId && !headers.has(TRACE_ID_HEADER)) {
    headers.set(TRACE_ID_HEADER, currentTraceId);
  }

  const response = await fetch(url, { ...init, headers });

  // Capture trace ID from response if we don't have one
  if (!currentTraceId) {
    const responseTraceId = response.headers.get(TRACE_ID_HEADER);
    if (responseTraceId) {
      currentTraceId = responseTraceId;
    }
  }

  return response;
}
