/**
 * Step 3: show the user the code they need in their React Native app so
 * UI Bridge can reach the runner.
 *
 * - USB: skipped (ADB forwarding needs no SDK changes).
 * - LAN: needs enableMdnsAnnounce + serverPort, plus the react-native-zeroconf dep.
 * - Remote: needs cloudRelayConfig with the tunnel's exposed URL.
 */

import { useState } from "react";
import { Copy, Check, Terminal } from "lucide-react";

import { Button } from "../ui/Button";
import type { WizardApi } from "./ConnectionWizard";

// ---------------------------------------------------------------------------
// Snippet generators
// ---------------------------------------------------------------------------

function lanSnippet(): string {
  return `import { UIBridgeNativeProvider } from "@qontinui/ui-bridge-native";

export default function App() {
  return (
    <UIBridgeNativeProvider
      appId="my-app"
      appType="mobile"
      enableMdnsAnnounce={true}
      serverPort={8087}
    >
      {/* your app */}
    </UIBridgeNativeProvider>
  );
}`;
}

function remoteSnippet(serverAddr: string): string {
  return `import { UIBridgeNativeProvider } from "@qontinui/ui-bridge-native";

export default function App() {
  return (
    <UIBridgeNativeProvider
      appId="my-app"
      appType="mobile"
      cloudRelayConfig={{
        // Points at your rathole server's exposed port.
        relayUrl: "ws://${serverAddr}/ui-bridge",
        // Optional per-device pairing token.
        authToken: "<paste from runner>",
      }}
    >
      {/* your app */}
    </UIBridgeNativeProvider>
  );
}`;
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export function MobileAppGuideStep({ api }: { api: WizardApi }) {
  const { connectionType, tunnelConfig } = api.state;

  // USB path is short-circuited by ConnectionWizard; this branch is defensive.
  if (connectionType === "usb") {
    return (
      <div className="space-y-3 text-sm text-muted-foreground">
        USB does not need SDK changes — ADB forwards the UI Bridge port transparently. Skipping
        ahead.
      </div>
    );
  }

  const snippet =
    connectionType === "lan"
      ? lanSnippet()
      : remoteSnippet(tunnelConfig?.serverAddr ?? "relay.example.com:2333");

  const npmDep = connectionType === "lan" ? "npm install react-native-zeroconf" : null;

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">
        Add this to your React Native / Expo app so it can reach the runner.
      </p>

      {npmDep && (
        <div className="flex items-center gap-2 p-3 rounded-md bg-zinc-800/50 border border-zinc-700">
          <Terminal className="w-4 h-4 text-primary" />
          <code className="flex-1 text-xs text-foreground">{npmDep}</code>
          <CopyButton text={npmDep} />
        </div>
      )}

      <div className="relative rounded-md bg-zinc-900 border border-zinc-700">
        <div className="absolute top-2 right-2">
          <CopyButton text={snippet} />
        </div>
        <pre className="p-4 pr-16 text-xs text-zinc-300 whitespace-pre overflow-x-auto max-h-72">
          {snippet}
        </pre>
      </div>

      <div className="flex justify-end">
        <Button variant="primary" onClick={api.next}>
          I added it — continue
        </Button>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// CopyButton
// ---------------------------------------------------------------------------

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard may be unavailable (iframe, insecure context) — degrade silently.
    }
  };

  return (
    <Button variant="ghost" size="sm" onClick={copy}>
      {copied ? (
        <Check className="w-3.5 h-3.5 text-emerald-400" />
      ) : (
        <Copy className="w-3.5 h-3.5" />
      )}
      {copied ? "Copied" : "Copy"}
    </Button>
  );
}
