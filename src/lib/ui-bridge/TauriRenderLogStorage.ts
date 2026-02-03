/**
 * Tauri Render Log Storage
 *
 * Implements the ui-bridge RenderLogStorage interface to send
 * render log entries to the Tauri/Rust backend.
 */

import { invoke } from "@tauri-apps/api/core";
import type { RenderLogStorage, RenderLogEntry, RenderLogEntryType } from "ui-bridge";

/**
 * Convert ui-bridge entry to the format expected by the Rust backend
 */
interface BackendRenderLogEntry {
  id: string;
  timestamp: number;
  component: string;
  trigger: string;
  data: Record<string, unknown>;
  visible_sections?: string[];
  task_run_id?: number;
}

function convertToBackendFormat(entry: RenderLogEntry, taskRunId?: number): BackendRenderLogEntry {
  // Map ui-bridge entry type to component name
  const componentMap: Record<RenderLogEntryType, string> = {
    snapshot: "DOMSnapshot",
    change: "DOMChange",
    navigation: "Navigation",
    interaction: "Interaction",
    error: "Error",
    custom: "Custom",
  };

  return {
    id: entry.id,
    timestamp: entry.timestamp,
    component: componentMap[entry.type] || entry.type,
    trigger: (entry.metadata?.trigger as string) || entry.type,
    data: entry.data as Record<string, unknown>,
    task_run_id: taskRunId,
  };
}

/**
 * Tauri Render Log Storage
 *
 * Sends render log entries to the Rust backend via Tauri IPC.
 * Also maintains an in-memory cache for quick access.
 */
export class TauriRenderLogStorage implements RenderLogStorage {
  private entries: RenderLogEntry[] = [];
  private maxEntries: number;
  private taskRunId?: number;
  private isDev: boolean;

  constructor(options: { maxEntries?: number; taskRunId?: string | number } = {}) {
    this.maxEntries = options.maxEntries ?? 1000;
    this.taskRunId =
      typeof options.taskRunId === "string"
        ? parseInt(options.taskRunId, 10) || undefined
        : options.taskRunId;
    this.isDev = import.meta.env.DEV;
  }

  /**
   * Set the current task run ID
   */
  setTaskRunId(taskRunId: string | number | undefined): void {
    this.taskRunId =
      typeof taskRunId === "string" ? parseInt(taskRunId, 10) || undefined : taskRunId;
  }

  async append(entry: RenderLogEntry): Promise<void> {
    // Add to local cache
    this.entries.push(entry);
    while (this.entries.length > this.maxEntries) {
      this.entries.shift();
    }

    // Send to backend (only in dev mode)
    if (this.isDev) {
      try {
        const backendEntry = convertToBackendFormat(entry, this.taskRunId);
        await invoke("append_render_log", { entry: backendEntry });
      } catch (error) {
        console.debug("[TauriRenderLogStorage] Failed to send entry:", error);
      }
    }
  }

  async getEntries(options?: {
    type?: RenderLogEntryType;
    since?: number;
    until?: number;
    limit?: number;
  }): Promise<RenderLogEntry[]> {
    let results = [...this.entries];

    if (options?.type) {
      results = results.filter((e) => e.type === options.type);
    }
    if (options?.since) {
      results = results.filter((e) => e.timestamp >= options.since!);
    }
    if (options?.until) {
      results = results.filter((e) => e.timestamp <= options.until!);
    }
    if (options?.limit) {
      results = results.slice(-options.limit);
    }

    return results;
  }

  async clear(): Promise<void> {
    this.entries = [];

    if (this.isDev) {
      try {
        await invoke("clear_render_log");
      } catch (error) {
        console.debug("[TauriRenderLogStorage] Failed to clear log:", error);
      }
    }
  }

  async count(): Promise<number> {
    return this.entries.length;
  }

  /**
   * Get entries synchronously from local cache
   */
  getEntriesSync(): RenderLogEntry[] {
    return [...this.entries];
  }
}
