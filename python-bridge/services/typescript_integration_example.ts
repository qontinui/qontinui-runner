/**
 * TypeScript Integration Example for State Detection Service
 *
 * This file demonstrates how to integrate the Python state detection service
 * with qontinui-runner TypeScript code.
 */

import { exec } from 'child_process';
import { promisify } from 'util';
import * as fs from 'fs/promises';
import * as path from 'path';

const execAsync = promisify(exec);

// ============================================================================
// Type Definitions
// ============================================================================

interface StateImage {
  name: string;
  similarity: number;
  bbox: [number, number, number, number] | null;
  context: string | null;
}

interface StateRegion {
  name: string;
  type: string | null;
  bbox: [number, number, number, number] | null;
}

interface StateLocation {
  name: string;
  x: number;
  y: number;
  target_state: string | null;
  confidence: number | null;
}

interface StateBoundary {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface DetectedState {
  name: string;
  description: string;
  state_images: StateImage[];
  state_regions: StateRegion[];
  state_locations: StateLocation[];
  boundary: StateBoundary | null;
  metadata: {
    num_images: number;
    num_regions: number;
    num_locations: number;
  };
}

interface StateDetectionResult {
  version: string;
  service: string;
  screenshots_dir: string;
  num_states: number;
  num_transitions: number;
  states: DetectedState[];
}

interface InputEvent {
  timestamp: number;
  event_type: 'click' | 'key' | 'move';
  x?: number;
  y?: number;
  button?: 'left' | 'right' | 'middle';
  key?: string;
  metadata?: Record<string, any>;
}

// ============================================================================
// State Detection Service Client
// ============================================================================

export class StateDetectionServiceClient {
  private pythonScriptPath: string;

  constructor(pythonBridgeDir?: string) {
    // Default to relative path from qontinui-runner
    const bridgeDir = pythonBridgeDir || path.join(__dirname, '../python-bridge');
    this.pythonScriptPath = path.join(bridgeDir, 'services/state_detection_service.py');
  }

  /**
   * Detect states from a capture session
   *
   * @param screenshotsDir - Directory containing PNG screenshots
   * @param eventsFile - JSON file with input events
   * @param outputFile - Where to write the output JSON
   * @returns Promise with detected states
   */
  async detectStates(
    screenshotsDir: string,
    eventsFile: string,
    outputFile: string
  ): Promise<StateDetectionResult> {
    console.log('[StateDetection] Starting state detection...');
    console.log(`  Screenshots: ${screenshotsDir}`);
    console.log(`  Events: ${eventsFile}`);
    console.log(`  Output: ${outputFile}`);

    // Build command (use python3 to ensure Python 3.x)
    const command = `python3 "${this.pythonScriptPath}" "${screenshotsDir}" "${eventsFile}" "${outputFile}"`;

    try {
      // Execute Python script
      const { stdout, stderr } = await execAsync(command, {
        maxBuffer: 10 * 1024 * 1024, // 10MB buffer for large outputs
      });

      // Log Python output
      if (stdout) {
        console.log('[StateDetection] Python output:', stdout);
      }
      if (stderr) {
        console.error('[StateDetection] Python stderr:', stderr);
      }

      // Read and parse output file
      const outputData = await fs.readFile(outputFile, 'utf-8');
      const result: StateDetectionResult = JSON.parse(outputData);

      console.log('[StateDetection] Success!');
      console.log(`  Detected ${result.num_states} states`);
      console.log(`  Found ${result.num_transitions} transitions`);

      return result;
    } catch (error) {
      console.error('[StateDetection] Failed to detect states:', error);
      throw new Error(`State detection failed: ${error}`);
    }
  }

  /**
   * Create events JSON file from captured events
   *
   * @param events - Array of input events
   * @param outputFile - Where to write the events JSON
   */
  async saveEvents(events: InputEvent[], outputFile: string): Promise<void> {
    const eventsData = {
      events: events.map(e => ({
        timestamp: e.timestamp,
        event_type: e.event_type,
        x: e.x,
        y: e.y,
        button: e.button,
        key: e.key,
        metadata: e.metadata,
      })),
    };

    await fs.writeFile(outputFile, JSON.stringify(eventsData, null, 2));
    console.log(`[StateDetection] Saved ${events.length} events to ${outputFile}`);
  }

  /**
   * Process a complete capture session
   *
   * This is a convenience method that handles the full workflow:
   * 1. Save events to JSON
   * 2. Run state detection
   * 3. Return results
   *
   * @param screenshotsDir - Directory with screenshots
   * @param events - Captured input events
   * @param outputDir - Where to write output files
   * @returns Promise with detected states
   */
  async processCaptureSession(
    screenshotsDir: string,
    events: InputEvent[],
    outputDir: string
  ): Promise<StateDetectionResult> {
    // Create output directory if needed
    await fs.mkdir(outputDir, { recursive: true });

    // Save events to JSON
    const eventsFile = path.join(outputDir, 'events.json');
    await this.saveEvents(events, eventsFile);

    // Run state detection
    const outputFile = path.join(outputDir, 'states.json');
    return await this.detectStates(screenshotsDir, eventsFile, outputFile);
  }
}

// ============================================================================
// Example Usage
// ============================================================================

async function exampleUsage() {
  // Create service client
  const client = new StateDetectionServiceClient();

  // Example events (from capture session)
  const events: InputEvent[] = [
    {
      timestamp: 1700000000.123,
      event_type: 'click',
      x: 100,
      y: 200,
      button: 'left',
    },
    {
      timestamp: 1700000001.456,
      event_type: 'key',
      key: 'Enter',
    },
    {
      timestamp: 1700000002.789,
      event_type: 'click',
      x: 300,
      y: 400,
      button: 'left',
    },
  ];

  try {
    // Process capture session
    const result = await client.processCaptureSession(
      './captures/session1', // screenshots directory
      events, // captured events
      './output' // output directory
    );

    // Use the detected states
    console.log(`\nDetected ${result.num_states} states:`);

    for (const state of result.states) {
      console.log(`\n  State: ${state.name}`);
      console.log(`    Description: ${state.description}`);
      console.log(`    StateImages: ${state.state_images.length}`);
      console.log(`    StateRegions: ${state.state_regions.length}`);
      console.log(`    StateLocations: ${state.state_locations.length}`);

      // Access specific elements
      for (const img of state.state_images) {
        console.log(`      - Image: ${img.name} (similarity: ${img.similarity})`);
      }

      for (const location of state.state_locations) {
        console.log(
          `      - Location: ${location.name} at (${location.x}, ${location.y})`
        );
      }
    }
  } catch (error) {
    console.error('Error:', error);
  }
}

// ============================================================================
// Integration with Existing qontinui-runner Code
// ============================================================================

/**
 * Example integration with capture tool
 */
export async function integrateWithCaptureTool(
  captureDir: string,
  capturedEvents: InputEvent[]
): Promise<StateDetectionResult> {
  const client = new StateDetectionServiceClient();

  // Assume screenshots are saved in captureDir/screenshots
  const screenshotsDir = path.join(captureDir, 'screenshots');

  // Process the capture
  const outputDir = path.join(captureDir, 'output');
  return await client.processCaptureSession(screenshotsDir, capturedEvents, outputDir);
}

/**
 * Example integration with workflow recording
 */
export async function detectStatesFromRecording(
  recordingId: string,
  dataDir: string
): Promise<StateDetectionResult> {
  const client = new StateDetectionServiceClient();

  // Paths based on recording structure
  const recordingDir = path.join(dataDir, 'recordings', recordingId);
  const screenshotsDir = path.join(recordingDir, 'screenshots');
  const eventsFile = path.join(recordingDir, 'events.json');
  const outputFile = path.join(recordingDir, 'states.json');

  return await client.detectStates(screenshotsDir, eventsFile, outputFile);
}

/**
 * Convert detected states to qontinui-runner state format
 */
export function convertToRunnerStateFormat(detectedState: DetectedState): any {
  // Convert Python-detected state to qontinui-runner's internal format
  return {
    name: detectedState.name,
    description: detectedState.description,

    // Convert state images to patterns
    patterns: detectedState.state_images.map(img => ({
      name: img.name,
      similarity: img.similarity,
      region: img.bbox
        ? {
            x: img.bbox[0],
            y: img.bbox[1],
            width: img.bbox[2],
            height: img.bbox[3],
          }
        : null,
    })),

    // Convert state regions to clickable areas
    regions: detectedState.state_regions.map(region => ({
      name: region.name,
      type: region.type || 'panel',
      bounds: region.bbox
        ? {
            x: region.bbox[0],
            y: region.bbox[1],
            width: region.bbox[2],
            height: region.bbox[3],
          }
        : null,
    })),

    // Convert state locations to transitions
    transitions: detectedState.state_locations.map(location => ({
      name: location.name,
      targetState: location.target_state,
      clickPoint: { x: location.x, y: location.y },
      confidence: location.confidence || 0.85,
    })),

    // Add boundary if present
    boundary: detectedState.boundary,
  };
}

// Export for use in qontinui-runner
export default StateDetectionServiceClient;

// Uncomment to run example
// exampleUsage().catch(console.error);
