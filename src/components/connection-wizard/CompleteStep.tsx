/**
 * Step 5: success summary.
 */

import { CheckCircle2, Smartphone } from "lucide-react";

import { Button } from "../ui/Button";
import type { WizardApi } from "./ConnectionWizard";

export function CompleteStep({ api, onClose }: { api: WizardApi; onClose: () => void }) {
  const { deviceId, connectionType, platform, elements, proxyUrl } = api.state;
  const count = elements?.length ?? 0;

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-3 p-4 rounded-lg bg-emerald-500/10 border border-emerald-500/30">
        <CheckCircle2 className="w-6 h-6 text-emerald-400 shrink-0" />
        <div className="min-w-0">
          <div className="text-sm font-medium text-foreground">Device connected</div>
          <div className="text-xs text-muted-foreground">
            {count} UI elements are readable from {connectionType?.toUpperCase() ?? "unknown"}{" "}
            transport
          </div>
        </div>
      </div>

      <div className="p-4 rounded-md bg-zinc-800/50 border border-zinc-700 space-y-2">
        <div className="flex items-center gap-2">
          <Smartphone className="w-4 h-4 text-primary" />
          <span className="text-sm text-foreground">{deviceId}</span>
        </div>
        <dl className="grid grid-cols-2 gap-2 text-xs">
          <Row label="Platform" value={platform ?? "—"} />
          <Row label="Transport" value={connectionType ?? "—"} />
          <Row label="Proxy URL" value={proxyUrl ?? "—"} span />
        </dl>
      </div>

      {elements && elements.length > 0 && (
        <div className="p-3 rounded-md bg-zinc-800/30 border border-zinc-800">
          <div className="text-xs text-muted-foreground mb-2">First few elements:</div>
          <ul className="space-y-1">
            {elements.slice(0, 4).map((e) => (
              <li key={e.id} className="text-xs text-foreground truncate">
                <span className="text-muted-foreground">{e.type}</span> · {e.label}
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="flex justify-end">
        <Button variant="primary" onClick={onClose}>
          Done
        </Button>
      </div>
    </div>
  );
}

function Row({ label, value, span }: { label: string; value: string; span?: boolean }) {
  return (
    <div className={span ? "col-span-2" : ""}>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="text-foreground truncate">{value}</dd>
    </div>
  );
}
