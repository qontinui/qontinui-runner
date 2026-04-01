import React from "react";

interface DomainInfo {
  domain: string;
  criteria_count: number;
  criteria_ids: string[];
}

interface DomainBreakdownProps {
  domains: DomainInfo[];
  complexityLevel: string;
}

const DOMAIN_COLORS: Record<string, string> = {
  compilation: "bg-blue-500",
  api_endpoint: "bg-green-500",
  ui_content: "bg-purple-500",
  database_state: "bg-orange-500",
  security: "bg-red-500",
  performance: "bg-yellow-500",
  integration: "bg-teal-500",
  ci_cd: "bg-indigo-500",
  general: "bg-zinc-500",
};

export const DomainBreakdown: React.FC<DomainBreakdownProps> = ({ domains, complexityLevel }) => {
  const total = domains.reduce((sum, d) => sum + d.criteria_count, 0);

  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700 p-4 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium text-zinc-900 dark:text-zinc-100">Domain Breakdown</h3>
        <span
          className={`text-xs font-mono px-2 py-0.5 rounded ${
            complexityLevel === "complex"
              ? "bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400"
              : complexityLevel === "moderate"
                ? "bg-yellow-100 text-yellow-700 dark:bg-yellow-900/30 dark:text-yellow-400"
                : "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400"
          }`}
        >
          {complexityLevel}
        </span>
      </div>

      {/* Domain bar */}
      <div className="flex h-3 rounded-full overflow-hidden">
        {domains.map((d) => (
          <div
            key={d.domain}
            className={`${DOMAIN_COLORS[d.domain] || "bg-zinc-400"} transition-all`}
            style={{ width: `${(d.criteria_count / total) * 100}%` }}
            title={`${d.domain}: ${d.criteria_count} criteria`}
          />
        ))}
      </div>

      {/* Domain list */}
      <div className="space-y-1.5">
        {domains.map((d) => (
          <div key={d.domain} className="flex items-center gap-2 text-sm">
            <div className={`w-2.5 h-2.5 rounded-sm ${DOMAIN_COLORS[d.domain] || "bg-zinc-400"}`} />
            <span className="text-zinc-700 dark:text-zinc-300 flex-1">
              {d.domain.replace(/_/g, " ")}
            </span>
            <span className="text-zinc-500 dark:text-zinc-400 font-mono text-xs">
              {d.criteria_count}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
};
