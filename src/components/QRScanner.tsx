import { useEffect, useRef, useState } from "react";
import { Html5Qrcode } from "html5-qrcode";
import { Camera, X, CheckCircle, AlertCircle } from "lucide-react";

export interface QRScannerProps {
  onScan: (data: string) => void;
  onError?: (error: string) => void;
  onClose: () => void;
}

export function QRScanner({ onScan, onError, onClose }: QRScannerProps) {
  const [scanning, setScanning] = useState(false);
  const [cameras, setCameras] = useState<Array<{ id: string; label: string }>>([]);
  const [selectedCamera, setSelectedCamera] = useState<string>("");
  const [error, setError] = useState<string | null>(null);
  const [scanSuccess, setScanSuccess] = useState(false);
  const [statusMessage, setStatusMessage] = useState("Initializing camera...");

  const html5QrCodeRef = useRef<Html5Qrcode | null>(null);
  const scannerIdRef = useRef("qr-reader");

  // Initialize camera list
  useEffect(() => {
    Html5Qrcode.getCameras()
      .then((devices) => {
        if (devices && devices.length > 0) {
          const cameraList = devices.map((device) => ({
            id: device.id,
            label: device.label || `Camera ${device.id}`,
          }));
          setCameras(cameraList);
          // Auto-select first camera
          setSelectedCamera(cameraList[0].id);
          setError(null);
        } else {
          const errorMsg = "No cameras found. Please connect a camera and try again.";
          setError(errorMsg);
          setStatusMessage(errorMsg);
          if (onError) onError(errorMsg);
        }
      })
      .catch((err) => {
        const errorMsg = "Failed to access cameras. Please grant camera permissions.";
        console.error("Camera enumeration error:", err);
        setError(errorMsg);
        setStatusMessage(errorMsg);
        if (onError) onError(errorMsg);
      });
  }, [onError]);

  // Start scanning when camera is selected
  useEffect(() => {
    if (!selectedCamera || scanSuccess) return;

    const startScanning = async () => {
      try {
        // Clean up existing instance
        if (html5QrCodeRef.current) {
          try {
            await html5QrCodeRef.current.stop();
            html5QrCodeRef.current.clear();
          } catch (e) {
            console.warn("Error stopping previous scanner:", e);
          }
        }

        // Create new scanner instance
        html5QrCodeRef.current = new Html5Qrcode(scannerIdRef.current);

        // Configure scanner
        const config = {
          fps: 10, // Scans per second
          qrbox: { width: 250, height: 250 }, // Scanning box size
          aspectRatio: 1.0,
        };

        // Success callback
        const onScanSuccess = (decodedText: string) => {
          console.log("QR Code detected:", decodedText);

          // Validate that it's valid JSON
          try {
            JSON.parse(decodedText);
            setScanSuccess(true);
            setStatusMessage("QR code detected!");
            setScanning(false);

            // Stop scanner
            if (html5QrCodeRef.current) {
              html5QrCodeRef.current.stop().then(() => {
                // Call onScan callback after a brief delay to show success message
                setTimeout(() => {
                  onScan(decodedText);
                }, 800);
              });
            }
          } catch (e) {
            const errorMsg = "Invalid QR code. Please scan a valid connection QR code.";
            setError(errorMsg);
            setStatusMessage(errorMsg);
            if (onError) onError(errorMsg);
            // Continue scanning
          }
        };

        // Error callback (fires on every frame without QR code - we can ignore these)
        const onScanFailure = (error: string) => {
          // Only log actual errors, not "No QR code found" messages
          if (!error.includes("NotFoundException")) {
            console.debug("Scan error:", error);
          }
        };

        // Start scanning
        await html5QrCodeRef.current.start(selectedCamera, config, onScanSuccess, onScanFailure);

        setScanning(true);
        setStatusMessage("Position QR code in front of camera");
        setError(null);
      } catch (err: any) {
        console.error("Failed to start scanner:", err);

        let errorMsg = "Failed to start camera.";
        if (err?.name === "NotAllowedError") {
          errorMsg = "Camera access denied. Please grant camera permissions.";
        } else if (err?.message) {
          errorMsg = `Camera error: ${err.message}`;
        }

        setError(errorMsg);
        setStatusMessage(errorMsg);
        setScanning(false);
        if (onError) onError(errorMsg);
      }
    };

    startScanning();

    // Cleanup on unmount or camera change
    return () => {
      if (html5QrCodeRef.current && scanning) {
        html5QrCodeRef.current
          .stop()
          .then(() => {
            html5QrCodeRef.current?.clear();
          })
          .catch((err) => {
            console.error("Error stopping scanner on cleanup:", err);
          });
      }
    };
  }, [selectedCamera, scanSuccess, onScan, onError]);

  const handleCameraChange = (cameraId: string) => {
    setSelectedCamera(cameraId);
    setScanSuccess(false);
    setError(null);
  };

  return (
    <div className="space-y-4">
      {/* Camera Selection */}
      {cameras.length > 1 && !scanSuccess && (
        <div className="space-y-2">
          <label className="block text-sm font-medium text-foreground">Select Camera</label>
          <select
            value={selectedCamera}
            onChange={(e) => handleCameraChange(e.target.value)}
            className="w-full px-3 py-2 bg-input border border-border/50 rounded-md text-foreground"
          >
            {cameras.map((camera) => (
              <option key={camera.id} value={camera.id}>
                {camera.label}
              </option>
            ))}
          </select>
        </div>
      )}

      {/* Video Preview */}
      <div className="relative bg-black rounded-lg overflow-hidden" style={{ minHeight: "400px" }}>
        <div id={scannerIdRef.current} className="w-full"></div>

        {/* Scanning Overlay */}
        {scanning && !scanSuccess && !error && (
          <div className="absolute inset-0 pointer-events-none">
            <div className="absolute inset-0 flex items-center justify-center">
              <div className="w-64 h-64 border-4 border-primary rounded-lg animate-pulse"></div>
            </div>
          </div>
        )}
      </div>

      {/* Status Message */}
      <div className="flex items-center justify-center gap-2 p-3 rounded-lg bg-card border border-border/50">
        {error ? (
          <>
            <AlertCircle className="w-5 h-5 text-red-400 shrink-0" />
            <span className="text-sm text-red-400">{statusMessage}</span>
          </>
        ) : scanSuccess ? (
          <>
            <CheckCircle className="w-5 h-5 text-green-400 shrink-0" />
            <span className="text-sm text-green-400">{statusMessage}</span>
          </>
        ) : scanning ? (
          <>
            <Camera className="w-5 h-5 text-primary shrink-0 animate-pulse" />
            <span className="text-sm text-muted-foreground">{statusMessage}</span>
          </>
        ) : (
          <>
            <Camera className="w-5 h-5 text-muted-foreground shrink-0" />
            <span className="text-sm text-muted-foreground">{statusMessage}</span>
          </>
        )}
      </div>

      {/* Action Buttons */}
      <div className="flex justify-end gap-3">
        <button
          onClick={onClose}
          className="px-4 py-2 bg-muted hover:bg-muted/80 text-foreground rounded-md font-medium transition-colors flex items-center gap-2"
        >
          <X className="w-4 h-4" />
          {scanSuccess ? "Close" : "Cancel"}
        </button>
      </div>
    </div>
  );
}
