import { FileText } from "lucide-react";
import type { MainTabId } from "./tab-types";

interface LogSource {
  id: string;
  name: string;
  path: string;
  enabled: boolean;
  color?: string;
  category: string;
  description?: string;
  tail_lines: number;
}

interface LogSourcesConfigTabProps {
  sources: LogSource[];
  onNavigate: (tab: MainTabId) => void;
}

export function LogSourcesConfigTab({ sources, onNavigate }: LogSourcesConfigTabProps) {
  return (
    <div className="h-full flex flex-col overflow-hidden">
      <div className="flex items-center justify-between px-6 py-3 border-b border-border shrink-0">
        <div className="flex items-center gap-2">
          <FileText className="w-4 h-4 text-muted-foreground" />
          <h1 className="text-lg font-semibold">Log Sources</h1>
          <span className="text-sm text-muted-foreground">
            External log files configured for monitoring
          </span>
        </div>
        <button
          onClick={() => onNavigate("settings-log-sources")}
          className="flex items-center gap-2 px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors"
        >
          Configure Sources
        </button>
      </div>
      <div className="flex-1 overflow-y-auto p-6">
        {sources.length === 0 ? (
          <div className="text-center py-12 text-muted-foreground">
            <FileText className="w-12 h-12 mx-auto mb-4 opacity-50" />
            <p className="text-lg font-medium mb-2">No Log Sources Configured</p>
            <p className="text-sm mb-4">Add external log files to monitor your applications</p>
            <button
              onClick={() => onNavigate("settings-log-sources")}
              className="px-4 py-2 bg-primary text-primary-foreground rounded-md hover:bg-primary/90"
            >
              Configure Log Sources
            </button>
          </div>
        ) : (
          <div className="space-y-3">
            {sources.map((source) => (
              <div
                key={source.id}
                className="flex items-center gap-4 p-4 bg-card border border-border rounded-lg"
                style={{ borderLeftWidth: "4px", borderLeftColor: source.color || "#6b7280" }}
              >
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 mb-1">
                    <span className="font-medium">{source.name}</span>
                    <span
                      className={`text-xs px-2 py-0.5 rounded ${source.enabled ? "bg-green-500/20 text-green-500" : "bg-muted text-muted-foreground"}`}
                    >
                      {source.enabled ? "Enabled" : "Disabled"}
                    </span>
                    <span className="text-xs px-2 py-0.5 bg-muted text-muted-foreground rounded">
                      {source.category}
                    </span>
                  </div>
                  <p className="text-sm text-muted-foreground truncate" title={source.path}>
                    {source.path}
                  </p>
                  {source.description && (
                    <p className="text-xs text-muted-foreground mt-1">{source.description}</p>
                  )}
                </div>
                <div className="text-xs text-muted-foreground">{source.tail_lines} lines</div>
              </div>
            ))}
          </div>
        )}

        {sources.length > 0 && (
          <div className="mt-6 pt-6 border-t border-border">
            <p className="text-sm text-muted-foreground">
              To view log content during workflow runs, use the{" "}
              <button
                onClick={() => onNavigate("run-recap")}
                className="text-primary hover:underline"
              >
                Session Summary
              </button>
              .
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
