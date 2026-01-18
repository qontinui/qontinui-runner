/**
 * useFlowDesigner.ts
 *
 * React hooks for the Flow Designer.
 * Provides data fetching and state management for flows.
 */

import { useState, useEffect, useCallback } from "react";
import { flowService } from "../../services/flow-service";
import type { Flow, FlowSummary, FlowState, FlowExecutionSummary } from "../../types/flow";

interface UseFlowListResult {
  flows: FlowSummary[];
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
}

/**
 * Hook to fetch all flows
 */
export function useFlowList(): UseFlowListResult {
  const [flows, setFlows] = useState<FlowSummary[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchFlows = useCallback(async () => {
    setIsLoading(true);
    setError(null);
    try {
      const result = await flowService.listFlows();
      setFlows(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch flows");
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchFlows();
  }, [fetchFlows]);

  return { flows, isLoading, error, refetch: fetchFlows };
}

interface UseFlowResult {
  flow: Flow | null;
  isLoading: boolean;
  error: string | null;
  refetch: () => Promise<void>;
}

/**
 * Hook to fetch a single flow by ID
 */
export function useFlow(id: string | null): UseFlowResult {
  const [flow, setFlow] = useState<Flow | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchFlow = useCallback(async () => {
    if (!id) {
      setFlow(null);
      return;
    }

    setIsLoading(true);
    setError(null);
    try {
      const result = await flowService.getFlow(id);
      setFlow(result);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch flow");
    } finally {
      setIsLoading(false);
    }
  }, [id]);

  useEffect(() => {
    fetchFlow();
  }, [fetchFlow]);

  return { flow, isLoading, error, refetch: fetchFlow };
}

interface UseFlowExecutionsResult {
  executions: FlowExecutionSummary[];
  isLoading: boolean;
  refetch: () => Promise<void>;
}

/**
 * Hook to fetch flow executions
 */
export function useFlowExecutions(): UseFlowExecutionsResult {
  const [executions, setExecutions] = useState<FlowExecutionSummary[]>([]);
  const [isLoading, setIsLoading] = useState(true);

  const fetchExecutions = useCallback(async () => {
    setIsLoading(true);
    try {
      const result = await flowService.listFlowExecutions();
      setExecutions(result);
    } catch {
      setExecutions([]);
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchExecutions();
  }, [fetchExecutions]);

  return { executions, isLoading, refetch: fetchExecutions };
}

/**
 * Actions for flow management
 */
export const flowActions = {
  async saveFlow(flow: Flow): Promise<string> {
    return flowService.saveFlow(flow);
  },

  async deleteFlow(id: string): Promise<boolean> {
    return flowService.deleteFlow(id);
  },

  async validateFlow(flow: Flow): Promise<string[]> {
    return flowService.validateFlow(flow);
  },

  async startExecution(flowId: string, inputs: Record<string, unknown> = {}): Promise<string> {
    return flowService.startFlowExecution(flowId, inputs);
  },

  async cancelExecution(instanceId: string): Promise<boolean> {
    return flowService.cancelFlowExecution(instanceId);
  },

  async getExecution(instanceId: string): Promise<FlowState | null> {
    return flowService.getFlowExecution(instanceId);
  },

  async createSampleFlow(): Promise<Flow> {
    return flowService.createSampleFlow();
  },

  async addSampleFlow(): Promise<string> {
    return flowService.addSampleFlow();
  },
};
