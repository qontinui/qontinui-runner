/**
 * Shared style constants and components for issue display.
 * Used by both GlobalIssuesPanel and SpecIssuesPanel.
 */

export const SEVERITY_STYLES: Record<string, string> = {
  critical: "bg-red-500/15 text-red-400 border-red-500/30",
  high: "bg-orange-500/15 text-orange-400 border-orange-500/30",
  medium: "bg-amber-500/15 text-amber-400 border-amber-500/30",
  low: "bg-blue-500/15 text-blue-400 border-blue-500/30",
};

export const CATEGORY_STYLES: Record<string, string> = {
  duplication: "bg-purple-500/15 text-purple-400 border-purple-500/30",
  rendering: "bg-cyan-500/15 text-cyan-400 border-cyan-500/30",
  data_integrity: "bg-green-500/15 text-green-400 border-green-500/30",
  timing: "bg-yellow-500/15 text-yellow-400 border-yellow-500/30",
  state: "bg-indigo-500/15 text-indigo-400 border-indigo-500/30",
  layout: "bg-pink-500/15 text-pink-400 border-pink-500/30",
  performance: "bg-orange-500/15 text-orange-400 border-orange-500/30",
  encoding: "bg-teal-500/15 text-teal-400 border-teal-500/30",
  navigation: "bg-sky-500/15 text-sky-400 border-sky-500/30",
  authentication: "bg-red-500/15 text-red-400 border-red-500/30",
  other: "bg-gray-500/15 text-gray-400 border-gray-500/30",
};

export const STATUS_STYLES: Record<string, string> = {
  active: "bg-amber-500/15 text-amber-400 border-amber-500/30",
  resolved: "bg-green-500/15 text-green-400 border-green-500/30",
  monitoring: "bg-blue-500/15 text-blue-400 border-blue-500/30",
  wont_fix: "bg-gray-500/15 text-gray-400 border-gray-500/30",
};
