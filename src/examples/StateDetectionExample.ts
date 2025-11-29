/**
 * Example usage of StateDetectionService
 *
 * This file demonstrates how to use the StateDetectionService to analyze
 * screenshots and detect application states.
 */

import { join } from "path";
import {
  StateDetectionService,
  StateDetectionError,
  StateDetectionErrorType,
  type StateDetectionConfig,
  type StateDetectionResult,
} from "../services/StateDetectionService";

/**
 * Basic usage example - analyze a session with default settings
 */
async function basicExample() {
  console.log("=== Basic Example ===\n");

  const service = new StateDetectionService();

  try {
    const sessionPath = "/path/to/screenshots/session_123";

    console.log(`Analyzing session: ${sessionPath}`);

    const result = await service.processSession(sessionPath);

    console.log(`\nAnalysis complete in ${result.processingTime}ms`);
    console.log(`Success: ${result.success}`);
    console.log(`Total screenshots: ${result.totalScreenshots}`);
    console.log(`State images found: ${result.stateImages.length}`);
    console.log(`States detected: ${result.states.length}`);

    // Display some state images
    console.log("\nDetected UI Elements:");
    result.stateImages.slice(0, 5).forEach((img, index) => {
      console.log(
        `  ${index + 1}. ${img.name} - ${img.width}x${img.height} at (${img.x}, ${img.y})`,
      );
      console.log(`     Tags: ${img.tags.join(", ")}`);
      console.log(`     Frequency: ${(img.frequency * 100).toFixed(1)}%`);
    });

    // Display states
    console.log("\nDetected States:");
    result.states.forEach((state, index) => {
      console.log(
        `  ${index + 1}. ${state.name} - Confidence: ${(state.confidence * 100).toFixed(1)}%`,
      );
      console.log(`     Elements: ${state.stateImageIds.length}`);
      console.log(`     Screenshots: ${state.screenshots.length}`);
    });
  } catch (error) {
    if (error instanceof StateDetectionError) {
      console.error(`\nDetection failed: ${error.type}`);
      console.error(`Message: ${error.message}`);
    } else {
      console.error("\nUnexpected error:", error);
    }
  }
}

/**
 * Advanced usage example - custom configuration and error handling
 */
async function advancedExample() {
  console.log("\n\n=== Advanced Example ===\n");

  // Create service with custom configuration
  const service = new StateDetectionService({
    pythonPath: "python3",
    bridgeScriptPath: join(__dirname, "..", "..", "python", "state_detection_bridge.py"),
    defaultTimeout: 600000, // 10 minutes
  });

  const config: StateDetectionConfig = {
    // Focus on smaller UI elements
    minRegionSize: [10, 10],
    maxRegionSize: [300, 300],

    // High precision settings
    stabilityThreshold: 0.99,
    similarityThreshold: 0.98,
    colorTolerance: 3,

    // Require elements to appear in at least 3 screenshots
    minScreenshotsPresent: 3,

    // Use accurate mode for best results
    processingMode: "accurate",

    // Enable all analysis features
    enableRectangleDecomposition: true,
    enableCooccurrenceAnalysis: true,

    // Analyze only a specific region (e.g., game UI area)
    region: {
      x: 0,
      y: 0,
      width: 1920,
      height: 1080,
    },

    // Custom timeout for large datasets
    timeout: 600000,
  };

  try {
    const sessionPath = "/path/to/large/session";

    console.log(`Analyzing session with custom config: ${sessionPath}`);
    console.log(`Config:`, JSON.stringify(config, null, 2));

    const result = await service.processSession(sessionPath, config);

    console.log(`\nAnalysis complete in ${result.processingTime}ms`);

    // Filter results by criteria
    const buttons = result.stateImages.filter((img) => img.tags.includes("button"));
    console.log(`\nFound ${buttons.length} buttons`);

    const highFrequencyElements = result.stateImages.filter((img) => img.frequency > 0.8);
    console.log(`Found ${highFrequencyElements.length} high-frequency elements (>80%)`);

    const highConfidenceStates = result.states.filter((state) => state.confidence > 0.9);
    console.log(`Found ${highConfidenceStates.length} high-confidence states (>90%)`);
  } catch (error) {
    if (error instanceof StateDetectionError) {
      handleDetectionError(error);
    } else {
      console.error("\nUnexpected error:", error);
    }
  } finally {
    // Cleanup
    console.log(`\nActive processes: ${service.getActiveProcessCount()}`);
    service.killAllProcesses();
  }
}

/**
 * Example error handling function
 */
function handleDetectionError(error: StateDetectionError): void {
  console.error(`\nDetection Error: ${error.type}`);

  switch (error.type) {
    case StateDetectionErrorType.PYTHON_NOT_FOUND:
      console.error("Python is not installed or not in PATH");
      console.error("Install Python 3.8+ and add it to your PATH");
      break;

    case StateDetectionErrorType.BRIDGE_SCRIPT_NOT_FOUND:
      console.error("Bridge script not found");
      console.error("Ensure the Python bridge script is in the correct location");
      console.error("Expected location:", error.details);
      break;

    case StateDetectionErrorType.TIMEOUT:
      console.error("Analysis timed out");
      console.error("Try increasing the timeout or using 'fast' processing mode");
      break;

    case StateDetectionErrorType.INVALID_JSON:
      console.error("Failed to parse Python output");
      console.error("Check Python script for print statements that interfere with JSON output");
      console.error("Details:", error.details);
      break;

    case StateDetectionErrorType.ANALYSIS_FAILED:
      console.error("Python analysis failed");
      console.error("Message:", error.message);
      console.error("Check Python stderr output for details");
      break;

    case StateDetectionErrorType.PROCESS_CRASHED:
      console.error("Python process crashed");
      console.error("Exit code:", (error.details as any)?.exitCode);
      console.error("Stderr:", (error.details as any)?.stderr);
      break;

    default:
      console.error("Unknown error:", error.message);
      console.error("Details:", error.details);
  }
}

/**
 * Example of analyzing multiple sessions in sequence
 */
async function batchExample() {
  console.log("\n\n=== Batch Analysis Example ===\n");

  const service = new StateDetectionService();

  const sessionPaths = ["/path/to/session_1", "/path/to/session_2", "/path/to/session_3"];

  const results: StateDetectionResult[] = [];

  for (const sessionPath of sessionPaths) {
    try {
      console.log(`\nAnalyzing: ${sessionPath}`);

      const result = await service.processSession(sessionPath, {
        processingMode: "fast",
        timeout: 300000,
      });

      results.push(result);

      console.log(
        `  ✓ Complete - ${result.stateImages.length} elements, ${result.states.length} states`,
      );
    } catch (error) {
      console.error(`  ✗ Failed: ${error instanceof Error ? error.message : String(error)}`);
      continue;
    }
  }

  // Aggregate results
  const totalElements = results.reduce((sum, r) => sum + r.stateImages.length, 0);
  const totalStates = results.reduce((sum, r) => sum + r.states.length, 0);
  const totalScreenshots = results.reduce((sum, r) => sum + r.totalScreenshots, 0);

  console.log("\n=== Batch Summary ===");
  console.log(`Sessions analyzed: ${results.length}/${sessionPaths.length}`);
  console.log(`Total screenshots: ${totalScreenshots}`);
  console.log(`Total elements: ${totalElements}`);
  console.log(`Total states: ${totalStates}`);
}

/**
 * Example of working with detection results
 */
function analyzeResults(result: StateDetectionResult) {
  console.log("\n\n=== Result Analysis Example ===\n");

  // Group elements by type
  const elementsByType = new Map<string, typeof result.stateImages>();
  result.stateImages.forEach((img) => {
    img.tags.forEach((tag) => {
      if (!elementsByType.has(tag)) {
        elementsByType.set(tag, []);
      }
      elementsByType.get(tag)!.push(img);
    });
  });

  console.log("Elements by type:");
  elementsByType.forEach((elements, type) => {
    console.log(`  ${type}: ${elements.length}`);
  });

  // Find overlay elements (high frequency, indicating UI that's always visible)
  const overlayElements = result.stateImages.filter((img) => img.frequency > 0.9);
  console.log(`\nPersistent overlay elements: ${overlayElements.length}`);

  // Find state-specific elements (lower frequency)
  const stateSpecificElements = result.stateImages.filter(
    (img) => img.frequency < 0.5 && img.frequency > 0.1,
  );
  console.log(`State-specific elements: ${stateSpecificElements.length}`);

  // Analyze state composition
  console.log("\nState composition:");
  result.states.forEach((state) => {
    const elementCount = state.stateImageIds.length;
    const avgElementFreq =
      state.stateImageIds.reduce((sum, id) => {
        const img = result.stateImages.find((i) => i.id === id);
        return sum + (img?.frequency || 0);
      }, 0) / elementCount;

    console.log(`  ${state.name}:`);
    console.log(`    Elements: ${elementCount}`);
    console.log(`    Avg element frequency: ${(avgElementFreq * 100).toFixed(1)}%`);
    console.log(`    Confidence: ${(state.confidence * 100).toFixed(1)}%`);
  });

  // Spatial analysis - find elements by region
  const regions = {
    topLeft: { x: 0, y: 0, width: 640, height: 360 },
    topRight: { x: 1280, y: 0, width: 640, height: 360 },
    bottomLeft: { x: 0, y: 720, width: 640, height: 360 },
    bottomRight: { x: 1280, y: 720, width: 640, height: 360 },
  };

  console.log("\nElements by screen region:");
  Object.entries(regions).forEach(([name, region]) => {
    const elementsInRegion = result.stateImages.filter(
      (img) =>
        img.x >= region.x &&
        img.x < region.x + region.width &&
        img.y >= region.y &&
        img.y < region.y + region.height,
    );
    console.log(`  ${name}: ${elementsInRegion.length} elements`);
  });
}

/**
 * Run all examples
 */
async function runAllExamples() {
  console.log("StateDetectionService Examples");
  console.log("================================\n");

  // Note: These examples use placeholder paths
  // Replace with actual session paths to run

  // await basicExample();
  // await advancedExample();
  // await batchExample();

  console.log("\n\nExamples complete!");
  console.log("To run these examples, update the session paths and uncomment the function calls.");
}

// Export examples
export { basicExample, advancedExample, batchExample, analyzeResults, handleDetectionError };

// Run examples if this file is executed directly
if (require.main === module) {
  runAllExamples().catch(console.error);
}
