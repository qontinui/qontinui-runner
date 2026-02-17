/**
 * Shared Step Icon Configuration
 *
 * This module provides consistent icon configuration for step types
 * across the application (Workflow Builder, Recap Page, etc.)
 */

import {
  FileCode,
  Navigation,
  GitBranch,
  MousePointer2,
  Globe,
  Terminal,
  TestTube2,
  Eye,
  Code,
  Package,
  Camera,
  MessageSquare,
  AlertTriangle,
  FileType,
  Search,
  CheckCircle,
  List,
  Play,
  FileSearch,
  Activity,
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
  // Setup/scripting steps
  script: {
    icon: FileCode,
    bgClass: "bg-emerald-500/10",
    textClass: "text-emerald-400",
  },
  state: {
    icon: Navigation,
    bgClass: "bg-blue-500/10",
    textClass: "text-blue-400",
  },
  workflow: {
    icon: GitBranch,
    bgClass: "bg-purple-500/10",
    textClass: "text-purple-400",
  },
  workflow_ref: {
    icon: GitBranch,
    bgClass: "bg-purple-500/10",
    textClass: "text-purple-400",
  },

  // Interaction steps
  gui_action: {
    icon: MousePointer2,
    bgClass: "bg-orange-500/10",
    textClass: "text-orange-400",
  },
  action: {
    icon: MousePointer2,
    bgClass: "bg-orange-500/10",
    textClass: "text-orange-400",
  },
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
  api_request: {
    icon: Globe,
    bgClass: "bg-cyan-500/10",
    textClass: "text-cyan-400",
  },

  // Verification/testing steps
  test: {
    icon: TestTube2,
    bgClass: "bg-green-500/10",
    textClass: "text-green-400",
  },
  test_playwright: {
    icon: TestTube2,
    bgClass: "bg-green-500/10",
    textClass: "text-green-400",
  },
  test_vision: {
    icon: Eye,
    bgClass: "bg-green-500/10",
    textClass: "text-green-400",
  },
  test_python: {
    icon: Code,
    bgClass: "bg-green-500/10",
    textClass: "text-green-400",
  },
  test_repository: {
    icon: Package,
    bgClass: "bg-green-500/10",
    textClass: "text-green-400",
  },
  screenshot: {
    icon: Camera,
    bgClass: "bg-pink-500/10",
    textClass: "text-pink-400",
  },
  check: {
    icon: AlertTriangle,
    bgClass: "bg-cyan-500/10",
    textClass: "text-cyan-400",
  },
  check_lint: {
    icon: AlertTriangle,
    bgClass: "bg-cyan-500/10",
    textClass: "text-cyan-400",
  },
  check_typecheck: {
    icon: FileType,
    bgClass: "bg-cyan-500/10",
    textClass: "text-cyan-400",
  },
  check_build: {
    icon: Package,
    bgClass: "bg-cyan-500/10",
    textClass: "text-cyan-400",
  },
  log_watch: {
    icon: FileSearch,
    bgClass: "bg-cyan-500/10",
    textClass: "text-cyan-400",
  },
  error_resolved: {
    icon: CheckCircle,
    bgClass: "bg-green-500/10",
    textClass: "text-green-400",
  },
  check_group: {
    icon: CheckCircle,
    bgClass: "bg-cyan-500/10",
    textClass: "text-cyan-400",
  },
  check_ci_cd: {
    icon: GitBranch,
    bgClass: "bg-purple-500/10",
    textClass: "text-purple-400",
  },

  // AI steps - use MessageSquare (matching "prompt" in Workflows builder)
  prompt: {
    icon: MessageSquare,
    bgClass: "bg-amber-500/10",
    textClass: "text-amber-400",
  },
  ai_session: {
    icon: MessageSquare,
    bgClass: "bg-amber-500/10",
    textClass: "text-amber-400",
  },
  ai_analysis: {
    icon: MessageSquare,
    bgClass: "bg-amber-500/10",
    textClass: "text-amber-400",
  },

  // AWAS step types
  awas_discover: {
    icon: Search,
    bgClass: "bg-teal-500/10",
    textClass: "text-teal-400",
  },
  awas_execute: {
    icon: Play,
    bgClass: "bg-teal-500/10",
    textClass: "text-teal-400",
  },
  awas_check_support: {
    icon: CheckCircle,
    bgClass: "bg-teal-500/10",
    textClass: "text-teal-400",
  },
  awas_list_actions: {
    icon: List,
    bgClass: "bg-teal-500/10",
    textClass: "text-teal-400",
  },
  awas_extract_elements: {
    icon: FileSearch,
    bgClass: "bg-teal-500/10",
    textClass: "text-teal-400",
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
 * @param stepType - The step type identifier (e.g., "workflow", "ai_session", "check_lint")
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
