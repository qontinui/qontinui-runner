/* eslint-disable no-console */
/**
 * Structured logging utility.
 *
 * In development, logs to the console with context tags.
 * In production, suppresses debug/info logs (only warn/error pass through).
 */

const isDev = import.meta.env.DEV;

type LogLevel = "debug" | "info" | "warn" | "error";

function formatMessage(_level: LogLevel, context: string, message: string): string {
  return `[${context}] ${message}`;
}

function createLogger(context: string) {
  return {
    debug(...args: unknown[]) {
      if (isDev) console.debug(formatMessage("debug", context, String(args[0])), ...args.slice(1));
    },
    info(...args: unknown[]) {
      if (isDev) console.info(formatMessage("info", context, String(args[0])), ...args.slice(1));
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
