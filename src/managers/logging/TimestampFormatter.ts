/**
 * TimestampFormatter
 *
 * Responsible for formatting timestamps consistently across the logging system.
 * Follows Single Responsibility Principle - handles only timestamp formatting.
 */

/**
 * Formats timestamps for log entries.
 */
export class TimestampFormatter {
  /**
   * Format current time as HH:MM:SS
   */
  formatCurrentTime(): string {
    return new Date().toLocaleTimeString("en-US", {
      hour12: false,
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }

  /**
   * Format Unix timestamp (in seconds) as HH:MM:SS
   */
  formatUnixTimestamp(timestamp: number): string {
    return new Date(timestamp * 1000).toLocaleTimeString("en-US", {
      hour12: false,
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }
}
