import { describe, it, expect } from "vitest";
import { configurationParser } from "./ConfigurationParserService";
import * as fs from "fs";

describe("ConfigurationParserService Integration", () => {
  it("should load and parse the bdo_latest_config (5).json correctly", () => {
    const configPath = "d:/qontinui_parent_directory/configurations/bdo_latest_config (5).json";

    // Skip if the fixture file doesn't exist on this machine
    if (!fs.existsSync(configPath)) {
      console.log(`Skipping test: fixture file not found at ${configPath}`);
      return;
    }

    // Read the file content
    const fileContent = fs.readFileSync(configPath, "utf-8");
    const jsonContent = JSON.parse(fileContent);

    // Parse the config
    const parsed = configurationParser.parseConfig(jsonContent, configPath);

    // Verify basic structure
    expect(parsed).toBeDefined();
    expect(parsed.config).toBeDefined();
    expect(parsed.config.name).toBe("bdo_latest_config (5)"); // Defaults to filename when no override provided

    // Verify workflows
    expect(parsed.allWorkflows.length).toBeGreaterThan(0);
    expect(parsed.config.workflows.length).toBeGreaterThan(0);

    // Check specific workflow logic if needed
    // The file has "Select grind in processing" workflow
    const grindWorkflow = parsed.allWorkflows.find((w) => w.name === "Select grind in processing");
    expect(grindWorkflow).toBeDefined();
    expect(grindWorkflow?.category).toBe("Grind corn");

    console.log(`Successfully loaded config with ${parsed.allWorkflows.length} workflows.`);
  });
});
