/**
 * Widget Registry System
 *
 * Manages registration and detection of dashboard widgets.
 * Widgets are selected at runtime based on the activities present in the current task.
 */

import type { ComponentType } from "react";
import type { ActivityType } from "./activity-types";
import type { BaseWidgetProps } from "./widget-props";

/**
 * Information about a task used to detect which activities/widgets to show.
 */
export interface TaskActivityInfo {
  /** Unique task ID */
  taskId: string | null;
  /** Type of task */
  taskType: "task" | "automation" | "scheduled" | null;
  /** Display name for the task */
  taskName: string | null;
  /** Whether the task has a qontinui config (state machine) */
  hasConfig: boolean;
  /** Whether the task has an AI prompt */
  hasPrompt: boolean;
  /** Whether the task includes Playwright scripts */
  hasPlaywrightScripts: boolean;
  /** Whether the task includes verification tests */
  hasVerificationTests: boolean;
  /** Workflow name if applicable */
  workflowName: string | null;
  /** Current iteration (1-based) */
  iteration: number;
  /** Maximum iterations */
  maxIterations: number;
  /** Explicitly detected activity types */
  activities: ActivityType[];
}

/**
 * Configuration for a widget type.
 *
 * @template TData The type of data the widget receives
 */
export interface WidgetConfig<TData = unknown> {
  /** Unique identifier matching ActivityType */
  id: ActivityType;
  /** Display name for the widget */
  displayName: string;
  /** Icon name from lucide-react */
  icon: string;
  /** Accent color for highlights */
  accentColor: string;
  /** The full widget component (shown when active) */
  FullComponent: ComponentType<BaseWidgetProps & { data: TData }>;
  /** The summary widget component (shown in sidebar) */
  SummaryComponent: ComponentType<BaseWidgetProps & { data: TData }>;
  /** Hook to fetch/subscribe to data for this widget */
  useData: () => TData;
  /** Function to detect if this activity should be shown for a task */
  detectActivity: (taskInfo: TaskActivityInfo) => boolean;
  /** Default priority (lower = higher priority, shown first) */
  defaultPriority: number;
  /** Route to detail page for "View All" links */
  detailRoute: string;
}

/**
 * Widget Registry - Singleton for managing widget registrations.
 *
 * Usage:
 * ```ts
 * // Register a widget
 * widgetRegistry.register({
 *   id: "gui_automation",
 *   displayName: "GUI Automation",
 *   ...
 * });
 *
 * // Detect widgets for a task
 * const widgets = widgetRegistry.detectWidgets(taskInfo);
 * ```
 */
class WidgetRegistryImpl {
  private widgets = new Map<ActivityType, WidgetConfig>();
  private initialized = false;

  /**
   * Register a widget configuration.
   */
  register<TData>(config: WidgetConfig<TData>): void {
    this.widgets.set(config.id, config as WidgetConfig);
  }

  /**
   * Get a widget configuration by activity type.
   */
  get(id: ActivityType): WidgetConfig | undefined {
    return this.widgets.get(id);
  }

  /**
   * Get all registered widget configurations.
   */
  getAll(): WidgetConfig[] {
    return Array.from(this.widgets.values());
  }

  /**
   * Get all registered activity types.
   */
  getRegisteredTypes(): ActivityType[] {
    return Array.from(this.widgets.keys());
  }

  /**
   * Detect which widgets should be shown for a task.
   * Returns activity types sorted by priority.
   */
  detectWidgets(taskInfo: TaskActivityInfo): ActivityType[] {
    const detected: ActivityType[] = [];

    for (const config of this.widgets.values()) {
      if (config.detectActivity(taskInfo)) {
        detected.push(config.id);
      }
    }

    // Sort by priority (lower = first)
    return detected.sort((a, b) => {
      const priorityA = this.widgets.get(a)?.defaultPriority ?? 999;
      const priorityB = this.widgets.get(b)?.defaultPriority ?? 999;
      return priorityA - priorityB;
    });
  }

  /**
   * Check if the registry has been initialized with widgets.
   */
  isInitialized(): boolean {
    return this.initialized;
  }

  /**
   * Mark the registry as initialized.
   * Called after all widgets have been registered.
   */
  markInitialized(): void {
    this.initialized = true;
  }

  /**
   * Clear all registrations (useful for testing).
   */
  clear(): void {
    this.widgets.clear();
    this.initialized = false;
  }
}

/**
 * Singleton widget registry instance.
 */
export const widgetRegistry = new WidgetRegistryImpl();

/**
 * Default activity detection functions.
 * These can be used when registering widgets.
 */
export const defaultDetectors: Record<ActivityType, (info: TaskActivityInfo) => boolean> = {
  gui_automation: (info) => {
    return info.hasConfig || info.activities.includes("gui_automation");
  },
  playwright: (info) => {
    return info.hasPlaywrightScripts || info.activities.includes("playwright");
  },
  ai_conversation: (info) => {
    // Show AI conversation for tasks with prompts or explicitly marked
    return info.hasPrompt || info.activities.includes("ai_conversation");
  },
  verification: (info) => {
    // Show verification if there are verification tests or the task includes it
    return info.hasVerificationTests || info.activities.includes("verification");
  },
  findings: (info) => {
    // Findings are always potentially present for AI tasks
    return info.hasPrompt || info.activities.includes("findings");
  },
  execution_status: (info) => {
    // Always show execution status when there's an active task
    return info.taskId !== null || info.activities.includes("execution_status");
  },
  shell_command: (info) => {
    // Show shell command widget when shell commands are being executed
    return info.activities.includes("shell_command");
  },
  api_request: (info) => {
    // Show API request widget when API requests are being made
    return info.activities.includes("api_request");
  },
  script: (info) => {
    // Show script widget when scripts are being executed
    return info.activities.includes("script");
  },
  workflow_ref: (info) => {
    // Show workflow ref widget when sub-workflows are being executed
    return info.activities.includes("workflow_ref");
  },
};
