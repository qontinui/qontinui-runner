/**
 * Runner Data Adapter
 *
 * Implements WorkflowDataAdapter from @qontinui/workflow-ui
 * using the runner's local HTTP API (via getApiBase()).
 */

import type { WorkflowDataAdapter, PermittedTrigger, BlockedTrigger } from "@qontinui/workflow-ui";
import type { UnifiedWorkflow, SkillDefinition } from "@qontinui/shared-types/workflow";
import type { LibraryItem } from "@qontinui/shared-types/library";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

export function createRunnerDataAdapter(): WorkflowDataAdapter {
  return {
    async fetchPrompts(): Promise<LibraryItem[]> {
      try {
        const res = await tracedFetch(`${getApiBase()}/prompts`);
        if (!res.ok) return [];
        const data = await res.json();
        const prompts = data.prompts ?? data ?? [];
        return prompts.map((p: Record<string, unknown>) => ({
          id: (p.id as string) ?? (p.name as string),
          name: p.name as string,
          description: p.description as string | undefined,
        }));
      } catch {
        return [];
      }
    },

    async fetchChecks(): Promise<LibraryItem[]> {
      try {
        const res = await tracedFetch(`${getApiBase()}/checks`);
        if (!res.ok) return [];
        const data = await res.json();
        const checks = data.checks ?? data ?? [];
        return checks.map((c: Record<string, unknown>) => ({
          id: (c.id as string) ?? (c.name as string),
          name: c.name as string,
          description: c.description as string | undefined,
        }));
      } catch {
        return [];
      }
    },

    async fetchCheckGroups(): Promise<LibraryItem[]> {
      try {
        const res = await tracedFetch(`${getApiBase()}/check-groups`);
        if (!res.ok) return [];
        const data = await res.json();
        const groups = data.check_groups ?? data ?? [];
        return groups.map((g: Record<string, unknown>) => ({
          id: (g.id as string) ?? (g.name as string),
          name: g.name as string,
          description: g.description as string | undefined,
        }));
      } catch {
        return [];
      }
    },

    async fetchShellCommands(): Promise<LibraryItem[]> {
      try {
        const res = await tracedFetch(`${getApiBase()}/shell-commands`);
        if (!res.ok) return [];
        const data = await res.json();
        const commands = data.shell_commands ?? data ?? [];
        return commands.map((c: Record<string, unknown>) => ({
          id: (c.id as string) ?? (c.name as string),
          name: c.name as string,
          description: c.description as string | undefined,
        }));
      } catch {
        return [];
      }
    },

    async fetchWorkflows(): Promise<UnifiedWorkflow[]> {
      try {
        const res = await tracedFetch(`${getApiBase()}/unified-workflows`);
        if (!res.ok) return [];
        const data = await res.json();
        if (data.success && data.data) {
          return data.data as UnifiedWorkflow[];
        }
        return data.workflows ?? data ?? [];
      } catch {
        return [];
      }
    },

    async fetchPlaywrightScripts(): Promise<LibraryItem[]> {
      try {
        const res = await tracedFetch(`${getApiBase()}/playwright-scripts`);
        if (!res.ok) return [];
        const data = await res.json();
        const scripts = data.scripts ?? data ?? [];
        return scripts.map((s: Record<string, unknown>) => ({
          id: (s.id as string) ?? (s.name as string),
          name: s.name as string,
          description: s.description as string | undefined,
        }));
      } catch {
        return [];
      }
    },

    async fetchSkills(): Promise<SkillDefinition[]> {
      try {
        const res = await tracedFetch(`${getApiBase()}/skills`);
        if (!res.ok) return [];
        const data = await res.json();
        // Runner API wraps in { success: true, data: [...] }
        const skills = data.data ?? data ?? [];
        return skills.filter((s: SkillDefinition) => s.source !== "builtin");
      } catch {
        return [];
      }
    },

    async fetchContexts(): Promise<LibraryItem[]> {
      try {
        const res = await tracedFetch(`${getApiBase()}/contexts`);
        if (!res.ok) return [];
        const data = await res.json();
        const contexts = data.contexts ?? data ?? [];
        return contexts.map((c: Record<string, unknown>) => ({
          id: (c.id as string) ?? (c.name as string),
          name: c.name as string,
          description: c.description as string | undefined,
        }));
      } catch {
        return [];
      }
    },

    async saveWorkflow(workflow: UnifiedWorkflow): Promise<UnifiedWorkflow> {
      const res = await tracedFetch(`${getApiBase()}/unified-workflows`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(workflow),
      });
      if (!res.ok) throw new Error("Failed to save workflow");
      const data = await res.json();
      if (data.success && data.data) return data.data as UnifiedWorkflow;
      return data as UnifiedWorkflow;
    },

    async loadWorkflow(id: string): Promise<UnifiedWorkflow> {
      const res = await tracedFetch(`${getApiBase()}/unified-workflows/${id}`);
      if (!res.ok) throw new Error("Failed to load workflow");
      const data = await res.json();
      if (data.success && data.data) return data.data as UnifiedWorkflow;
      return data as UnifiedWorkflow;
    },

    async deleteWorkflow(id: string): Promise<void> {
      const res = await tracedFetch(`${getApiBase()}/unified-workflows/${id}`, {
        method: "DELETE",
      });
      if (!res.ok) throw new Error("Failed to delete workflow");
    },

    async listWorkflows(): Promise<UnifiedWorkflow[]> {
      try {
        const res = await tracedFetch(`${getApiBase()}/unified-workflows`);
        if (!res.ok) return [];
        const data = await res.json();
        if (data.success && data.data) {
          return data.data as UnifiedWorkflow[];
        }
        return data.workflows ?? data ?? [];
      } catch {
        return [];
      }
    },

    async getPermittedTriggers(activeStateIds: string[]): Promise<PermittedTrigger[]> {
      try {
        const qs = activeStateIds.length
          ? `?active_state_ids=${encodeURIComponent(activeStateIds.join(","))}`
          : "";
        const res = await tracedFetch(`${getApiBase()}/state-machine/permitted-triggers${qs}`);
        if (!res.ok) return [];
        const data = await res.json();
        // ApiResponse envelope: { success, data: { permitted_triggers: [...] } }
        const inner = data.data ?? data;
        const list = inner?.permitted_triggers ?? [];
        return Array.isArray(list) ? (list as PermittedTrigger[]) : [];
      } catch {
        return [];
      }
    },

    async getBlockedTriggers(activeStateIds: string[]): Promise<BlockedTrigger[]> {
      try {
        const qs = activeStateIds.length
          ? `?active_state_ids=${encodeURIComponent(activeStateIds.join(","))}`
          : "";
        const res = await tracedFetch(`${getApiBase()}/state-machine/blocked-triggers${qs}`);
        if (!res.ok) return [];
        const data = await res.json();
        const inner = data.data ?? data;
        const list = inner?.blocked_triggers ?? [];
        return Array.isArray(list) ? (list as BlockedTrigger[]) : [];
      } catch {
        return [];
      }
    },

    async getMermaidDiagram(activeStateIds?: string[]): Promise<string> {
      try {
        const ids = activeStateIds ?? [];
        const qs = ids.length ? `?active_state_ids=${encodeURIComponent(ids.join(","))}` : "";
        const res = await tracedFetch(`${getApiBase()}/state-machine/mermaid-diagram${qs}`);
        if (!res.ok) return "";
        const data = await res.json();
        // ApiResponse envelope: { success, data: { diagram: "..." } }
        const inner = data.data ?? data;
        const diagram = inner?.diagram;
        return typeof diagram === "string" ? diagram : "";
      } catch {
        return "";
      }
    },
  };
}
