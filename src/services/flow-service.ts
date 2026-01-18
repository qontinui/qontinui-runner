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
};
