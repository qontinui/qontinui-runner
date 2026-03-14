// Types for the UI Bridge Integration page

export type Framework =
  | "react"
  | "nextjs"
  | "vue"
  | "angular"
  | "svelte"
  | "plain_html"
  | "unknown";

export type PackageManager = "npm" | "yarn" | "pnpm" | "bun" | "unknown";

export type IntegrationStatus = "none" | "partial" | "full";

export type IntegrationType = "source";

export type IntegrationHealthStatus = "active" | "disconnected" | "outdated";

// --- Project Analyzer ---

export interface EntryPoint {
  path: string;
  entry_type: "app_root" | "layout" | "index_html" | "main";
}

export interface ProjectAnalysis {
  project_path: string;
  framework: Framework;
  package_manager: PackageManager;
  entry_points: EntryPoint[];
  ui_bridge_status: IntegrationStatus;
  existing_sdk_version: string | null;
  has_babel_plugin: boolean;
  has_swc_plugin: boolean;
  server_adapter: string | null;
  dev_server_port: number | null;
  issues: string[];
}

// --- Source Integration ---

export type ModificationType = "insert" | "replace" | "create_new";

export interface FileModification {
  file_path: string;
  modification_type: ModificationType;
  description: string;
  original_content: string | null;
  new_content: string;
}

export interface IntegrationOptions {
  install_deps: boolean;
  /** @deprecated Auto-instrumentation is no longer needed. The SDK's AutoRegisterProvider
   * generates stable semantic IDs at runtime, eliminating the need for build-time Babel plugins. */
  auto_instrument?: boolean;
  sdk_version?: string;
}

export interface IntegrationResult {
  success: boolean;
  modifications: FileModification[];
  install_output: string | null;
  warnings: string[];
  next_steps: string[];
}

// --- Integration Status Tracker ---

export interface TrackedIntegration {
  id: string;
  project_path: string;
  label: string | null;
  framework: string | null;
  integration_type: IntegrationType;
  sdk_version: string | null;
  status: IntegrationHealthStatus;
  target_url: string | null;
  last_health_check: number | null;
  element_count: number | null;
  created_at: number;
  updated_at: number;
}

// --- Discovery (reuses existing app_discovery types) ---

export interface DiscoveredApp {
  app_id: string;
  app_name: string;
  app_type: string;
  framework: string | null;
  url: string;
  port: number;
  base_path: string;
  version: string | null;
  capabilities: string[];
  element_count: number | null;
  component_count: number | null;
  discovered_at: number;
}

// --- API Response wrapper ---

export interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}
