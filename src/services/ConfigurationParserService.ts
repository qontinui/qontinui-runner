/**
 * ConfigurationParserService
 *
 * Pure parsing and filtering logic for configuration data.
 * Responsibility: Parse raw config data and filter workflows.
 */

export interface ConfigImage {
  id: string;
  name?: string;
  data?: string;
  [key: string]: unknown;
}

export interface ConfigState {
  id: string;
  name?: string;
  [key: string]: unknown;
}

export interface Workflow {
  id: string;
  name: string;
  category?: string;
  visibility?: string;
}

export interface Config {
  name: string;
  version: string;
  statesCount: number;
  workflowsCount: number;
  workflows: Workflow[];
  images?: ConfigImage[];
  states?: ConfigState[];
  path: string;
}

export interface ConfigRawData {
  states?: ConfigState[];
  workflows?: Workflow[];
  images?: ConfigImage[];
}

export interface FilterStats {
  total: number;
  main: number;
  internal: number;
  categoryCounts: Record<string, number>;
  visibilityCounts: Record<string, number>;
}

export interface ParsedConfig {
  config: Config;
  mainWorkflows: Workflow[];
  allWorkflows: Workflow[];
  filterStats: FilterStats;
}

/**
 * Service for parsing and filtering configuration data.
 * Follows Single Responsibility Principle - only handles parsing/filtering.
 */
export class ConfigurationParserService {
  /**
   * Filter workflows to only include "Main" category with public visibility.
   * Case-insensitive category matching.
   */
  filterMainWorkflows(workflows: Workflow[]): Workflow[] {
    return workflows.filter(
      (w) =>
        w.category?.toLowerCase() === "main" && (!w.visibility || w.visibility !== "internal"),
    );
  }

  /**
   * Calculate filter statistics for debugging.
   */
  calculateFilterStats(allWorkflows: Workflow[], mainWorkflows: Workflow[]): FilterStats {
    const categoryCounts: Record<string, number> = {};
    const visibilityCounts: Record<string, number> = {};

    allWorkflows.forEach((w) => {
      const cat = w.category || "No category";
      const vis = w.visibility || "public";
      categoryCounts[cat] = (categoryCounts[cat] || 0) + 1;
      visibilityCounts[vis] = (visibilityCounts[vis] || 0) + 1;
    });

    return {
      total: allWorkflows.length,
      main: mainWorkflows.length,
      internal: allWorkflows.length - mainWorkflows.length,
      categoryCounts,
      visibilityCounts,
    };
  }

  /**
   * Extract filename without extension from a path.
   */
  extractConfigName(configPath: string): string {
    const pathParts = configPath.split(/[/\\]/);
    const fullFilename = pathParts[pathParts.length - 1] || "config.json";
    return fullFilename.replace(/\.[^/.]+$/, "");
  }

  /**
   * Parse raw config data into a structured ParsedConfig.
   * This is the main entry point for config parsing.
   *
   * @param rawData - The raw data from the backend (states, workflows, images)
   * @param configPath - The file path of the config
   * @param overrideName - Optional name override (e.g., from metadata)
   * @param overrideVersion - Optional version override
   */
  parseConfig(
    rawData: ConfigRawData | undefined,
    configPath: string,
    overrideName?: string,
    overrideVersion?: string,
  ): ParsedConfig {
    const allWorkflows = rawData?.workflows || [];
    const mainWorkflows = this.filterMainWorkflows(allWorkflows);
    const filterStats = this.calculateFilterStats(allWorkflows, mainWorkflows);

    const config: Config = {
      name: overrideName || this.extractConfigName(configPath),
      version: overrideVersion || "1.0.0",
      statesCount: rawData?.states?.length || 0,
      workflowsCount: allWorkflows.length,
      workflows: allWorkflows,
      images: rawData?.images || [],
      states: rawData?.states || [],
      path: configPath,
    };

    return {
      config,
      mainWorkflows,
      allWorkflows,
      filterStats,
    };
  }
}

/**
 * Singleton instance for use throughout the app.
 */
export const configurationParser = new ConfigurationParserService();
