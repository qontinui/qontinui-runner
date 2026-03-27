/** Types for the Session Recap feature — mirrors the Rust backend structs. */

export interface SessionRecapRequest {
  lookback?: string;
  repos?: string[];
}

export interface SessionRecap {
  timespan: TimeSpan;
  repos_touched: RepoSummary[];
  files_created: FileChange[];
  files_modified: FileChange[];
  files_deleted: FileChange[];
  types_defined: TypeDefinition[];
  endpoints_added: EndpointInfo[];
  database_changes: DbChange[];
  ui_components: ComponentInfo[];
  dependency_graph: DependencyEdge[];
  summary: RecapSummary;
}

export interface TimeSpan {
  start: string;
  end: string;
  lookback_spec: string;
}

export interface RepoSummary {
  name: string;
  files_changed: number;
  lines_added: number;
  lines_removed: number;
}

export interface FileChange {
  path: string;
  repo: string;
  language: string;
  change_type: "created" | "modified" | "deleted";
  lines_added: number;
  lines_removed: number;
  category: string;
}

export interface TypeDefinition {
  name: string;
  kind: string;
  file: string;
  repo: string;
  language: string;
}

export interface EndpointInfo {
  path: string;
  method: string;
  file: string;
  repo: string;
}

export interface DbChange {
  table_name: string;
  change_type: string;
  file: string;
  repo: string;
}

export interface ComponentInfo {
  name: string;
  file: string;
  repo: string;
  component_type: string;
}

export interface DependencyEdge {
  from_file: string;
  to_file: string;
  relationship: string;
  cross_language: boolean;
}

export interface RecapSummary {
  total_files: number;
  total_repos: number;
  total_lines_added: number;
  total_lines_removed: number;
  new_types: number;
  new_endpoints: number;
  new_tables: number;
  new_components: number;
  categories: Record<string, number>;
}
