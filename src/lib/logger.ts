/* eslint-disable no-console */
/**
 * Structured logging utility.
 *
 * Routes every level through `console.<level>` with a `[context]` prefix.
 * Production console output is governed by the consumer (DevTools filter,
 * SDK `ConsoleCapture` buffer) — not by a build-time gate here. Call sites
 * that want production-silent emission must guard themselves (typically
 * `localStorage["debug:<flag>"] === "1"`); see `useTabSessionIdCapture.ts`
 * for the canonical pattern.
 *
 * Prior to 2026-05-13 this module suppressed `debug`/`info` outside
 * `import.meta.env.DEV`. That neutered every documented `debug:<flag>`
 * diagnostic surface in the embedded production build (the runner's
 * primary :9876 webview), which is the only build where those surfaces
 * matter. The `isDev` gate was removed; `info` calls fire repo-wide
 * unconditionally now too, so noisy callers must guard themselves.
 */

type LogLevel = "debug" | "info" | "warn" | "error";

function formatMessage(_level: LogLevel, context: string, message: string): string {
  return `[${context}] ${message}`;
}

function createLogger(context: string) {
  return {
    debug(...args: unknown[]) {
      console.debug(formatMessage("debug", context, String(args[0])), ...args.slice(1));
    },
    info(...args: unknown[]) {
      console.info(formatMessage("info", context, String(args[0])), ...args.slice(1));
    },
    warn(...args: unknown[]) {
      console.warn(formatMessage("warn", context, String(args[0])), ...args.slice(1));
    },
    error(...args: unknown[]) {
      console.error(formatMessage("error", context, String(args[0])), ...args.slice(1));
    },
  };
}

export { createLogger };
export type { LogLevel };
