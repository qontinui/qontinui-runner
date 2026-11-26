/**
 * Project-based log configuration types
 *
 * Projects reference global log sources (from Settings > Log Sources)
 * instead of embedding their own copies.
 */

import { formatBytes } from "@/lib/formatting";

/**
 * Project-specific log configuration (slim — references global sources)
 */
export interface ProjectLogConfig {
  /** Unique project identifier */
  projectId: string;

  /** Human-readable project name */
  projectName: string;

  /** ID of the global profile to use, or None for "all enabled" */
  globalProfileId?: string;

  /** Selected global source IDs (overrides profile when non-empty) */
  selectedSourceIds: string[];

  /** Directory where runner stores its own logs for this project */
  logDirectory: string;

  /** Directory where runner stores screenshots for this project */
  screenshotDirectory: string;

  /** Directory where AI analysis outputs are stored */
  aiOutputDirectory: string;

  /** Last modified timestamp */
  updatedAt?: string;
}

/**
 * Content read from a log source
 */
export interface LogSourceContent {
  /** The log source this content came from */
  sourceId: string;

  /** Source name for display */
  sourceName: string;

  /** The file's last lines, oldest first */
  lines: string[];

  /**
   * The file's line count — known only when the read covered the whole file.
   * `null` for a tail of a larger file: the runner reads at most a few MiB
   * from a log's end and never counts the whole of it (a day log reaches
   * 2.9 GB). `null` is "not counted", never zero.
   */
  totalLines: number | null;

  /** The file's size in bytes when it was read; `null` when it was not read */
  fileBytes: number | null;

  /** File path that was read (a dated rotation of the configured path, when that is absent) */
  filePath: string;

  /** Last modified time of the file */
  lastModified?: string;

  /** Any error that occurred while reading */
  error?: string;
}

/**
 * The figure shown beside a log source's name: its line count when the read
 * covered the whole file, otherwise the file's size. The runner reads only a
 * bounded tail of a large log and never counts it, so the size stands in —
 * the number of lines the tail happened to hold would understate the file.
 * `null` when neither is known (the file was not read).
 */
export function sourceSizeLabel(
  source: Pick<LogSourceContent, "totalLines" | "fileBytes">,
): string | null {
  if (source.totalLines != null) return `${source.totalLines} lines`;
  if (source.fileBytes != null) return formatBytes(source.fileBytes);
  return null;
}

/**
 * Combined log content from all enabled sources
 */
export interface ProjectLogsState {
  /** Project this state belongs to */
  projectId: string;

  /** Content from each enabled log source */
  sources: LogSourceContent[];

  /** Whether logs are currently being refreshed */
  loading: boolean;

  /** Last refresh timestamp */
  lastRefresh?: string;

  /** Global error message if refresh failed */
  error?: string;
}

/**
 * AI-suggested log source from the find_log_sources_with_ai command
 */
export interface AiSuggestedLogSource {
  /** Suggested name for the log source */
  name: string;
  /** Type: "file" or "directory" */
  type: "file" | "directory";
  /** Suggested path (may contain placeholders) */
  path: string;
  /** Glob pattern for directory type */
  pattern?: string;
  /** Description of what this log source typically contains */
  description: string;
  /** Suggested color for UI display */
  color?: string;
}

/**
 * Create a new project log config with default values
 */
export function createProjectLogConfig(
  projectId: string,
  projectName: string,
  baseDir: string,
): ProjectLogConfig {
  return {
    projectId,
    projectName,
    globalProfileId: undefined,
    selectedSourceIds: [],
    logDirectory: `${baseDir}/logs`,
    screenshotDirectory: `${baseDir}/screenshots`,
    aiOutputDirectory: `${baseDir}/ai-output`,
    updatedAt: new Date().toISOString(),
  };
}
