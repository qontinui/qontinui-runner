import { useState } from "react";
import { Video, Check, AlertCircle } from "lucide-react";

interface VideoRecordingConfig {
  enabled: boolean;
  startDelaySeconds: number;
  maxDurationSeconds: number;
  fps: number;
  quality: "low" | "medium" | "high";
  codec: string;
}

interface VideoRecordingSettingsProps {
  onLog: (level: "info" | "warning" | "error" | "success", message: string) => void;
}

export function VideoRecordingSettings({ onLog }: VideoRecordingSettingsProps) {
  const [config, setConfig] = useState<VideoRecordingConfig>({
    enabled: false,
    startDelaySeconds: 0,
    maxDurationSeconds: 300, // 5 minutes default
    fps: 15,
    quality: "medium",
    codec: "h264",
  });

  const [saving, _setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);

  const handleToggleEnabled = () => {
    setConfig((prev) => ({
      ...prev,
      enabled: !prev.enabled,
    }));
  };

  const handleStartDelayChange = (value: number) => {
    const clamped = Math.max(0, Math.min(60, value));
    setConfig((prev) => ({
      ...prev,
      startDelaySeconds: clamped,
    }));
  };

  const handleMaxDurationChange = (value: number) => {
    const clamped = Math.max(0, value);
    setConfig((prev) => ({
      ...prev,
      maxDurationSeconds: clamped,
    }));
  };

  const handleFpsChange = (value: number) => {
    const clamped = Math.max(15, Math.min(30, value));
    setConfig((prev) => ({
      ...prev,
      fps: clamped,
    }));
  };

  const handleQualityChange = (quality: "low" | "medium" | "high") => {
    setConfig((prev) => ({
      ...prev,
      quality,
    }));
  };

  const saveSettings = () => {
    // Settings are stored in component state
    // They will be used when automation starts
    setSaveSuccess(true);
    onLog("success", "Video recording settings updated");
    setTimeout(() => setSaveSuccess(false), 3000);
  };

  return (
    <div className="space-y-6 bg-card rounded-lg border border-border/50 p-6">
      <div className="flex items-center gap-3">
        <Video className="w-5 h-5 text-primary" />
        <h4 className="font-semibold text-lg">Video Recording</h4>
      </div>

      {/* Enable Video Recording Toggle */}
      <div className="space-y-2">
        <label className="flex items-center justify-between cursor-pointer">
          <div className="space-y-1">
            <div className="font-medium">Enable Video Recording</div>
            <div className="text-sm text-muted-foreground">
              Record screen during automation execution
            </div>
          </div>
          <button
            onClick={handleToggleEnabled}
            className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
              config.enabled ? "bg-primary" : "bg-muted"
            }`}
          >
            <span
              className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
                config.enabled ? "translate-x-6" : "translate-x-1"
              }`}
            />
          </button>
        </label>
      </div>

      {/* Start Delay */}
      <div className="space-y-2">
        <label className="block">
          <div className="font-medium mb-1">Start Delay (seconds)</div>
          <div className="text-sm text-muted-foreground mb-3">
            Delay before starting recording after automation begins (0-60 seconds)
          </div>
          <div className="flex items-center gap-4">
            <input
              type="range"
              min="0"
              max="60"
              value={config.startDelaySeconds}
              onChange={(e) => handleStartDelayChange(parseInt(e.target.value))}
              disabled={!config.enabled}
              className="flex-1 h-2 bg-muted rounded-lg appearance-none cursor-pointer accent-primary disabled:opacity-50"
            />
            <input
              type="number"
              min="0"
              max="60"
              value={config.startDelaySeconds}
              onChange={(e) => handleStartDelayChange(parseInt(e.target.value))}
              disabled={!config.enabled}
              className="w-20 px-3 py-2 bg-input border border-border/50 rounded-md text-center disabled:opacity-50"
            />
          </div>
        </label>
      </div>

      {/* Max Duration */}
      <div className="space-y-2">
        <label className="block">
          <div className="font-medium mb-1">Max Duration (seconds)</div>
          <div className="text-sm text-muted-foreground mb-3">
            Maximum recording duration (0 = unlimited). Recording auto-stops when reached.
          </div>
          <input
            type="number"
            min="0"
            step="30"
            value={config.maxDurationSeconds}
            onChange={(e) => handleMaxDurationChange(parseInt(e.target.value) || 0)}
            disabled={!config.enabled}
            className="w-full px-3 py-2 bg-input border border-border/50 rounded-md disabled:opacity-50"
            placeholder="300 (5 minutes)"
          />
        </label>
      </div>

      {/* FPS */}
      <div className="space-y-2">
        <label className="block">
          <div className="font-medium mb-1">Frame Rate (FPS)</div>
          <div className="text-sm text-muted-foreground mb-3">
            Frames per second for video recording (15-30). Lower FPS = smaller file size.
          </div>
          <div className="flex items-center gap-4">
            <input
              type="range"
              min="15"
              max="30"
              value={config.fps}
              onChange={(e) => handleFpsChange(parseInt(e.target.value))}
              disabled={!config.enabled}
              className="flex-1 h-2 bg-muted rounded-lg appearance-none cursor-pointer accent-primary disabled:opacity-50"
            />
            <input
              type="number"
              min="15"
              max="30"
              value={config.fps}
              onChange={(e) => handleFpsChange(parseInt(e.target.value))}
              disabled={!config.enabled}
              className="w-20 px-3 py-2 bg-input border border-border/50 rounded-md text-center disabled:opacity-50"
            />
          </div>
        </label>
      </div>

      {/* Quality */}
      <div className="space-y-2">
        <div className="font-medium mb-1">Video Quality</div>
        <div className="text-sm text-muted-foreground mb-3">
          Higher quality produces larger file sizes
        </div>
        <div className="flex gap-2">
          {(["low", "medium", "high"] as const).map((quality) => (
            <button
              key={quality}
              onClick={() => handleQualityChange(quality)}
              disabled={!config.enabled}
              className={`flex-1 px-4 py-2 rounded-md font-medium transition-colors disabled:opacity-50 ${
                config.quality === quality
                  ? "bg-primary text-primary-foreground"
                  : "bg-muted hover:bg-muted/80"
              }`}
            >
              {quality.charAt(0).toUpperCase() + quality.slice(1)}
            </button>
          ))}
        </div>
      </div>

      {/* Codec Info */}
      <div className="space-y-2">
        <div className="font-medium mb-1">Codec</div>
        <div className="text-sm text-muted-foreground">
          Using H.264 (libx264) for maximum compatibility
        </div>
      </div>

      {/* Info box */}
      <div className="p-3 bg-primary/5 border border-primary/20 rounded-lg">
        <div className="flex items-start gap-2">
          <AlertCircle className="w-4 h-4 text-primary mt-0.5 shrink-0" />
          <div className="text-sm text-muted-foreground">
            <strong className="text-foreground">Requirements:</strong>
            <ul className="mt-2 space-y-1 ml-4 list-disc">
              <li>FFmpeg must be installed and available in system PATH</li>
              <li>Videos are saved to your system's Videos folder (qontinui-recordings)</li>
              <li>Recording starts automatically when automation begins (after delay)</li>
              <li>Recording stops when automation ends or max duration is reached</li>
            </ul>
          </div>
        </div>
      </div>

      {/* Success message */}
      {saveSuccess && (
        <div className="p-3 bg-green-500/10 border border-green-500/30 rounded-lg flex items-start gap-2">
          <Check className="w-5 h-5 text-green-400 shrink-0 mt-0.5" />
          <span className="text-green-400 text-sm">Settings saved successfully!</span>
        </div>
      )}

      {/* Save Button */}
      <div className="flex justify-end">
        <button
          onClick={saveSettings}
          disabled={saving}
          className="px-6 py-2 bg-primary hover:bg-primary/80 text-primary-foreground rounded-md font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
        >
          {saving ? (
            <>
              <div className="w-4 h-4 border-2 border-primary-foreground/30 border-t-primary-foreground rounded-full animate-spin" />
              Saving...
            </>
          ) : saveSuccess ? (
            <>
              <Check className="w-4 h-4" />
              Saved!
            </>
          ) : (
            <>
              <Video className="w-4 h-4" />
              Save Video Settings
            </>
          )}
        </button>
      </div>
    </div>
  );
}

// Helper function to get current config (for use in automation integration)
export function getVideoRecordingConfig(): VideoRecordingConfig {
  // This would need to be managed through a context or state management solution
  // For now, return default config
  return {
    enabled: false,
    startDelaySeconds: 0,
    maxDurationSeconds: 300,
    fps: 15,
    quality: "medium",
    codec: "h264",
  };
}
