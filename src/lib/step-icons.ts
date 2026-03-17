/**
 * Shared Step Icon Configuration
 *
 * This module provides consistent icon configuration for step types
 * across the application (Workflow Builder, Recap Page, etc.)
 *
 * Core step types: command, prompt, test, ui_bridge
 */

import {
  Terminal,
  TestTube2,
  MessageSquare,
  AlertTriangle,
  CheckCircle,
  Monitor,
  Activity,
  Workflow,
  Wrench,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";

export interface StepIconConfig {
  icon: LucideIcon;
  bgClass: string;
  textClass: string;
}

/**
 * Icon configuration for step types.
 *
 * Maps step type identifiers to their icon and color configuration.
 * Used by both the Workflow Builder and Recap page for consistent styling.
 */
export const STEP_ICON_CONFIG: Record<string, StepIconConfig> = {
  // Command steps (unified)
  command: {
    icon: Terminal,
    bgClass: "bg-gray-500/10",
    textClass: "text-gray-400",
  },
  // Legacy aliases for backward compatibility in dashboard widgets
  shell_command: {
    icon: Terminal,
    bgClass: "bg-gray-500/10",
    textClass: "text-gray-400",
  },
  shell: {
    icon: Terminal,
    bgClass: "bg-gray-500/10",
    textClass: "text-gray-400",
  },
  check: {
    icon: AlertTriangle,
    bgClass: "bg-cyan-500/10",
    textClass: "text-cyan-400",
  },
  check_group: {
    icon: CheckCircle,
    bgClass: "bg-cyan-500/10",
    textClass: "text-cyan-400",
  },

  // AI steps
  prompt: {
    icon: MessageSquare,
    bgClass: "bg-amber-500/10",
    textClass: "text-amber-400",
  },

  // Verification/testing steps
  test: {
    icon: TestTube2,
    bgClass: "bg-green-500/10",
    textClass: "text-green-400",
  },

  // UI Bridge steps
  ui_bridge: {
    icon: Monitor,
    bgClass: "bg-emerald-500/10",
    textClass: "text-emerald-400",
  },

  // Fixup/validation steps
  workflow_fixup: {
    icon: Wrench,
    bgClass: "bg-violet-500/10",
    textClass: "text-violet-400",
  },

  // Workflow composition steps
  workflow: {
    icon: Workflow,
    bgClass: "bg-blue-500/10",
    textClass: "text-blue-400",
  },
};

/**
 * Default icon configuration used when step type is not recognized.
 */
const DEFAULT_CONFIG: StepIconConfig = {
  icon: Activity,
  bgClass: "bg-zinc-500/10",
  textClass: "text-zinc-400",
};

/**
 * Get icon configuration for a step type.
 *
 * @param stepType - The step type identifier (e.g., "command", "test", "ui_bridge", "prompt")
 * @returns The icon configuration for rendering the step
 */
export function getStepIconConfig(stepType: string): StepIconConfig {
  return STEP_ICON_CONFIG[stepType] || DEFAULT_CONFIG;
}

/**
 * Get icon configuration by combining step_type and additional context.
 *
 * This allows for more specific icons when icon_type is provided separately,
 * falling back to step_type for backward compatibility.
 *
 * @param iconType - The specific icon type (if available)
 * @param stepType - The general step type
 * @returns The icon configuration
 */
export function getStepIconConfigWithFallback(
  iconType: string | undefined,
  stepType: string,
): StepIconConfig {
  if (iconType && STEP_ICON_CONFIG[iconType]) {
    return STEP_ICON_CONFIG[iconType];
  }
  return STEP_ICON_CONFIG[stepType] || DEFAULT_CONFIG;
}
