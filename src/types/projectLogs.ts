/**
 * Project-based log configuration types
 *
 * Defines the data structures for managing external log sources
 * that are specific to each project being automated.
 */

/**
 * A single external log source configuration
 */
export interface LogSource {
  /** Unique identifier for this log source */
  id: string;

  /** Human-readable name (e.g., Frontend, Backend, API) */
  name: string;

  /** Type of source - single file or directory with pattern matching */
  type: "file" | "directory";

  /** Absolute path to the log file or directory */
  path: string;

  /** Glob pattern for directory type (e.g., *.log, **\/*.log) */
  pattern?: string;

  /** Number of lines to tail from the end (default: 100) */
  tailLines?: number;

  /** Whether this source is enabled for viewing */
  enabled: boolean;

  /** Optional color for distinguishing in UI */
  color?: string;
}

/**
 * Project-specific log configuration
 */
export interface ProjectLogConfig {
  /** Unique project identifier */
  projectId: string;

  /** Human-readable project name */
  projectName: string;

  /** External log sources for this project */
  logSources: LogSource[];

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

  /** The log lines */
  lines: string[];

  /** Total line count in the file */
  totalLines: number;

  /** File path that was read */
  filePath: string;

  /** Last modified time of the file */
  lastModified?: string;

  /** Any error that occurred while reading */
  error?: string;
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
 * Common log path definition for quick-add functionality
 */
export interface CommonLogPath {
  /** Display name */
  name: string;
  /** Description of what this log contains */
  description: string;
  /** Relative path pattern (use {projectDir} as placeholder) */
  relativePath: string;
  /** Type of source */
  type: "file" | "directory";
  /** Pattern for directory types */
  pattern?: string;
  /** Category for grouping */
  category: "frontend" | "backend" | "general" | "runner";
  /** Suggested color */
  color: string;
}

/**
 * Common log paths for quick-add functionality
 * These are relative paths that can be appended to a project directory
 */
export const COMMON_LOG_PATHS: CommonLogPath[] = [
  // Qontinui Ecosystem (dev-start.ps1 logs)
  {
    name: "Qontinui Backend",
    description: "qontinui-web FastAPI backend (port 8000)",
    relativePath: "../.dev-logs/backend.log",
    type: "file",
    category: "backend",
    color: "#22c55e",
  },
  {
    name: "Qontinui Backend Errors",
    description: "qontinui-web backend stderr",
    relativePath: "../.dev-logs/backend.err.log",
    type: "file",
    category: "backend",
    color: "#ef4444",
  },
  {
    name: "Qontinui API",
    description: "qontinui-api service (port 8001)",
    relativePath: "../.dev-logs/qontinui-api.log",
    type: "file",
    category: "backend",
    color: "#06b6d4",
  },
  {
    name: "Qontinui API Errors",
    description: "qontinui-api stderr",
    relativePath: "../.dev-logs/qontinui-api.err.log",
    type: "file",
    category: "backend",
    color: "#ef4444",
  },
  {
    name: "Qontinui Frontend",
    description: "qontinui-web Next.js frontend (port 3001)",
    relativePath: "../.dev-logs/frontend.log",
    type: "file",
    category: "frontend",
    color: "#3b82f6",
  },
  {
    name: "Qontinui Frontend Errors",
    description: "qontinui-web frontend stderr",
    relativePath: "../.dev-logs/frontend.err.log",
    type: "file",
    category: "frontend",
    color: "#ef4444",
  },
  {
    name: "Qontinui Runner (Tauri)",
    description: "qontinui-runner Vite + Rust logs",
    relativePath: "../.dev-logs/runner-tauri.log",
    type: "file",
    category: "runner",
    color: "#f97316",
  },
  // Backend structured logs
  {
    name: "Backend Structured Log",
    description: "qontinui-web backend JSON logs",
    relativePath: "../qontinui-web/backend/logs/app.log",
    type: "file",
    category: "backend",
    color: "#8b5cf6",
  },
  // Generic project paths
  {
    name: "Application Logs Directory",
    description: "Common logs directory",
    relativePath: "logs",
    type: "directory",
    pattern: "*.log",
    category: "general",
    color: "#8b5cf6",
  },
  {
    name: "FastAPI/Uvicorn Log",
    description: "Python FastAPI server logs",
    relativePath: "logs/app.log",
    type: "file",
    category: "backend",
    color: "#22c55e",
  },
  {
    name: "Next.js Dev Server",
    description: "Next.js development server output",
    relativePath: ".next/trace",
    type: "directory",
    pattern: "*.log",
    category: "frontend",
    color: "#3b82f6",
  },
  {
    name: "PM2 Logs",
    description: "PM2 process manager logs",
    relativePath: ".pm2/logs",
    type: "directory",
    pattern: "*.log",
    category: "general",
    color: "#ef4444",
  },
];

/**
 * Default log source templates for common project types
 */
export const LOG_SOURCE_TEMPLATES: Record<string, Omit<LogSource, "id">[]> = {
  qontinui: [
    {
      name: "Backend",
      type: "file",
      path: "C:/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/backend.log",
      tailLines: 200,
      enabled: true,
      color: "#22c55e",
    },
    {
      name: "Backend Errors",
      type: "file",
      path: "C:/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/backend.err.log",
      tailLines: 100,
      enabled: true,
      color: "#ef4444",
    },
    {
      name: "API",
      type: "file",
      path: "C:/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/qontinui-api.log",
      tailLines: 200,
      enabled: true,
      color: "#06b6d4",
    },
    {
      name: "API Errors",
      type: "file",
      path: "C:/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/qontinui-api.err.log",
      tailLines: 100,
      enabled: true,
      color: "#ef4444",
    },
    {
      name: "Frontend",
      type: "file",
      path: "C:/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/frontend.log",
      tailLines: 200,
      enabled: true,
      color: "#3b82f6",
    },
    {
      name: "Frontend Errors",
      type: "file",
      path: "C:/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/frontend.err.log",
      tailLines: 100,
      enabled: true,
      color: "#ef4444",
    },
    {
      name: "Runner (Tauri)",
      type: "file",
      path: "C:/Users/Joshua/Documents/qontinui_parent_directory/.dev-logs/runner-tauri.log",
      tailLines: 200,
      enabled: true,
      color: "#f97316",
    },
  ],
  "next-fastapi": [
    {
      name: "Frontend",
      type: "file",
      path: "",
      enabled: true,
      color: "#3b82f6",
    },
    {
      name: "Backend",
      type: "file",
      path: "",
      enabled: true,
      color: "#22c55e",
    },
  ],
  tauri: [
    {
      name: "Vite/React",
      type: "file",
      path: "",
      enabled: true,
      color: "#3b82f6",
    },
    {
      name: "Rust Backend",
      type: "file",
      path: "",
      enabled: true,
      color: "#f97316",
    },
  ],
  generic: [
    {
      name: "Application Log",
      type: "file",
      path: "",
      enabled: true,
      color: "#8b5cf6",
    },
  ],
};

/**
 * Generate a unique ID for a log source
 */
export function generateLogSourceId(): string {
  return `log-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

/**
 * Create a new log source with default values
 */
export function createLogSource(partial: Partial<LogSource> = {}): LogSource {
  return {
    id: generateLogSourceId(),
    name: "New Log Source",
    type: "file",
    path: "",
    enabled: true,
    tailLines: 100,
    ...partial,
  };
}

/**
 * Create a new project log config with default values
 */
export function createProjectLogConfig(
  projectId: string,
  projectName: string,
  baseDir: string
): ProjectLogConfig {
  return {
    projectId,
    projectName,
    logSources: [],
    logDirectory: `${baseDir}/logs`,
    screenshotDirectory: `${baseDir}/screenshots`,
    aiOutputDirectory: `${baseDir}/ai-output`,
    updatedAt: new Date().toISOString(),
  };
}
