/**
 * RuleInfluencePanel.tsx
 *
 * Displays rules that appear to have no effect (ineffective rules),
 * helping identify knowledge base entries that may need updating or removal.
 */

import { BookOpen, AlertTriangle, ShieldCheck } from "lucide-react";
import { useIneffectiveRules, type IneffectiveRule } from "../../hooks/useGraphAnalytics";

function RuleRow({ rule }: { rule: IneffectiveRule }) {
  const effectRatio =
    rule.total_loaded_count > 0
      ? ((rule.prevented_error_count / rule.total_loaded_count) * 100).toFixed(0)
      : "0";

  return (
    <div className="flex items-center justify-between py-2 border-b border-border last:border-0">
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm font-mono truncate">{rule.rule_id.slice(0, 16)}</span>
          {rule.no_effect_count >= 5 && (
            <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs bg-orange-500/10 text-orange-400">
              <AlertTriangle className="w-3 h-3" />
              high no-effect
            </span>
          )}
        </div>
        <div className="flex items-center gap-3 mt-0.5 text-xs text-muted-foreground">
          <span>Loaded {rule.total_loaded_count}x</span>
          <span>No effect {rule.no_effect_count}x</span>
          <span>Prevented {rule.prevented_error_count}x</span>
        </div>
      </div>
      <div className="text-right">
        <span
          className={`text-sm font-semibold ${
            Number(effectRatio) > 50 ? "text-green-400" : "text-orange-400"
          }`}
        >
          {effectRatio}%
        </span>
        <div className="text-xs text-muted-foreground">effective</div>
      </div>
    </div>
  );
}

export function RuleInfluencePanel() {
  const { data: rules, isLoading, error } = useIneffectiveRules();

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <h3 className="font-semibold mb-4 flex items-center gap-2">
        <BookOpen className="w-4 h-4" />
        Rule Influence
      </h3>

      {isLoading && (
        <div className="text-center text-muted-foreground py-8">Loading rules...</div>
      )}

      {error && (
        <div className="text-center text-destructive py-8">
          Failed to load rules: {String(error)}
        </div>
      )}

      {rules && rules.length === 0 && (
        <div className="text-center text-muted-foreground py-8">
          <ShieldCheck className="w-6 h-6 mx-auto mb-2 opacity-50" />
          All rules appear effective
        </div>
      )}

      {rules && rules.length > 0 && (
        <div className="divide-y divide-border">
          {rules.map((rule) => (
            <RuleRow key={rule.rule_id} rule={rule} />
          ))}
        </div>
      )}
    </div>
  );
}
