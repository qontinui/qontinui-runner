import * as Tabs from "@radix-ui/react-tabs";
import { Plug, Radar, Fingerprint } from "lucide-react";
import { useState, useCallback } from "react";
import { DiscoveryPanel } from "./DiscoveryPanel";
import { SourceIntegrationPanel } from "./SourceIntegrationPanel";
import { StateExplorerPanel } from "./StateExplorerPanel";

const TABS = [
  { id: "applications", label: "Applications", icon: Radar },
  { id: "state-explorer", label: "State Explorer", icon: Fingerprint },
] as const;

export function UIBridgeIntegrationPage() {
  const [activeTab, setActiveTab] = useState<string>("applications");
  const [integratePath, setIntegratePath] = useState<string | null>(null);

  const handleIntegrateFromDiscovery = useCallback((projectPath: string) => {
    setIntegratePath(projectPath);
    setActiveTab("applications");
  }, []);

  return (
    <div className="h-full flex flex-col p-4 gap-4">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Plug className="w-5 h-5 text-cyan-400" />
        <h1 className="text-lg font-semibold">UI Bridge</h1>
        <span className="text-xs px-1.5 py-0.5 rounded bg-cyan-500/20 text-cyan-400 border border-cyan-500/30 font-medium">
          integration
        </span>
      </div>

      {/* Tabs */}
      <Tabs.Root
        value={activeTab}
        onValueChange={setActiveTab}
        className="flex-1 flex flex-col min-h-0"
      >
        <Tabs.List className="flex gap-1 border-b border-border pb-px shrink-0">
          {TABS.map((tab) => {
            const Icon = tab.icon;
            return (
              <Tabs.Trigger
                key={tab.id}
                value={tab.id}
                className="flex items-center gap-1.5 px-3 py-1.5 text-sm text-muted-foreground
                           hover:text-foreground border-b-2 border-transparent
                           data-[state=active]:text-foreground data-[state=active]:border-cyan-500
                           transition-colors"
              >
                <Icon className="w-3.5 h-3.5" />
                {tab.label}
              </Tabs.Trigger>
            );
          })}
        </Tabs.List>

        <Tabs.Content value="applications" className="flex-1 min-h-0 pt-4 overflow-auto">
          <div className="flex flex-col gap-6">
            <DiscoveryPanel onIntegrate={handleIntegrateFromDiscovery} />
            {integratePath && (
              <div className="border-t border-border pt-4">
                <SourceIntegrationPanel
                  prefillPath={integratePath}
                  onPrefillConsumed={() => setIntegratePath(null)}
                />
              </div>
            )}
          </div>
        </Tabs.Content>

        <Tabs.Content value="state-explorer" className="flex-1 min-h-0 pt-4 overflow-auto">
          <StateExplorerPanel />
        </Tabs.Content>
      </Tabs.Root>
    </div>
  );
}
