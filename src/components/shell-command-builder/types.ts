/**
 * Shell Command Builder Types
 *
 * TypeScript types for the shell command management system.
 */

// Shell command stored in database
export interface ShellCommand {
  id: string;
  name: string;
  description?: string;
  command: string;
  working_directory?: string;
  timeout_seconds: number;
  fail_on_error: boolean;
  category: string;
  tags: string[];
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

// Input for creating a new shell command
export interface CreateShellCommandInput {
  name: string;
  description?: string;
  command: string;
  working_directory?: string;
  timeout_seconds?: number;
  fail_on_error?: boolean;
  category?: string;
  tags?: string[];
}

// Input for updating an existing shell command
export interface UpdateShellCommandInput {
  name?: string;
  description?: string;
  command?: string;
  working_directory?: string;
  timeout_seconds?: number;
  fail_on_error?: boolean;
  category?: string;
  tags?: string[];
  enabled?: boolean;
}

// Result of executing a shell command
export interface ExecuteShellCommandResult {
  success: boolean;
  exit_code?: number;
  stdout: string;
  stderr: string;
  duration_ms: number;
}

// Command response from Tauri
export interface CommandResponse<T = unknown> {
  success: boolean;
  message?: string;
  data?: T;
}

// Category metadata for UI display
export interface ShellCommandCategoryInfo {
  id: string;
  name: string;
  icon: string;
  color: string;
}

// Shell command categories
export const SHELL_COMMAND_CATEGORIES: ShellCommandCategoryInfo[] = [
  { id: "git", name: "Git", icon: "GitBranch", color: "orange" },
  { id: "npm", name: "npm/Node", icon: "Package", color: "green" },
  { id: "poetry", name: "Poetry/Python", icon: "FileCode", color: "blue" },
  { id: "docker", name: "Docker", icon: "Container", color: "cyan" },
  { id: "build", name: "Build", icon: "Hammer", color: "amber" },
  { id: "general", name: "General", icon: "Terminal", color: "gray" },
];

// Helper to get category info
export function getCategoryInfo(categoryId: string): ShellCommandCategoryInfo | undefined {
  return SHELL_COMMAND_CATEGORIES.find((c) => c.id === categoryId);
}

// Helper to get category color class
export function getCategoryColorClass(categoryId: string): string {
  const category = getCategoryInfo(categoryId);
  if (!category) return "text-gray-400";

  const colorMap: Record<string, string> = {
    orange: "text-orange-400",
    green: "text-green-400",
    blue: "text-blue-400",
    cyan: "text-cyan-400",
    amber: "text-amber-400",
    gray: "text-gray-400",
  };

  return colorMap[category.color] || "text-gray-400";
}

// Helper to get category background color class
export function getCategoryBgClass(categoryId: string): string {
  const category = getCategoryInfo(categoryId);
  if (!category) return "bg-gray-500/10";

  const colorMap: Record<string, string> = {
    orange: "bg-orange-500/10",
    green: "bg-green-500/10",
    blue: "bg-blue-500/10",
    cyan: "bg-cyan-500/10",
    amber: "bg-amber-500/10",
    gray: "bg-gray-500/10",
  };

  return colorMap[category.color] || "bg-gray-500/10";
}
