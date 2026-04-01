export interface MemoryResult {
  id: string;
  source: string;
  title: string;
  snippet: string;
  source_score: number;
  fused_score: number;
  timestamp: string | null;
  memory_type: string;
  found_by: string[];
}

export interface MemorySearchResponse {
  success: boolean;
  data: {
    results: MemoryResult[];
    total: number;
    query: string;
  } | null;
  error: string | null;
}

export type MemorySourceFilter =
  | "observation"
  | "timeline"
  | "knowledge"
  | "finding"
  | "fix"
  | "error"
  | "rule"
  | "graph"
  | "workflow"
  | "ui_element";

export const ALL_SOURCES: MemorySourceFilter[] = [
  "observation",
  "timeline",
  "knowledge",
  "finding",
  "fix",
  "error",
  "rule",
  "graph",
  "workflow",
  "ui_element",
];

export const SOURCE_COLORS: Record<string, string> = {
  observation: "bg-blue-500/10 text-blue-400 border-blue-500/20",
  activity_timeline: "bg-purple-500/10 text-purple-400 border-purple-500/20",
  task_knowledge: "bg-green-500/10 text-green-400 border-green-500/20",
  finding: "bg-yellow-500/10 text-yellow-400 border-yellow-500/20",
  fix: "bg-emerald-500/10 text-emerald-400 border-emerald-500/20",
  error: "bg-red-500/10 text-red-400 border-red-500/20",
  rule: "bg-cyan-500/10 text-cyan-400 border-cyan-500/20",
  graph_node: "bg-orange-500/10 text-orange-400 border-orange-500/20",
  workflow: "bg-indigo-500/10 text-indigo-400 border-indigo-500/20",
  ui_element: "bg-pink-500/10 text-pink-400 border-pink-500/20",
};

export const SOURCE_ICONS: Record<string, string> = {
  observation: "Eye",
  activity_timeline: "Clock",
  task_knowledge: "BookOpen",
  finding: "Search",
  fix: "Wrench",
  error: "AlertTriangle",
  rule: "Scale",
  graph_node: "GitBranch",
  workflow: "Workflow",
  ui_element: "Layout",
};
