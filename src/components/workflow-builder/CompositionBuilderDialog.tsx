/**
 * CompositionBuilderDialog.tsx
 *
 * Modal wrapper for the CompositionSkillBuilder component.
 */

import React from "react";
import { CompositionSkillBuilder } from "@qontinui/workflow-ui/components";
import { registerUserSkills } from "@qontinui/workflow-utils";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

interface CompositionBuilderDialogProps {
  isOpen: boolean;
  onClose: () => void;
  resolveIcon: (iconId: string) => React.ComponentType<{ className?: string }>;
}

export function CompositionBuilderDialog({
  isOpen,
  onClose,
  resolveIcon,
}: CompositionBuilderDialogProps) {
  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl w-[520px]">
        <CompositionSkillBuilder
          resolveIcon={resolveIcon}
          onSave={async (refs) => {
            const name = prompt("Composition skill name:");
            if (!name) return;
            const slug = name
              .toLowerCase()
              .replace(/[^a-z0-9]+/g, "-")
              .replace(/^-|-$/g, "");
            try {
              await tracedFetch(`${getApiBase()}/skills`, {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({
                  id: `user:${slug}`,
                  name,
                  slug,
                  description: `Composition of ${refs.length} skills`,
                  category: "composition",
                  tags: ["composition"],
                  icon: "layers",
                  color: "blue",
                  allowed_phases: ["setup", "verification", "completion"],
                  parameters: [],
                  template: { kind: "composition", skill_refs: refs },
                }),
              });
              const skillsRes = await tracedFetch(`${getApiBase()}/skills`);
              const skillsData = await skillsRes.json();
              const allSkills = skillsData.data ?? skillsData ?? [];
              const nonBuiltin = allSkills.filter(
                (s: { source: string }) => s.source !== "builtin",
              );
              registerUserSkills(nonBuiltin);
            } catch (error) {
              console.error("Failed to create composition skill:", error);
            }
            onClose();
          }}
          onCancel={onClose}
        />
      </div>
    </div>
  );
}
