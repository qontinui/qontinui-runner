/**
 * ProviderHealthStatus
 *
 * Renders a row of circuit breaker status badges for each AI provider.
 * Real-time updates via Tauri event subscription. When a circuit is open,
 * a reset button is shown to manually close it.
 */

import { Activity, AlertTriangle, CheckCircle2, RotateCcw } from "lucide-react";
import { useProviderCircuitBreakers } from "@/hooks/useProviderCircuitBreakers";
import { getAccentColors } from "@/design-system";
import type { ProviderCircuitState } from "./types";

const PROVIDER_LABELS: Record<string, string> = {
  claude_cli: "Claude CLI",
  claude_api: "Claude API",
  gemini_cli: "Gemini CLI",
  gemini_api: "Gemini API",
};

function stateColor(state: ProviderCircuitState["state"]) {
  switch (state) {
    case "Closed":
      return "green";
    case "HalfOpen":
      return "yellow";
    case "Open":
      return "red";
  }
}

function StateIcon({ state }: { state: ProviderCircuitState["state"] }) {
  switch (state) {
    case "Closed":
      return <CheckCircle2 className="w-3.5 h-3.5" />;
    case "HalfOpen":
      return <Activity className="w-3.5 h-3.5" />;
    case "Open":
      return <AlertTriangle className="w-3.5 h-3.5" />;
  }
}

export function ProviderHealthStatus() {
  const { states, loading, error, resetProvider } = useProviderCircuitBreakers();

  if (loading) {
    return null;
  }

  if (error) {
    return (
      <div className="text-xs text-muted-foreground">
        Provider health unavailable: {error}
      </div>
    );
  }

  // Only show providers that have had activity (circuit breakers are lazy-created)
  if (states.length === 0) {
    return null;
  }

  return (
    <div className="space-y-2 rounded-lg bg-card/50 p-4">
      <h4 className="font-medium text-sm flex items-center gap-2">
        <Activity className="w-4 h-4" />
        Provider Health
      </h4>
      <div className="flex flex-wrap gap-2">
        {states.map((s) => {
          const colors = getAccentColors(stateColor(s.state));
          const label = PROVIDER_LABELS[s.provider_key] ?? s.provider_key;
          return (
            <div
              key={s.provider_key}
              className={`flex items-center gap-2 rounded-md px-2.5 py-1 text-xs ${colors.bg} ${colors.text}`}
              title={`${label}: ${s.state}${s.available ? "" : " (unavailable)"}`}
            >
              <StateIcon state={s.state} />
              <span className="font-medium">{label}</span>
              <span className="opacity-75">{s.state}</span>
              {s.state === "Open" && (
                <button
                  type="button"
                  onClick={() => resetProvider(s.provider_key)}
                  className="ml-1 p-0.5 hover:opacity-100 opacity-75"
                  title="Reset circuit breaker"
                >
                  <RotateCcw className="w-3 h-3" />
                </button>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
