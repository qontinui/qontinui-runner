import { useState } from "react";
import { QRCodeSVG } from "qrcode.react";

/**
 * QR Test Generator - Development Tool
 *
 * This component is for development and testing purposes only.
 * It generates test QR codes that can be scanned by the QR scanner.
 *
 * Usage:
 * 1. Import this component in your development environment
 * 2. Render it somewhere visible (e.g., in Settings during dev)
 * 3. Use your phone or a QR scanner app to scan the generated code
 * 4. Test the QR scanning functionality
 *
 * To use in production, remove this component or gate it behind a dev flag.
 */
export function QRTestGenerator() {
  const [customUrl, setCustomUrl] = useState("ws://localhost:8001");
  const [customToken, setCustomToken] = useState("test_token_123456789");
  const [customUserId, setCustomUserId] = useState("test-user-id");
  const [customProjectId, setCustomProjectId] = useState<number | null>(1);

  const testConnectionString = {
    version: "1.0",
    url: customUrl,
    token: customToken,
    userId: customUserId,
    projectId: customProjectId,
    createdAt: new Date().toISOString(),
  };

  const jsonString = JSON.stringify(testConnectionString);

  return (
    <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
      <div>
        <h4 className="font-semibold text-lg mb-2">QR Code Test Generator</h4>
        <p className="text-sm text-muted-foreground">
          Development tool for testing QR scanner functionality. Remove in production.
        </p>
      </div>

      {/* Custom Connection Settings */}
      <div className="space-y-4">
        <div>
          <label className="block text-sm font-medium mb-1">WebSocket URL</label>
          <input
            type="text"
            value={customUrl}
            onChange={(e) => setCustomUrl(e.target.value)}
            className="w-full px-3 py-2 bg-input border border-border/50 rounded-md text-sm"
            placeholder="ws://localhost:8001"
          />
        </div>

        <div>
          <label className="block text-sm font-medium mb-1">Token</label>
          <input
            type="text"
            value={customToken}
            onChange={(e) => setCustomToken(e.target.value)}
            className="w-full px-3 py-2 bg-input border border-border/50 rounded-md text-sm font-mono"
            placeholder="test_token_123456789"
          />
        </div>

        <div>
          <label className="block text-sm font-medium mb-1">User ID</label>
          <input
            type="text"
            value={customUserId}
            onChange={(e) => setCustomUserId(e.target.value)}
            className="w-full px-3 py-2 bg-input border border-border/50 rounded-md text-sm"
            placeholder="test-user-id"
          />
        </div>

        <div>
          <label className="block text-sm font-medium mb-1">Project ID (optional)</label>
          <input
            type="number"
            value={customProjectId ?? ""}
            onChange={(e) => setCustomProjectId(e.target.value ? parseInt(e.target.value) : null)}
            className="w-full px-3 py-2 bg-input border border-border/50 rounded-md text-sm"
            placeholder="1"
          />
        </div>
      </div>

      {/* Generated QR Code */}
      <div className="flex flex-col items-center gap-4 p-6 bg-white rounded-lg">
        <div className="text-sm font-medium text-gray-800">Scan this QR Code</div>
        <QRCodeSVG value={jsonString} size={256} level="H" />
        <div className="text-xs text-gray-500 text-center max-w-md break-all">{jsonString}</div>
      </div>

      {/* Instructions */}
      <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
        <div className="text-sm text-muted-foreground">
          <strong className="text-foreground">Testing Instructions:</strong>
          <ol className="list-decimal list-inside mt-2 space-y-1">
            <li>Display this page on one screen</li>
            <li>Click "Scan QR Code" in Quick Connect</li>
            <li>Point your webcam at the QR code above</li>
            <li>The connection string should auto-populate and connect</li>
          </ol>
        </div>
      </div>
    </div>
  );
}
