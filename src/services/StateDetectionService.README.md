# StateDetectionService

TypeScript integration for state detection in qontinui-runner. This service spawns a Python bridge process to analyze screenshots and detect UI elements and application states.

## File Location

`/Users/jspinak/Documents/qontinui/qontinui-runner/src/services/StateDetectionService.ts`

## Overview

The `StateDetectionService` provides a TypeScript interface to the qontinui Python state detection library. It:

- Spawns and manages Python bridge processes
- Handles stdout/stderr parsing
- Provides typed interfaces for all detection results
- Includes comprehensive error handling
- Supports configurable timeouts and analysis options

## Installation

The service is part of the qontinui-runner package. No additional installation is required.

## Basic Usage

```typescript
import { StateDetectionService } from "./services/StateDetectionService";

// Create service instance
const service = new StateDetectionService({
  pythonPath: "python3",
  bridgeScriptPath: "/path/to/state_detection_bridge.py",
  defaultTimeout: 300000, // 5 minutes
});

// Analyze screenshots from a session
try {
  const result = await service.processSession("/path/to/screenshots/session_123", {
    minRegionSize: [20, 20],
    maxRegionSize: [500, 500],
    stabilityThreshold: 0.98,
    processingMode: "full",
  });

  console.log(`Analysis complete in ${result.processingTime}ms`);
  console.log(`Found ${result.stateImages.length} UI elements`);
  console.log(`Detected ${result.states.length} application states`);

  // Process state images (UI elements)
  result.stateImages.forEach((element) => {
    console.log(`Element: ${element.name} at (${element.x}, ${element.y})`);
    console.log(`  Type: ${element.tags.join(", ")}`);
    console.log(`  Frequency: ${(element.frequency * 100).toFixed(1)}%`);
  });

  // Process detected states
  result.states.forEach((state) => {
    console.log(`State: ${state.name} (confidence: ${state.confidence})`);
    console.log(`  Elements: ${state.stateImageIds.length}`);
    console.log(`  Screenshots: ${state.screenshots.length}`);
  });
} catch (error) {
  if (error instanceof StateDetectionError) {
    console.error(`Detection failed: ${error.type}`);
    console.error(`Message: ${error.message}`);
    console.error(`Details:`, error.details);
  }
}
```

## Using the Singleton Instance

For convenience, a singleton instance is exported:

```typescript
import { stateDetectionService } from "./services/StateDetectionService";

// Use the singleton
const result = await stateDetectionService.processSession("/path/to/screenshots", {
  processingMode: "fast",
});
```

## Configuration Options

### StateDetectionConfig

```typescript
interface StateDetectionConfig {
  // Region size constraints
  minRegionSize?: [number, number]; // Default: [20, 20]
  maxRegionSize?: [number, number]; // Default: [500, 500]

  // Detection thresholds
  colorTolerance?: number; // Default: 5 (0-255)
  stabilityThreshold?: number; // Default: 0.98 (0.0-1.0)
  varianceThreshold?: number; // Default: 10.0
  similarityThreshold?: number; // Default: 0.95 (0.0-1.0)

  // Filtering options
  minScreenshotsPresent?: number; // Default: 2

  // Processing options
  processingMode?: "full" | "fast" | "accurate"; // Default: "full"
  enableRectangleDecomposition?: boolean; // Default: true
  enableCooccurrenceAnalysis?: boolean; // Default: true

  // Optional region of interest
  region?: {
    x: number;
    y: number;
    width: number;
    height: number;
  };

  // Timeout
  timeout?: number; // Default: 300000 (5 minutes)
}
```

## Advanced Usage

### Custom Configuration

```typescript
const service = new StateDetectionService({
  pythonPath: "/usr/local/bin/python3.11",
  bridgeScriptPath: "/custom/path/bridge.py",
  defaultTimeout: 600000, // 10 minutes
});

const result = await service.processSession("/screenshots", {
  // Focus on small UI elements
  minRegionSize: [10, 10],
  maxRegionSize: [200, 200],

  // High precision settings
  stabilityThreshold: 0.99,
  similarityThreshold: 0.98,

  // Analyze only a specific region
  region: {
    x: 100,
    y: 100,
    width: 800,
    height: 600,
  },

  // Fast processing
  processingMode: "fast",
  enableRectangleDecomposition: false,

  // Custom timeout for large datasets
  timeout: 600000,
});
```

### Error Handling

```typescript
import { StateDetectionError, StateDetectionErrorType } from "./services/StateDetectionService";

try {
  const result = await service.processSession("/screenshots");
} catch (error) {
  if (error instanceof StateDetectionError) {
    switch (error.type) {
      case StateDetectionErrorType.PYTHON_NOT_FOUND:
        console.error("Python is not installed or not in PATH");
        break;

      case StateDetectionErrorType.BRIDGE_SCRIPT_NOT_FOUND:
        console.error("Bridge script missing:", error.details);
        break;

      case StateDetectionErrorType.TIMEOUT:
        console.error("Analysis timed out");
        // Retry with longer timeout
        break;

      case StateDetectionErrorType.INVALID_JSON:
        console.error("Failed to parse Python output:", error.details);
        break;

      case StateDetectionErrorType.ANALYSIS_FAILED:
        console.error("Python analysis failed:", error.message);
        break;

      default:
        console.error("Unknown error:", error.message);
    }
  }
}
```

### Process Management

```typescript
// Check active processes
console.log(`Active processes: ${service.getActiveProcessCount()}`);

// Kill all active processes (cleanup)
service.killAllProcesses();

// Update configuration
service.setPythonPath("/usr/local/bin/python3");
service.setBridgeScriptPath("/new/path/bridge.py");
service.setDefaultTimeout(600000);
```

### Working with Results

```typescript
const result = await service.processSession("/screenshots");

// Find all button elements
const buttons = result.stateImages.filter((img) => img.tags.includes("button"));

// Find high-frequency elements (appear in >80% of screenshots)
const persistentElements = result.stateImages.filter((img) => img.frequency > 0.8);

// Find states with high confidence
const reliableStates = result.states.filter((state) => state.confidence > 0.9);

// Get elements by position
const topLeftElements = result.stateImages.filter((img) => img.x < 200 && img.y < 200);

// Group elements by state
const stateElementMap = new Map<string, StateImageInfo[]>();
result.states.forEach((state) => {
  const elements = result.stateImages.filter((img) => state.stateImageIds.includes(img.id));
  stateElementMap.set(state.id, elements);
});
```

## TypeScript Interfaces

### StateImageInfo

Represents a detected UI element:

```typescript
interface StateImageInfo {
  id: string; // Unique identifier
  name: string; // Human-readable name
  x: number; // Top-left X
  y: number; // Top-left Y
  x2: number; // Bottom-right X
  y2: number; // Bottom-right Y
  width: number; // Element width
  height: number; // Element height
  pixelHash: string; // Pixel data hash
  frequency: number; // Appearance frequency (0.0-1.0)
  screenshots: string[]; // Screenshot IDs
  tags: string[]; // Element type tags
  darkPixelPercentage?: number; // Dark pixel %
  lightPixelPercentage?: number; // Light pixel %
  maskDensity: number; // Mask density (0.0-1.0)
  hasMask: boolean; // Has mask?
}
```

### DetectedState

Represents an application state:

```typescript
interface DetectedState {
  id: string; // Unique identifier
  name: string; // Human-readable name
  stateImageIds: string[]; // Constituent element IDs
  screenshots: string[]; // Screenshot IDs
  confidence: number; // Detection confidence (0.0-1.0)
  metadata: Record<string, unknown>; // Additional metadata
}
```

### StateDetectionResult

Complete analysis result:

```typescript
interface StateDetectionResult {
  success: boolean; // Success flag
  stateImages: StateImageInfo[]; // Detected elements
  states: DetectedState[]; // Detected states
  totalScreenshots: number; // Total screenshots analyzed
  processingTime: number; // Processing time (ms)
  error?: string; // Error message (if failed)
  metadata?: Record<string, unknown>; // Additional metadata
}
```

## Integration with qontinui-runner

### In App Component

```typescript
import { stateDetectionService } from './services/StateDetectionService';

function MyComponent() {
  const [isAnalyzing, setIsAnalyzing] = useState(false);
  const [results, setResults] = useState<StateDetectionResult | null>(null);

  const handleAnalyze = async (sessionPath: string) => {
    setIsAnalyzing(true);
    try {
      const result = await stateDetectionService.processSession(
        sessionPath,
        {
          processingMode: 'full',
          minScreenshotsPresent: 3
        }
      );
      setResults(result);
    } catch (error) {
      console.error('Analysis failed:', error);
    } finally {
      setIsAnalyzing(false);
    }
  };

  return (
    <div>
      {isAnalyzing && <LoadingSpinner />}
      {results && <ResultsView results={results} />}
    </div>
  );
}
```

### With Tauri Commands

```typescript
import { invoke } from "@tauri-apps/api/core";
import { stateDetectionService } from "./services/StateDetectionService";

// In your Tauri frontend
async function analyzeSessionWithTauri(sessionId: string) {
  // Get session path from Tauri backend
  const sessionPath = await invoke<string>("get_session_path", { sessionId });

  // Run state detection
  const result = await stateDetectionService.processSession(sessionPath);

  // Save results via Tauri
  await invoke("save_state_detection_results", {
    sessionId,
    results: result,
  });

  return result;
}
```

## Python Bridge Script

The service expects a Python bridge script at the configured path. The script should:

1. Accept command-line arguments: `python bridge.py <session_path> <config_json>`
2. Load screenshots from the session path
3. Run state detection analysis
4. Output JSON to stdout with the format:

```json
{
  "success": true,
  "state_images": [...],
  "states": [...],
  "total_screenshots": 42,
  "metadata": {}
}
```

Or on error:

```json
{
  "success": false,
  "error": "Error message"
}
```

## Performance Considerations

- **Timeout**: Default timeout is 5 minutes. Increase for large datasets.
- **Processing Mode**:
  - `"fast"`: Quick analysis, lower accuracy
  - `"full"`: Balanced (default)
  - `"accurate"`: Slower, higher accuracy
- **Memory**: Python process memory usage scales with screenshot count
- **Parallelization**: Service supports multiple concurrent analyses (tracked by process ID)

## Troubleshooting

### Python not found

```typescript
service.setPythonPath("/usr/local/bin/python3");
```

### Bridge script not found

```typescript
service.setBridgeScriptPath("/absolute/path/to/bridge.py");
```

### Timeout errors

```typescript
// Increase timeout
const result = await service.processSession(path, {
  timeout: 600000, // 10 minutes
});
```

### JSON parse errors

- Check Python script output format
- Ensure Python script uses `print()` for JSON output only
- Use `console.error()` in Python instead of `print()` for debugging

## License

Part of the qontinui project.
