import { useState, useCallback } from "react";
import { Plug } from "lucide-react";
import { SourceIntegrationPanel } from "./SourceIntegrationPanel";
import { DiscoveryPanel } from "./DiscoveryPanel";

export function UIBridgeIntegrationPage() {
  const [selectedProjectPath, setSelectedProjectPath] = useState<string | undefined>();

  const handleSelectApp = useCallback((basePath: string) => {
    setSelectedProjectPath(basePath);
    // Scroll to the integration panel at the top
    window.scrollTo({ top: 0, behavior: "smooth" });
  }, []);

  return (
    <div className="h-full flex flex-col p-4 gap-6 overflow-auto">
      {/* Header */}
      <div className="flex items-center gap-3">
        <Plug className="w-5 h-5 text-cyan-400" />
        <h1 className="text-lg font-semibold">UI Bridge</h1>
        <span className="text-xs px-1.5 py-0.5 rounded bg-cyan-500/20 text-cyan-400 border border-cyan-500/30 font-medium">
          integration
        </span>
      </div>

      {/* Source Integration — primary action */}
      <SourceIntegrationPanel initialProjectPath={selectedProjectPath} />

      {/* Discovery — scan for running apps */}
      <DiscoveryPanel onSelectApp={handleSelectApp} selectedProjectPath={selectedProjectPath} />
    </div>
  );
}
