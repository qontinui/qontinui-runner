import { spawn, ChildProcess } from "child_process";
import { join } from "path";

/**
 * Represents a detected UI element within a state.
 */
export interface StateImageInfo {
  /** Unique identifier for the element */
  id: string;
  /** Human-readable name for the element */
  name: string;
  /** X coordinate of top-left corner */
  x: number;
  /** Y coordinate of top-left corner */
  y: number;
  /** X coordinate of bottom-right corner */
  x2: number;
  /** Y coordinate of bottom-right corner */
  y2: number;
  /** Width of the element */
  width: number;
  /** Height of the element */
  height: number;
  /** Hash of the element's pixel data */
  pixelHash: string;
  /** Frequency of appearance (0.0 to 1.0) */
  frequency: number;
  /** List of screenshot IDs containing this element */
  screenshots: string[];
  /** Element type tags (e.g., "button", "input", "icon") */
  tags: string[];
  /** Percentage of dark pixels in the element */
  darkPixelPercentage?: number;
  /** Percentage of light pixels in the element */
  lightPixelPercentage?: number;
  /** Density of the mask (0.0 to 1.0) */
  maskDensity: number;
  /** Whether the element has a mask */
  hasMask: boolean;
}

/**
 * Represents a region of interest in the screenshot.
 */
export interface StateRegionInfo {
  /** X coordinate of top-left corner */
  x: number;
  /** Y coordinate of top-left corner */
  y: number;
  /** Width of the region */
  width: number;
  /** Height of the region */
  height: number;
}

/**
 * Represents a location match for state detection.
 */
export interface StateLocationInfo {
  /** X coordinate */
  x: number;
  /** Y coordinate */
  y: number;
  /** Detection confidence (0.0 to 1.0) */
  confidence: number;
}

/**
 * Represents a detected application state.
 */
export interface DetectedState {
  /** Unique identifier for the state */
  id: string;
  /** Human-readable name for the state */
  name: string;
  /** IDs of StateImages that compose this state */
  stateImageIds: string[];
  /** Screenshot IDs where this state was detected */
  screenshots: string[];
  /** Detection confidence (0.0 to 1.0) */
  confidence: number;
  /** Additional metadata about the state */
  metadata: Record<string, unknown>;
}

/**
 * Configuration options for state detection analysis.
 */
export interface StateDetectionConfig {
  /** Minimum region size [width, height] in pixels */
  minRegionSize?: [number, number];
  /** Maximum region size [width, height] in pixels */
  maxRegionSize?: [number, number];
  /** Color tolerance for region matching (0-255) */
  colorTolerance?: number;
  /** Stability threshold for pixel stability analysis (0.0-1.0) */
  stabilityThreshold?: number;
  /** Variance threshold for region detection */
  varianceThreshold?: number;
  /** Minimum number of screenshots an element must appear in */
  minScreenshotsPresent?: number;
  /** Processing mode: "full" | "fast" | "accurate" */
  processingMode?: "full" | "fast" | "accurate";
  /** Enable rectangle decomposition for complex regions */
  enableRectangleDecomposition?: boolean;
  /** Enable co-occurrence analysis for state grouping */
  enableCooccurrenceAnalysis?: boolean;
  /** Similarity threshold for element matching (0.0-1.0) */
  similarityThreshold?: number;
  /** Optional region to focus analysis on */
  region?: StateRegionInfo;
  /** Maximum time to wait for analysis (in milliseconds) */
  timeout?: number;
}

/**
 * Result of state detection analysis.
 */
export interface StateDetectionResult {
  /** Whether the analysis completed successfully */
  success: boolean;
  /** List of detected UI elements */
  stateImages: StateImageInfo[];
  /** List of detected application states */
  states: DetectedState[];
  /** Total number of screenshots analyzed */
  totalScreenshots: number;
  /** Time taken for analysis in milliseconds */
  processingTime: number;
  /** Error message if analysis failed */
  error?: string;
  /** Additional metadata about the analysis */
  metadata?: Record<string, unknown>;
}

/**
 * Error types for state detection operations.
 */
export enum StateDetectionErrorType {
  PYTHON_NOT_FOUND = "PYTHON_NOT_FOUND",
  BRIDGE_SCRIPT_NOT_FOUND = "BRIDGE_SCRIPT_NOT_FOUND",
  PROCESS_SPAWN_FAILED = "PROCESS_SPAWN_FAILED",
  PROCESS_CRASHED = "PROCESS_CRASHED",
  TIMEOUT = "TIMEOUT",
  INVALID_JSON = "INVALID_JSON",
  INVALID_SESSION_PATH = "INVALID_SESSION_PATH",
  ANALYSIS_FAILED = "ANALYSIS_FAILED",
  UNKNOWN = "UNKNOWN",
}

/**
 * Custom error class for state detection operations.
 */
export class StateDetectionError extends Error {
  public readonly type: StateDetectionErrorType;
  public readonly details?: unknown;

  constructor(type: StateDetectionErrorType, message: string, details?: unknown) {
    super(message);
    this.name = "StateDetectionError";
    this.type = type;
    this.details = details;
  }
}

/**
 * Service for detecting application states from screenshots using Python bridge.
 *
 * This service spawns a Python process to analyze screenshots and detect UI elements
 * and application states. It handles process lifecycle, stdout/stderr parsing,
 * and error recovery.
 *
 * @example
 * ```typescript
 * const service = new StateDetectionService({
 *   pythonPath: "python3",
 *   bridgeScriptPath: "/path/to/state_detection_bridge.py"
 * });
 *
 * try {
 *   const result = await service.processSession(
 *     "/path/to/screenshots",
 *     {
 *       minRegionSize: [20, 20],
 *       maxRegionSize: [500, 500],
 *       stabilityThreshold: 0.98
 *     }
 *   );
 *
 *   console.log(`Found ${result.stateImages.length} elements`);
 *   console.log(`Detected ${result.states.length} states`);
 * } catch (error) {
 *   if (error instanceof StateDetectionError) {
 *     console.error(`Detection failed: ${error.type} - ${error.message}`);
 *   }
 * }
 * ```
 */
export class StateDetectionService {
  private pythonPath: string;
  private bridgeScriptPath: string;
  private defaultTimeout: number;
  private activeProcesses: Map<string, ChildProcess>;

  /**
   * Creates a new StateDetectionService instance.
   *
   * @param config - Service configuration
   * @param config.pythonPath - Path to Python interpreter (default: "python3")
   * @param config.bridgeScriptPath - Path to Python bridge script
   * @param config.defaultTimeout - Default timeout for analysis in milliseconds (default: 300000)
   */
  constructor(config?: {
    pythonPath?: string;
    bridgeScriptPath?: string;
    defaultTimeout?: number;
  }) {
    this.pythonPath = config?.pythonPath || "python3";
    this.bridgeScriptPath =
      config?.bridgeScriptPath ||
      join(__dirname, "..", "..", "python", "state_detection_bridge.py");
    this.defaultTimeout = config?.defaultTimeout || 300000; // 5 minutes default
    this.activeProcesses = new Map();
  }

  /**
   * Process a session of screenshots to detect states and UI elements.
   *
   * @param sessionPath - Path to directory containing screenshots
   * @param config - Detection configuration options
   * @returns Promise resolving to detection results
   * @throws {StateDetectionError} If analysis fails
   */
  async processSession(
    sessionPath: string,
    config?: StateDetectionConfig,
  ): Promise<StateDetectionResult> {
    const startTime = Date.now();
    const processId = `process_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

    // Build configuration with defaults
    const analysisConfig = {
      min_region_size: config?.minRegionSize || [20, 20],
      max_region_size: config?.maxRegionSize || [500, 500],
      color_tolerance: config?.colorTolerance || 5,
      stability_threshold: config?.stabilityThreshold || 0.98,
      variance_threshold: config?.varianceThreshold || 10.0,
      min_screenshots_present: config?.minScreenshotsPresent || 2,
      processing_mode: config?.processingMode || "full",
      enable_rectangle_decomposition: config?.enableRectangleDecomposition ?? true,
      enable_cooccurrence_analysis: config?.enableCooccurrenceAnalysis ?? true,
      similarity_threshold: config?.similarityThreshold || 0.95,
      region: config?.region || null,
    };

    try {
      // Spawn Python process
      const process = await this.spawnPythonBridge(sessionPath, analysisConfig, processId);

      // Wait for process to complete and collect output
      const output = await this.waitForCompletion(
        process,
        processId,
        config?.timeout || this.defaultTimeout,
      );

      // Parse and validate output
      const result = this.parseOutput(output);

      // Add processing time
      result.processingTime = Date.now() - startTime;

      return result;
    } catch (error) {
      // Clean up process if it exists
      this.cleanupProcess(processId);

      if (error instanceof StateDetectionError) {
        throw error;
      }

      throw new StateDetectionError(
        StateDetectionErrorType.UNKNOWN,
        `Unexpected error during state detection: ${error instanceof Error ? error.message : String(error)}`,
        error,
      );
    }
  }

  /**
   * Spawn the Python bridge process.
   *
   * @private
   */
  private spawnPythonBridge(
    sessionPath: string,
    config: Record<string, unknown>,
    processId: string,
  ): Promise<ChildProcess> {
    return new Promise((resolve, reject) => {
      try {
        const args = [this.bridgeScriptPath, sessionPath, JSON.stringify(config)];

        const process = spawn(this.pythonPath, args, {
          stdio: ["pipe", "pipe", "pipe"],
        });

        this.activeProcesses.set(processId, process);

        // Handle immediate spawn errors
        process.on("error", (error) => {
          reject(
            new StateDetectionError(
              StateDetectionErrorType.PROCESS_SPAWN_FAILED,
              `Failed to spawn Python process: ${error.message}`,
              error,
            ),
          );
        });

        // Wait for process to start successfully
        setTimeout(() => {
          if (process.pid) {
            resolve(process);
          } else {
            reject(
              new StateDetectionError(
                StateDetectionErrorType.PROCESS_SPAWN_FAILED,
                "Python process failed to start",
              ),
            );
          }
        }, 100);
      } catch (error) {
        reject(
          new StateDetectionError(
            StateDetectionErrorType.PROCESS_SPAWN_FAILED,
            `Failed to spawn Python process: ${error instanceof Error ? error.message : String(error)}`,
            error,
          ),
        );
      }
    });
  }

  /**
   * Wait for the Python process to complete and collect output.
   *
   * @private
   */
  private waitForCompletion(
    process: ChildProcess,
    processId: string,
    timeout: number,
  ): Promise<string> {
    return new Promise((resolve, reject) => {
      let stdout = "";
      let stderr = "";
      let timeoutId: NodeJS.Timeout | null = null;

      // Set up timeout
      timeoutId = setTimeout(() => {
        process.kill();
        reject(
          new StateDetectionError(
            StateDetectionErrorType.TIMEOUT,
            `State detection timed out after ${timeout}ms`,
            { stdout, stderr },
          ),
        );
      }, timeout);

      // Collect stdout
      process.stdout?.on("data", (data) => {
        stdout += data.toString();
      });

      // Collect stderr
      process.stderr?.on("data", (data) => {
        stderr += data.toString();
        // Log stderr for debugging
        console.error("[StateDetection] Python stderr:", data.toString());
      });

      // Handle process exit
      process.on("close", (code) => {
        if (timeoutId) {
          clearTimeout(timeoutId);
        }

        this.cleanupProcess(processId);

        if (code === 0) {
          resolve(stdout);
        } else {
          reject(
            new StateDetectionError(
              StateDetectionErrorType.PROCESS_CRASHED,
              `Python process exited with code ${code}`,
              { stdout, stderr, exitCode: code },
            ),
          );
        }
      });

      // Handle process errors
      process.on("error", (error) => {
        if (timeoutId) {
          clearTimeout(timeoutId);
        }

        this.cleanupProcess(processId);

        reject(
          new StateDetectionError(
            StateDetectionErrorType.PROCESS_CRASHED,
            `Python process error: ${error.message}`,
            { error, stdout, stderr },
          ),
        );
      });
    });
  }

  /**
   * Parse the JSON output from the Python bridge.
   *
   * @private
   */
  private parseOutput(output: string): StateDetectionResult {
    try {
      // Find JSON output (last complete JSON object in stdout)
      const lines = output.trim().split("\n");
      let jsonOutput = "";

      // Look for JSON starting from the end
      for (let i = lines.length - 1; i >= 0; i--) {
        const line = lines[i].trim();
        if (line.startsWith("{") || jsonOutput) {
          jsonOutput = line + jsonOutput;
          if (line.startsWith("{")) {
            break;
          }
        }
      }

      if (!jsonOutput) {
        throw new Error("No JSON output found in Python bridge response");
      }

      const rawData = JSON.parse(jsonOutput);

      // Check for error in response
      if (!rawData.success) {
        throw new StateDetectionError(
          StateDetectionErrorType.ANALYSIS_FAILED,
          rawData.error || "Analysis failed without error message",
          rawData,
        );
      }

      // Parse and transform the data to match our TypeScript interfaces
      const result: StateDetectionResult = {
        success: true,
        stateImages: this.parseStateImages(rawData.state_images || []),
        states: this.parseStates(rawData.states || []),
        totalScreenshots: rawData.total_screenshots || 0,
        processingTime: 0, // Will be set by caller
        metadata: rawData.metadata || {},
      };

      return result;
    } catch (error) {
      if (error instanceof StateDetectionError) {
        throw error;
      }

      throw new StateDetectionError(
        StateDetectionErrorType.INVALID_JSON,
        `Failed to parse Python bridge output: ${error instanceof Error ? error.message : String(error)}`,
        { output, error },
      );
    }
  }

  /**
   * Parse raw state image data into StateImageInfo objects.
   *
   * @private
   */
  private parseStateImages(rawImages: Array<Record<string, unknown>>): StateImageInfo[] {
    return rawImages.map((img) => ({
      id: String(img.id || ""),
      name: String(img.name || ""),
      x: Number(img.x || 0),
      y: Number(img.y || 0),
      x2: Number(img.x2 || 0),
      y2: Number(img.y2 || 0),
      width: Number(img.width || 0),
      height: Number(img.height || 0),
      pixelHash: String(img.pixel_hash || img.pixelHash || ""),
      frequency: Number(img.frequency || 0),
      screenshots: Array.isArray(img.screenshots) ? img.screenshots.map(String) : [],
      tags: Array.isArray(img.tags) ? img.tags.map(String) : [],
      darkPixelPercentage:
        img.dark_pixel_percentage !== undefined
          ? Number(img.dark_pixel_percentage)
          : img.darkPixelPercentage !== undefined
            ? Number(img.darkPixelPercentage)
            : undefined,
      lightPixelPercentage:
        img.light_pixel_percentage !== undefined
          ? Number(img.light_pixel_percentage)
          : img.lightPixelPercentage !== undefined
            ? Number(img.lightPixelPercentage)
            : undefined,
      maskDensity: Number(img.mask_density || img.maskDensity || 1.0),
      hasMask: Boolean(img.has_mask || img.hasMask || false),
    }));
  }

  /**
   * Parse raw state data into DetectedState objects.
   *
   * @private
   */
  private parseStates(rawStates: Array<Record<string, unknown>>): DetectedState[] {
    return rawStates.map((state) => ({
      id: String(state.id || ""),
      name: String(state.name || ""),
      stateImageIds: Array.isArray(state.state_image_ids)
        ? state.state_image_ids.map(String)
        : Array.isArray(state.stateImageIds)
          ? state.stateImageIds.map(String)
          : [],
      screenshots: Array.isArray(state.screenshots) ? state.screenshots.map(String) : [],
      confidence: Number(state.confidence || 0),
      metadata: (state.metadata as Record<string, unknown>) || {},
    }));
  }

  /**
   * Clean up a process and remove it from tracking.
   *
   * @private
   */
  private cleanupProcess(processId: string): void {
    const process = this.activeProcesses.get(processId);
    if (process && !process.killed) {
      process.kill();
    }
    this.activeProcesses.delete(processId);
  }

  /**
   * Kill all active processes (cleanup method).
   */
  killAllProcesses(): void {
    for (const [processId, process] of this.activeProcesses.entries()) {
      if (!process.killed) {
        process.kill();
      }
      this.activeProcesses.delete(processId);
    }
  }

  /**
   * Get the number of active Python processes.
   */
  getActiveProcessCount(): number {
    return this.activeProcesses.size;
  }

  /**
   * Update the Python path for the service.
   *
   * @param pythonPath - New path to Python interpreter
   */
  setPythonPath(pythonPath: string): void {
    this.pythonPath = pythonPath;
  }

  /**
   * Update the bridge script path for the service.
   *
   * @param bridgeScriptPath - New path to bridge script
   */
  setBridgeScriptPath(bridgeScriptPath: string): void {
    this.bridgeScriptPath = bridgeScriptPath;
  }

  /**
   * Update the default timeout for the service.
   *
   * @param timeout - New default timeout in milliseconds
   */
  setDefaultTimeout(timeout: number): void {
    this.defaultTimeout = timeout;
  }
}

// Export a singleton instance for convenience
export const stateDetectionService = new StateDetectionService();
