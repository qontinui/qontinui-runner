/**
 * Formatting Utilities
 *
 * Shared formatting functions for duration, time, and other display values.
 */

/**
 * Format duration in milliseconds to a human-readable string.
 *
 * @param ms - Duration in milliseconds
 * @returns Formatted string like "1h 23m", "45s", or "123ms"
 */
export function formatDuration(ms: number | undefined | null): string {
  if (ms === undefined || ms === null) return "-";

  if (ms < 1000) {
    return `${ms}ms`;
  }

  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) {
    return `${seconds}s`;
  }

  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;
  if (minutes < 60) {
    return remainingSeconds > 0 ? `${minutes}m ${remainingSeconds}s` : `${minutes}m`;
  }

  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return remainingMinutes > 0 ? `${hours}h ${remainingMinutes}m` : `${hours}h`;
}

/**
 * Format a timestamp as a localized date/time string.
 *
 * @param timestamp - ISO 8601 timestamp string
 * @returns Localized date/time string
 */
export function formatTimestamp(timestamp: string | undefined | null): string {
  if (!timestamp) return "-";
  return new Date(timestamp).toLocaleString();
}

/**
 * Format a timestamp as a relative time string (e.g., "2 hours ago").
 *
 * @param timestamp - ISO 8601 timestamp string
 * @returns Relative time string
 */
export function formatRelativeTime(timestamp: string | undefined | null): string {
  if (!timestamp) return "-";

  const date = new Date(timestamp);
  const now = new Date();
  const diffMs = now.getTime() - date.getTime();

  const seconds = Math.floor(diffMs / 1000);
  if (seconds < 60) return "just now";

  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;

  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d ago`;

  return date.toLocaleDateString();
}
