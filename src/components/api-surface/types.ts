/** API Surface scan response types (mirrors Rust structs) */

export interface ApiSurface {
  tauriCommands: TauriCommand[];
  mcpRoutes: McpRoute[];
  uiBridgeRoutes: UiBridgeRoute[];
  pythonEvents: PythonEvent[];
  pgMethods: PgMethod[];
  clorindeQueries: ClorindeQuery[];
  dbTables: DbTable[];
  connections: ApiConnection[];
  orphans: OrphanedEndpoint[];
  summary: ScanSummary;
}

export interface TauriCommand {
  name: string;
  file: string;
  line: number;
  parameters: string[];
  returnType: string;
  callers: Caller[];
}

export interface McpRoute {
  method: string;
  path: string;
  handler: string;
  file: string;
  line: number;
  callers: Caller[];
}

export interface UiBridgeRoute {
  path: string;
  method: string;
  file: string;
  line: number;
}

export interface PythonEvent {
  name: string;
  value: string;
  file: string;
  line: number;
  interceptedBy: Caller[];
}

export interface PgMethod {
  name: string;
  file: string;
  line: number;
  clorindeQuery: string | null;
  callers: Caller[];
}

export interface ClorindeQuery {
  name: string;
  file: string;
  line: number;
  sqlPreview: string;
  table: string | null;
  wrapper: string | null;
}

export interface DbTable {
  name: string;
  file: string;
  line: number;
  columnCount: number;
}

export interface ApiConnection {
  fromType: string;
  fromName: string;
  toType: string;
  toName: string;
  connectionType: string;
}

export interface OrphanedEndpoint {
  endpointType: string;
  name: string;
  file: string;
  line: number;
  reason: string;
}

export interface Caller {
  file: string;
  line: number;
  context: string;
}

export interface ScanSummary {
  totalTauriCommands: number;
  totalMcpRoutes: number;
  totalPgMethods: number;
  totalClorindeQueries: number;
  totalDbTables: number;
  totalPythonEvents: number;
  totalConnections: number;
  totalOrphans: number;
  scanDurationMs: number;
}

export interface ApiSurfaceDiff {
  addedCommands: TauriCommand[];
  removedCommands: string[];
  addedRoutes: McpRoute[];
  removedRoutes: string[];
  addedPgMethods: PgMethod[];
  removedPgMethods: string[];
  newOrphans: OrphanedEndpoint[];
  resolvedOrphans: string[];
  newConnections: ApiConnection[];
  brokenConnections: ApiConnection[];
  summary: string;
}

export interface ApiSurfaceSnapshot {
  id: number;
  totalEndpoints: number;
  orphanCount: number;
  summary: string;
  createdAt: string;
}

/** Node categories for graph layers */
export type NodeCategory =
  | "frontend"
  | "tauri_command"
  | "mcp_route"
  | "ui_bridge_route"
  | "pg_method"
  | "clorinde_query"
  | "db_table"
  | "python_event";

/** Color scheme for each node category */
export const NODE_COLORS: Record<NodeCategory, { bg: string; border: string; text: string }> = {
  frontend:        { bg: "rgba(59,130,246,0.15)",  border: "#3b82f6", text: "#93c5fd" },
  tauri_command:   { bg: "rgba(249,115,22,0.15)",  border: "#f97316", text: "#fdba74" },
  mcp_route:       { bg: "rgba(34,197,94,0.15)",   border: "#22c55e", text: "#86efac" },
  ui_bridge_route: { bg: "rgba(34,197,94,0.12)",   border: "#10b981", text: "#6ee7b7" },
  pg_method:       { bg: "rgba(168,85,247,0.15)",  border: "#a855f7", text: "#c4b5fd" },
  clorinde_query:  { bg: "rgba(6,182,212,0.15)",   border: "#06b6d4", text: "#67e8f9" },
  db_table:        { bg: "rgba(113,113,122,0.15)", border: "#71717a", text: "#a1a1aa" },
  python_event:    { bg: "rgba(234,179,8,0.15)",   border: "#eab308", text: "#fde047" },
};

export const EDGE_STYLES: Record<string, { stroke: string; dasharray?: string; animated: boolean }> = {
  invokes:    { stroke: "#3b82f6", animated: true },
  fetches:    { stroke: "#22c55e", animated: true },
  calls:      { stroke: "#a855f7", dasharray: "6 3", animated: false },
  intercepts: { stroke: "#eab308", dasharray: "4 4", animated: false },
  queries:    { stroke: "#06b6d4", dasharray: "3 3", animated: false },
};
