/**
 * AddStepDropdown.tsx
 *
 * Dropdown menu for adding steps to a workflow via the Skill Catalog.
 * Browse skills by category, search, and configure parameters.
 */

import React, { useRef, useEffect, useState, useCallback } from "react";
import {
  Plus,
  ChevronDown,
  Terminal,
  MessageSquare,
  TestTube2,
  Monitor,
  AlertTriangle,
  Workflow,
  CheckCircle2,
  ShieldCheck,
  Bot,
  Globe,
  Compass,
  Activity,
  ScanSearch,
  AlignLeft,
  FileType,
  CheckSquare,
  HeartPulse,
  Camera,
  Rocket,
  Puzzle,
  GitBranch,
  Pointer,
  GitCompareArrows,
} from "lucide-react";
import type { WorkflowPhase, UnifiedStep } from "../../types";
import { SkillCatalogConcrete } from "@qontinui/workflow-ui/components";
import { SkillLibraryPicker } from "./SkillLibraryPicker";

// =============================================================================
// Icon Resolver (shared with skill catalog)
// =============================================================================

const ICON_MAP: Record<string, React.ComponentType<{ className?: string }>> = {
  terminal: Terminal,
  "message-square": MessageSquare,
  "test-tube-2": TestTube2,
  monitor: Monitor,
  "alert-triangle": AlertTriangle,
  "check-circle": CheckCircle2,
  workflow: Workflow,
  activity: Activity,
  bot: Bot,
  globe: Globe,
  "scan-search": ScanSearch,
  "align-left": AlignLeft,
  "file-type": FileType,
  "check-square": CheckSquare,
  "heart-pulse": HeartPulse,
  camera: Camera,
  rocket: Rocket,
  puzzle: Puzzle,
  "git-branch": GitBranch,
  "shield-check": ShieldCheck,
  pointer: Pointer,
  "git-compare-arrows": GitCompareArrows,
  compass: Compass,
};

function resolveIcon(iconId: string): React.ComponentType<{ className?: string }> {
  return ICON_MAP[iconId] ?? Activity;
}

// =============================================================================
// Component
// =============================================================================

interface AddStepDropdownProps {
  filterPhase?: WorkflowPhase;
  onAddStep: (step: UnifiedStep, phase: WorkflowPhase) => void;
  isOpen: boolean;
  onClose: () => void;
  /** Called after a skill is successfully instantiated (steps added). */
  onSkillUsed?: (skillId: string) => void;
}

export function AddStepDropdown({
  filterPhase,
  onAddStep,
  isOpen,
  onClose,
  onSkillUsed,
}: AddStepDropdownProps) {
  const dropdownRef = useRef<HTMLDivElement>(null);
  const [showLibrary, setShowLibrary] = useState(false);

  const phase = filterPhase || "setup";

  // Close dropdown when clicking outside
  useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        onClose();
      }
    }

    if (isOpen) {
      document.addEventListener("mousedown", handleClickOutside);
      return () => document.removeEventListener("mousedown", handleClickOutside);
    }
  }, [isOpen, onClose]);

  // Reset state when dropdown closes
  useEffect(() => {
    if (!isOpen) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- reset side-effect on close
      setShowLibrary(false);
    }
  }, [isOpen]);

  // Handle skill catalog adding steps
  const handleAddSteps = useCallback(
    (steps: UnifiedStep[], targetPhase: WorkflowPhase) => {
      for (const step of steps) {
        onAddStep(step, targetPhase);
      }
    },
    [onAddStep],
  );

  if (!isOpen) return null;

  return (
    <>
      <div
        ref={dropdownRef}
        className="absolute z-50 w-80 rounded-lg border border-zinc-700 bg-zinc-800 shadow-xl overflow-hidden"
      >
        <div className="py-1">
          <div className="px-3 py-2 text-xs font-medium text-zinc-500 uppercase tracking-wider border-b border-zinc-700">
            Add Step
          </div>

          <SkillCatalogConcrete
            phase={phase}
            isOpen={true}
            onAddSteps={handleAddSteps}
            onClose={onClose}
            onSkillUsed={onSkillUsed}
            resolveIcon={resolveIcon}
          />
          <div className="px-3 py-2 border-t border-zinc-700/50">
            <button
              className="w-full flex items-center justify-center gap-1.5 px-2 py-1.5 text-xs text-zinc-400 hover:text-zinc-200 hover:bg-zinc-700/50 rounded-md transition-colors"
              onClick={() => setShowLibrary(true)}
            >
              <Compass className="w-3.5 h-3.5" />
              Browse Full Library
            </button>
          </div>
        </div>
      </div>
      <SkillLibraryPicker
        isOpen={showLibrary}
        onClose={() => setShowLibrary(false)}
        phase={phase}
        onSelect={() => {}}
        onAddSteps={handleAddSteps}
        onSkillUsed={onSkillUsed}
        resolveIcon={resolveIcon}
      />
    </>
  );
}

// =============================================================================
// Button Component
// =============================================================================

interface AddStepButtonProps {
  onClick: () => void;
  variant?: "default" | "compact" | "phase";
  phaseLabel?: string;
}

export function AddStepButton({ onClick, variant = "default", phaseLabel }: AddStepButtonProps) {
  if (variant === "compact") {
    return (
      <button
        onClick={onClick}
        className="flex items-center gap-1 px-2 py-1 text-sm rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-300 transition-colors"
      >
        <Plus className="w-4 h-4" />
        <span>Add</span>
      </button>
    );
  }

  if (variant === "phase" && phaseLabel) {
    return (
      <button
        onClick={onClick}
        className="w-full flex items-center justify-center gap-2 p-2 rounded-md bg-zinc-700/50 hover:bg-zinc-700 text-zinc-400 hover:text-zinc-200 transition-colors text-sm"
      >
        <Plus className="w-4 h-4" />
        <span>Add {phaseLabel} Step</span>
      </button>
    );
  }

  return (
    <button
      onClick={onClick}
      className="flex items-center gap-2 px-4 py-2 rounded-md bg-zinc-700 hover:bg-zinc-600 text-zinc-200 transition-colors"
    >
      <Plus className="w-4 h-4" />
      <span>Add Step</span>
      <ChevronDown className="w-4 h-4" />
    </button>
  );
}
