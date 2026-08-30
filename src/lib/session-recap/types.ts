/** Types for the Session Recap feature — mirrors the Rust backend structs. */

export interface SessionRecapRequest {
  lookback?: string;
  repos?: string[];
}

export interface SessionRecap {
  timespan: TimeSpan;
  /**
   * `true` ⇒ this recap is PARTIAL: the aggregate git budget actually cost the
   * scan something. The backend has published it since the budget landed and
   * nothing here read it, so a truncated scan and a quiet one rendered
   * identically — and "nothing changed" is the wrong conclusion to draw from a
   * scan that was cut off.
   *
   * Exactly `repos_skipped.length > 0`.
   */
  git_budget_exhausted: boolean;
  /** The repos the budget cost the scan something on, each typed with which. */
  repos_skipped: RepoScanGap[];
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

/**
 * Which kind of hole the git budget left in one repo's scan.
 *
 * * `not-started` — no git child ran for this repo at all. It is absent from
 *   `repos_touched`, from the file lists, and from every count: its absence
 *   says nothing about whether it changed.
 * * `cut-short` — the scan started and was refused a child partway through, so
 *   this repo IS in `repos_touched` and in `summary`, with partial numbers.
 *   Empty type / endpoint / table lists for it do not mean there were none.
 */
export type RepoScanGapState = "not-started" | "cut-short";

export interface RepoScanGap {
  repo: string;
  state: RepoScanGapState;
  /** One operator-facing sentence for `state`, supplied by the backend. */
  detail: string;
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

  /**
   * `false` ⇒ PARTIAL. Carried INSIDE the summary because the summary is the
   * part that gets detached and persisted ("Save to Memory" writes
   * `JSON.stringify(recap.summary)`); without it, a truncated scan is filed in
   * the observations store looking complete, forever.
   */
  scan_complete: boolean;
  /** Repos no git child ran for — missing from every list in this recap. */
  repos_not_started: number;
  /** Repos scanned only in part — counted in `total_repos`, incomplete. */
  repos_cut_short: number;
}
