/**
 * SkillLibraryPicker.tsx
 *
 * Standalone modal dialog for browsing and selecting skills from the skill catalog.
 * Can be used independently from the AddStepDropdown for full-screen skill browsing.
 *
 * Uses the headless SkillCatalog from @qontinui/workflow-ui for state management
 * and renders a full modal overlay with search, category filtering, and skill cards.
 */

import React from "react";
import { Search, X, ChevronRight, Tag } from "lucide-react";
import type {
  SkillDefinition,
  SkillCategory,
  WorkflowPhase,
  UnifiedStep,
} from "@qontinui/shared-types/workflow";
import { getSkillCategoryIconData } from "@qontinui/workflow-utils";
import { SkillCatalog as HeadlessSkillCatalog } from "@qontinui/workflow-ui";
import { SkillParameterForm } from "./SkillParameterForm";

// =============================================================================
// Types
// =============================================================================

export interface SkillLibraryPickerProps {
  isOpen: boolean;
  onClose: () => void;
  phase: WorkflowPhase;
  onSelect: (skill: SkillDefinition) => void;
  /** Optional: if provided, skill will be instantiated and steps returned directly */
  onAddSteps?: (steps: UnifiedStep[], phase: WorkflowPhase) => void;
  /** Called after a skill is successfully instantiated (steps added). */
  onSkillUsed?: (skillId: string) => void;
  /** Icon resolver from the parent app (maps icon IDs to React components) */
  resolveIcon: (iconId: string) => React.ComponentType<{ className?: string }>;
}

// =============================================================================
// Category Labels
// =============================================================================

const CATEGORY_LABELS: Record<string, string> = {
  "code-quality": "Code Quality",
  testing: "Testing",
  monitoring: "Monitoring",
  "ai-task": "AI Task",
  deployment: "Deployment",
  composition: "Composition",
  custom: "Custom",
};

function getCategoryLabel(category: string): string {
  return CATEGORY_LABELS[category] ?? category;
}

// =============================================================================
// Component
// =============================================================================

export function SkillLibraryPicker({
  isOpen,
  onClose,
  phase,
  onSelect,
  onAddSteps,
  onSkillUsed,
  resolveIcon,
}: SkillLibraryPickerProps) {
  if (!isOpen) return null;

  const handleAddSteps = (steps: UnifiedStep[], targetPhase: WorkflowPhase) => {
    if (onAddSteps) {
      onAddSteps(steps, targetPhase);
    }
    onClose();
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <HeadlessSkillCatalog
        phase={phase}
        onAddSteps={handleAddSteps}
        onClose={onClose}
        onSkillUsed={onSkillUsed}
      >
        {(props) => (
          <div className="bg-zinc-900 border border-zinc-700 rounded-lg shadow-2xl w-[640px] max-w-[90vw] max-h-[80vh] flex flex-col">
            {/* Header */}
            <div className="flex items-center justify-between px-4 py-3 border-b border-zinc-700">
              <div>
                <h2 className="text-lg font-semibold text-zinc-100">
                  {props.mode === "browse" ? "Skill Catalog" : "Configure Skill"}
                </h2>
                <p className="text-sm text-zinc-500">
                  {props.mode === "browse"
                    ? `Browse skills for the ${phase} phase`
                    : (props.selectedSkill?.description ?? "Configure parameters")}
                </p>
              </div>
              <button
                onClick={onClose}
                className="p-1.5 hover:bg-zinc-800 rounded-md transition-colors text-zinc-400 hover:text-zinc-200"
              >
                <X className="w-5 h-5" />
              </button>
            </div>

            {props.mode === "browse" ? (
              <BrowseView
                {...props}
                phase={phase}
                resolveIcon={resolveIcon}
                onSelectDirect={onSelect}
              />
            ) : (
              <ConfigureView {...props} resolveIcon={resolveIcon} />
            )}
          </div>
        )}
      </HeadlessSkillCatalog>
    </div>
  );
}

// =============================================================================
// Browse View
// =============================================================================

interface BrowseViewProps {
  searchQuery: string;
  setSearchQuery: (q: string) => void;
  selectedCategory: SkillCategory | null;
  setSelectedCategory: (c: SkillCategory | null) => void;
  categories: SkillCategory[];
  filteredSkills: SkillDefinition[];
  onSelectSkill: (skill: SkillDefinition) => void;
  phase: WorkflowPhase;
  resolveIcon: (iconId: string) => React.ComponentType<{ className?: string }>;
  onSelectDirect: (skill: SkillDefinition) => void;
}

function BrowseView({
  searchQuery,
  setSearchQuery,
  selectedCategory,
  setSelectedCategory,
  categories,
  filteredSkills,
  onSelectSkill,
  resolveIcon,
}: BrowseViewProps) {
  // Group skills by category for display
  const groupedSkills = React.useMemo(() => {
    const groups: Record<string, SkillDefinition[]> = {};
    for (const skill of filteredSkills) {
      if (!groups[skill.category]) {
        groups[skill.category] = [];
      }
      groups[skill.category].push(skill);
    }
    return groups;
  }, [filteredSkills]);

  return (
    <>
      {/* Search and Filter */}
      <div className="px-4 py-3 border-b border-zinc-700/50 space-y-3">
        <div className="relative">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-zinc-500" />
          <input
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Search skills..."
            className="w-full bg-zinc-800 border border-zinc-700 rounded-md pl-9 pr-3 py-2 text-sm text-zinc-200 placeholder:text-zinc-500 focus:outline-hidden focus:ring-1 focus:ring-zinc-500"
            autoFocus
          />
        </div>

        {/* Category Chips */}
        <div className="flex gap-1.5 flex-wrap">
          <CategoryChip
            label="All"
            isActive={selectedCategory === null}
            onClick={() => setSelectedCategory(null)}
          />
          {categories.map((cat) => (
            <CategoryChip
              key={cat}
              label={getCategoryLabel(cat)}
              isActive={selectedCategory === cat}
              onClick={() => setSelectedCategory(selectedCategory === cat ? null : cat)}
            />
          ))}
        </div>
      </div>

      {/* Skill List */}
      <div className="flex-1 overflow-y-auto p-4">
        {filteredSkills.length === 0 ? (
          <div className="text-center py-8 text-zinc-500">No skills match your search.</div>
        ) : selectedCategory ? (
          // Flat list when a category is selected
          <div className="space-y-1">
            {filteredSkills.map((skill) => (
              <SkillRow
                key={skill.id}
                skill={skill}
                onClick={() => onSelectSkill(skill)}
                resolveIcon={resolveIcon}
              />
            ))}
          </div>
        ) : (
          // Grouped by category when showing all
          <div className="space-y-4">
            {Object.entries(groupedSkills).map(([category, skills]) => (
              <div key={category}>
                <h3 className="text-xs font-medium text-zinc-500 uppercase tracking-wider mb-2 px-1">
                  {getCategoryLabel(category)}
                </h3>
                <div className="space-y-1">
                  {skills.map((skill) => (
                    <SkillRow
                      key={skill.id}
                      skill={skill}
                      onClick={() => onSelectSkill(skill)}
                      resolveIcon={resolveIcon}
                    />
                  ))}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Footer */}
      <div className="flex items-center justify-between px-4 py-3 border-t border-zinc-700 bg-zinc-800/50">
        <span className="text-sm text-zinc-500">
          {filteredSkills.length} skill{filteredSkills.length !== 1 ? "s" : ""} available
        </span>
      </div>
    </>
  );
}

// =============================================================================
// Configure View
// =============================================================================

interface ConfigureViewProps {
  selectedSkill: SkillDefinition | null;
  paramValues: Record<string, unknown>;
  setParamValue: (name: string, value: unknown) => void;
  validationErrors: string[];
  onConfirm: () => void;
  onBack: () => void;
  resolveIcon: (iconId: string) => React.ComponentType<{ className?: string }>;
}

/**
 * Renders a resolved skill icon.
 * Extracted to module scope to avoid defining a component inside ConfigureView/SkillRow.
 */
function ResolvedSkillIcon({
  iconId,
  className,
  resolveIcon,
}: {
  iconId: string;
  className?: string;
  resolveIcon: (iconId: string) => React.ComponentType<{ className?: string }>;
}) {
  return React.createElement(resolveIcon(iconId), { className });
}

function ConfigureView({
  selectedSkill,
  paramValues,
  setParamValue,
  validationErrors,
  onConfirm,
  onBack,
  resolveIcon,
}: ConfigureViewProps) {
  if (!selectedSkill) return null;

  const iconData = getSkillCategoryIconData(selectedSkill.category);

  return (
    <>
      {/* Skill Header */}
      <div className="px-4 py-3 border-b border-zinc-700/50 flex items-center gap-3">
        <button
          className="p-1.5 rounded hover:bg-zinc-800 text-zinc-400 hover:text-zinc-200 transition-colors"
          onClick={onBack}
          title="Back to catalog"
        >
          <ChevronRight className="w-4 h-4 rotate-180" />
        </button>
        <div className={`p-2 rounded ${iconData.bgClass}`}>
          <ResolvedSkillIcon
            iconId={selectedSkill.icon}
            className={`w-5 h-5 ${iconData.textClass}`}
            resolveIcon={resolveIcon}
          />
        </div>
        <div className="flex-1 min-w-0">
          <h3 className="text-sm font-medium text-zinc-200">{selectedSkill.name}</h3>
          <p className="text-xs text-zinc-500">{selectedSkill.description}</p>
        </div>
        {selectedSkill.parameters.length > 0 && (
          <span className="text-xs text-zinc-500 bg-zinc-800 px-2 py-0.5 rounded">
            {selectedSkill.parameters.length} param
            {selectedSkill.parameters.length !== 1 ? "s" : ""}
          </span>
        )}
      </div>

      {/* Parameter Form */}
      <div className="flex-1 overflow-y-auto p-4">
        <SkillParameterForm
          skill={selectedSkill}
          values={paramValues}
          onChange={setParamValue}
          errors={validationErrors}
        />
      </div>

      {/* Footer */}
      <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-zinc-700 bg-zinc-800/50">
        <button
          className="px-4 py-2 text-sm text-zinc-400 hover:text-zinc-200 rounded-md hover:bg-zinc-800 transition-colors"
          onClick={onBack}
        >
          Cancel
        </button>
        <button
          className="px-4 py-2 text-sm bg-blue-600 hover:bg-blue-500 text-white rounded-md disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          onClick={onConfirm}
          disabled={validationErrors.length > 0}
        >
          Add to Phase
        </button>
      </div>
    </>
  );
}

// =============================================================================
// Sub-Components
// =============================================================================

function CategoryChip({
  label,
  isActive,
  onClick,
}: {
  label: string;
  isActive: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className={`
        px-2.5 py-1 text-xs rounded-full transition-colors
        ${
          isActive
            ? "bg-zinc-600 text-zinc-100"
            : "bg-zinc-800 text-zinc-400 hover:bg-zinc-700 hover:text-zinc-300"
        }
      `}
      onClick={onClick}
    >
      {label}
    </button>
  );
}

function SkillRow({
  skill,
  onClick,
  resolveIcon,
}: {
  skill: SkillDefinition;
  onClick: () => void;
  resolveIcon: (iconId: string) => React.ComponentType<{ className?: string }>;
}) {
  const iconData = getSkillCategoryIconData(skill.category);

  return (
    <button
      className="w-full flex items-center gap-3 px-3 py-2.5 rounded-lg hover:bg-zinc-800 transition-colors text-left group"
      onClick={onClick}
    >
      <div className={`shrink-0 p-1.5 rounded ${iconData.bgClass}`}>
        <ResolvedSkillIcon
          iconId={skill.icon}
          className={`w-4 h-4 ${iconData.textClass}`}
          resolveIcon={resolveIcon}
        />
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-sm text-zinc-200 group-hover:text-zinc-100 font-medium">
            {skill.name}
          </span>
          {skill.parameters.length > 0 && (
            <span className="text-[10px] text-zinc-500 bg-zinc-800 px-1.5 py-0.5 rounded">
              {skill.parameters.length} param{skill.parameters.length !== 1 ? "s" : ""}
            </span>
          )}
        </div>
        <p className="text-xs text-zinc-500 truncate">{skill.description}</p>
      </div>
      {skill.tags.length > 0 && (
        <div className="hidden sm:flex items-center gap-1 shrink-0">
          <Tag className="w-3 h-3 text-zinc-600" />
          <span className="text-[10px] text-zinc-600">{skill.tags.slice(0, 2).join(", ")}</span>
        </div>
      )}
      <ChevronRight className="w-4 h-4 text-zinc-600 group-hover:text-zinc-400 shrink-0" />
    </button>
  );
}
