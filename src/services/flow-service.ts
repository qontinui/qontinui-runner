/**
 * Flow Service
 *
 * Provides Tauri invoke calls for the Flow Designer.
 * Wraps the flow Tauri commands from src-tauri/src/commands/flow.rs
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  Flow,
  FlowState,
  FlowSummary,
  FlowExecutionSummary,
  FlowExecutionFilter,
  PaginatedFlowExecutionResult,
} from "../types/flow";

/**
 * Service for managing deterministic flow workflows via Tauri commands.
 */
export const flowService = {
  // ===========================================================================
  // Flow CRUD Operations
  // ===========================================================================

  /**
   * List all saved flows.
   * @returns Array of flow summaries.
   */
  async listFlows(): Promise<FlowSummary[]> {
    return invoke("list_flows");
  },

  /**
   * Get a flow by ID.
   * @param id - Flow ID.
   * @returns The flow definition or null if not found.
   */
  async getFlow(id: string): Promise<Flow | null> {
    return invoke("get_flow", { id });
  },

  /**
   * Save a flow (create or update).
   * @param flow - Flow definition to save.
   * @returns The saved flow ID.
   */
  async saveFlow(flow: Flow): Promise<string> {
    return invoke("save_flow", { flow });
  },

  /**
   * Delete a flow by ID.
   * @param id - Flow ID to delete.
   * @returns True if deleted, false if not found.
   */
  async deleteFlow(id: string): Promise<boolean> {
    return invoke("delete_flow", { id });
  },

  /**
   * Validate a flow definition.
   * @param flow - Flow to validate.
   * @returns Array of validation errors (empty if valid).
   */
  async validateFlow(flow: Flow): Promise<string[]> {
    return invoke("validate_flow", { flow });
  },

  // ===========================================================================
  // Flow Execution
  // ===========================================================================

  /**
   * Start a flow execution.
   * @param flowId - ID of the flow to execute.
   * @param inputs - Input values for the flow.
   * @returns Instance ID of the execution.
   */
  async startFlowExecution(flowId: string, inputs: Record<string, unknown> = {}): Promise<string> {
    return invoke("start_flow_execution", { flowId, inputs });
  },

  /**
   * Get the current state of a flow execution.
   * @param instanceId - Execution instance ID.
   * @returns Flow state or null if not found.
   */
  async getFlowExecution(instanceId: string): Promise<FlowState | null> {
    return invoke("get_flow_execution", { instanceId });
  },

  /**
   * List all flow executions.
   * @returns Array of execution summaries.
   */
  async listFlowExecutions(): Promise<FlowExecutionSummary[]> {
    return invoke("list_flow_executions");
  },

  /**
   * Cancel a flow execution.
   * @param instanceId - Execution instance ID to cancel.
   * @returns True if cancelled, false if not found.
   */
  async cancelFlowExecution(instanceId: string): Promise<boolean> {
    return invoke("cancel_flow_execution", { instanceId });
  },

  // ===========================================================================
  // Sample Data
  // ===========================================================================

  /**
   * Create a sample flow for demonstration.
   * @returns The sample flow definition.
   */
  async createSampleFlow(): Promise<Flow> {
    return invoke("create_sample_flow");
  },

  /**
   * Add the sample flow to storage.
   * @returns The saved flow ID.
   */
  async addSampleFlow(): Promise<string> {
    return invoke("add_sample_flow");
  },

  // ===========================================================================
  // Enhanced Queries (Tag Filtering, Execution Filtering, Pagination)
  // ===========================================================================

  /**
   * Get flows filtered by tag.
   * @param tag - Tag to search for.
   * @returns Array of flow summaries matching the tag.
   */
  async getFlowsByTag(tag: string): Promise<FlowSummary[]> {
    return invoke("get_flows_by_tag", { tag });
  },

  /**
   * Get flow executions with optional filtering.
   * @param filter - Filter options (flow_id, status).
   * @returns Array of filtered flow execution summaries.
   */
  async getFlowExecutionsFiltered(
    filter: FlowExecutionFilter = {},
  ): Promise<FlowExecutionSummary[]> {
    return invoke("get_flow_executions_filtered", { filter });
  },

  /**
   * Get flow executions with pagination.
   * @param flowId - Optional flow ID to filter by.
   * @param offset - Number of records to skip.
   * @param limit - Maximum number of records to return.
   * @returns Paginated result with items, total count, offset, and limit.
   */
  async getFlowExecutionsPaginated(
    flowId: string | undefined,
    offset: number,
    limit: number,
  ): Promise<PaginatedFlowExecutionResult> {
    return invoke("get_flow_executions_paginated", { flowId, offset, limit });
  },

  /**
   * Get total count of flow executions.
   * @param flowId - Optional flow ID to filter by.
   * @returns Total number of flow execution records.
   */
  async getFlowExecutionsCount(flowId?: string): Promise<number> {
    return invoke("get_flow_executions_count", { flowId });
  },

  // ===========================================================================
  // Flow Version History
  // ===========================================================================

  /**
   * Create a new version snapshot of a flow.
   * @param flowId - Flow ID.
   * @param message - Optional version message.
   * @param createdBy - Optional creator identifier.
   * @returns The created version summary.
   */
  async createFlowVersion(
    flowId: string,
    message?: string,
    createdBy?: string
  ): Promise<{ id: string; version: number }> {
    return invoke("create_flow_version", { flowId, message, createdBy });
  },

  /**
   * List all versions of a flow.
   * @param flowId - Flow ID.
   * @returns Array of version summaries.
   */
  async listFlowVersions(
    flowId: string
  ): Promise<Array<{ id: string; version: number; message?: string; created_at: string }>> {
    return invoke("list_flow_versions", { flowId });
  },

  /**
   * Get a specific version of a flow.
   * @param flowId - Flow ID.
   * @param version - Version number.
   * @returns The version data with full definition.
   */
  async getFlowVersion(
    flowId: string,
    version: number
  ): Promise<{ definition: Flow; message?: string } | null> {
    return invoke("get_flow_version", { flowId, version });
  },

  /**
   * Restore a flow to a specific version.
   * @param flowId - Flow ID.
   * @param version - Version number to restore.
   * @param createdBy - Optional creator identifier.
   * @returns Result with restored flow info.
   */
  async restoreFlowVersion(
    flowId: string,
    version: number,
    createdBy?: string
  ): Promise<{ new_version: number }> {
    return invoke("restore_flow_version", { flowId, version, createdBy });
  },

  // ===========================================================================
  // Flow Import/Export
  // ===========================================================================

  /**
   * Export a flow to JSON format.
   * @param flowId - Flow ID to export.
   * @returns JSON string of the flow export.
   */
  async exportFlowJson(flowId: string): Promise<string> {
    return invoke("export_flow_json", { flowId });
  },

  /**
   * Export a flow to YAML format.
   * @param flowId - Flow ID to export.
   * @returns YAML string of the flow export.
   */
  async exportFlowYaml(flowId: string): Promise<string> {
    return invoke("export_flow_yaml", { flowId });
  },

  /**
   * Import a flow from JSON format.
   * @param content - JSON content to import.
   * @param generateNewId - Whether to generate a new ID for the flow.
   * @returns The imported flow.
   */
  async importFlowJson(content: string, generateNewId: boolean = false): Promise<Flow> {
    return invoke("import_flow_json", { content, generateNewId });
  },

  /**
   * Import a flow from YAML format.
   * @param content - YAML content to import.
   * @param generateNewId - Whether to generate a new ID for the flow.
   * @returns The imported flow.
   */
  async importFlowYaml(content: string, generateNewId: boolean = false): Promise<Flow> {
    return invoke("import_flow_yaml", { content, generateNewId });
  },

  /**
   * Export multiple flows to a single JSON file.
   * @param flowIds - Array of flow IDs to export.
   * @returns JSON string of the bulk export.
   */
  async exportFlowsBulk(flowIds: string[]): Promise<string> {
    return invoke("export_flows_bulk", { flowIds });
  },

  /**
   * Import multiple flows from a bulk export.
   * @param content - JSON content to import.
   * @param generateNewIds - Whether to generate new IDs for the flows.
   * @returns Array of imported flows.
   */
  async importFlowsBulk(content: string, generateNewIds: boolean = false): Promise<Flow[]> {
    return invoke("import_flows_bulk", { content, generateNewIds });
  },
};
