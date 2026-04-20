import React, { useEffect, useState, useCallback } from "react";
import { tracedFetch } from "@/lib/runner-api";

interface StepTemplate {
  id: string;
  domain: string;
  pattern_description: string;
  success_count: number;
  failure_count: number;
  confidence: number;
  source: string;
}

interface TemplateLibraryPanelProps {
  domain?: string;
  apiBase: string;
}

export const TemplateLibraryPanel: React.FC<TemplateLibraryPanelProps> = ({ domain, apiBase }) => {
  const [templates, setTemplates] = useState<StepTemplate[]>([]);
  const [loading, setLoading] = useState(false);

  const fetchTemplates = useCallback(async () => {
    setLoading(true);
    try {
      const url = domain
        ? `${apiBase}/generation/templates?domain=${domain}`
        : `${apiBase}/generation/templates`;
      const res = await tracedFetch(url);
      if (res.ok) {
        const data = await res.json();
        setTemplates(data.data || []);
      }
    } catch {
      // silently fail
    } finally {
      setLoading(false);
    }
  }, [domain, apiBase]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    fetchTemplates();
  }, [fetchTemplates]);

  if (loading) {
    return <div className="p-4 text-sm text-zinc-500 dark:text-zinc-400">Loading templates...</div>;
  }

  if (templates.length === 0) {
    return (
      <div className="p-4 text-sm text-zinc-500 dark:text-zinc-400">
        No templates available{domain ? ` for ${domain}` : ""}.
      </div>
    );
  }

  return (
    <div className="space-y-2">
      <h3 className="text-sm font-medium text-zinc-900 dark:text-zinc-100 px-4 pt-3">
        Template Library ({templates.length})
      </h3>
      <div className="max-h-64 overflow-y-auto divide-y divide-zinc-100 dark:divide-zinc-800">
        {templates.map((t) => (
          <div key={t.id} className="px-4 py-2 hover:bg-zinc-50 dark:hover:bg-zinc-800/50">
            <div className="flex items-center justify-between">
              <span className="text-sm text-zinc-900 dark:text-zinc-100 truncate">
                {t.pattern_description}
              </span>
              <span
                className={`text-xs font-mono px-1.5 py-0.5 rounded ${
                  t.confidence >= 0.8
                    ? "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400"
                    : t.confidence >= 0.5
                      ? "bg-yellow-100 text-yellow-700 dark:bg-yellow-900/30 dark:text-yellow-400"
                      : "bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400"
                }`}
              >
                {(t.confidence * 100).toFixed(0)}%
              </span>
            </div>
            <div className="flex gap-3 mt-1 text-xs text-zinc-500 dark:text-zinc-400">
              <span>{t.domain}</span>
              <span>
                {t.success_count}&#10003; / {t.failure_count}&#10007;
              </span>
              <span>{t.source}</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
};
