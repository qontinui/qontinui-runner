import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** `fallback (detail)`, or the bare detail when a caller passes an empty fallback. */
function withFallback(fallback: string, detail: string): string {
  return fallback ? `${fallback} (${detail})` : detail;
}

/** Longest JSON dump `describeThrown` will embed before truncating it. */
export const DESCRIBE_THROWN_DUMP_MAX = 500;

/**
 * Describe a THROWN value, whatever shape it arrived in. The one helper every
 * catch site uses to turn what it caught into display text.
 *
 * THE DEFECT this closes: a loader written as
 * `err instanceof Error ? err.message : "<bare constant>"` takes the `else`
 * arm on every real Tauri failure — `invoke()` rejects with a plain STRING
 * carrying the Rust command's own error text — and so DISCARDS the diagnosis.
 * The `: String(err)` variant is the same defect: a rejected object renders
 * `[object Object]`. `eslint.config.js` (`CATCH_SITE_DISCARD_SELECTORS`) bans
 * the hand-rolled ternary so this helper stays the only spelling.
 *
 * An `Error`'s message and a string are returned trimmed. Everything else is
 * serialized rather than dropped: a number/boolean alongside the fallback (a
 * bare digit carries no context), an object's numeric HTTP `status` and/or its
 * first `error` / `message` / `detail` / `code` string when it has one, and a
 * JSON dump (capped at
 * `DESCRIBE_THROWN_DUMP_MAX` chars) as the last resort. `fallback` alone is
 * used ONLY when the value carries no information at all — losing the cause
 * is strictly worse than showing it ugly.
 *
 * Plan `2026-09-09-catch-site-discard-is-repo-wide-and-the-deferral-was-never-measured`.
 */
export function describeThrown(err: unknown, fallback: string): string {
  if (err instanceof Error) return err.message.trim() || fallback;
  if (typeof err === "string") return err.trim() || fallback;
  if (typeof err === "number" || typeof err === "boolean")
    return withFallback(fallback, String(err));
  if (err && typeof err === "object") {
    const o = err as Record<string, unknown>;
    // Only a numeric (or 3-digit string) status is an HTTP status; a word
    // like `status: "failed"` is left to the JSON dump below, which keeps the
    // fallback's context and every other field.
    const status =
      typeof o.status === "number" ||
      (typeof o.status === "string" && /^\d{3}$/.test(o.status.trim()))
        ? `HTTP ${String(o.status).trim()}`
        : null;
    const body = [o.error, o.message, o.detail, o.code].find(
      (c): c is string => typeof c === "string" && c.trim() !== "",
    );
    if (status && body) return `${status}: ${body.trim()}`;
    if (body) return body.trim();
    if (status) return status;
    try {
      const dump = JSON.stringify(err);
      if (dump && dump !== "{}") {
        const shown =
          dump.length > DESCRIBE_THROWN_DUMP_MAX
            ? `${dump.slice(0, DESCRIBE_THROWN_DUMP_MAX)}…(truncated)`
            : dump;
        return withFallback(fallback, shown);
      }
    } catch {
      // Circular / non-serializable — fall through to the fallback.
    }
  }
  return fallback;
}
