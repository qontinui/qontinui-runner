/**
 * AddStepDropdown Component
 *
 * Dropdown menu to add new steps to the workflow.
 */

import { useState, useRef, useEffect } from "react";
import {
  Plus,
  MousePointer2,
  MousePointerClick,
  Keyboard,
  Command,
  Navigation,
} from "lucide-react";
import type { GuiActionType } from "../../types/gui-workflow";
import { getActionTypeLabel } from "../../types/gui-workflow";

interface AddStepDropdownProps {
  onAddStep: (actionType: GuiActionType) => void;
  disabled?: boolean;
}

const actionTypes: {
  type: GuiActionType;
  icon: React.ReactNode;
  description: string;
}[] = [
  {
    type: "click",
    icon: <MousePointer2 className="h-4 w-4" />,
    description: "Click on a target image",
  },
  {
    type: "double_click",
    icon: <MousePointerClick className="h-4 w-4" />,
    description: "Double-click on a target image",
  },
  {
    type: "right_click",
    icon: <MousePointer2 className="h-4 w-4" />,
    description: "Right-click on a target image",
  },
  {
    type: "type",
    icon: <Keyboard className="h-4 w-4" />,
    description: "Type text input",
  },
  {
    type: "hotkey",
    icon: <Command className="h-4 w-4" />,
    description: "Press a keyboard shortcut",
  },
  {
    type: "go_to_state",
    icon: <Navigation className="h-4 w-4" />,
    description: "Navigate to an application state",
  },
];

export function AddStepDropdown({ onAddStep, disabled }: AddStepDropdownProps) {
  const [isOpen, setIsOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Close dropdown when clicking outside
  useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        setIsOpen(false);
      }
    }

    if (isOpen) {
      document.addEventListener("mousedown", handleClickOutside);
      return () => document.removeEventListener("mousedown", handleClickOutside);
    }
  }, [isOpen]);

  const handleSelect = (type: GuiActionType) => {
    onAddStep(type);
    setIsOpen(false);
  };

  return (
    <div className="relative" ref={dropdownRef}>
      <button
        onClick={() => !disabled && setIsOpen(!isOpen)}
        disabled={disabled}
        className="w-full flex items-center justify-center gap-2 px-4 py-2 border border-dashed border-border rounded-lg text-sm font-medium text-muted-foreground hover:text-foreground hover:border-primary/50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
      >
        <Plus className="h-4 w-4" />
        Add Step
      </button>

      {isOpen && (
        <div className="absolute z-50 mt-2 w-64 bg-card border border-border rounded-lg shadow-lg overflow-hidden">
          <div className="px-3 py-2 border-b border-border">
            <span className="text-xs font-medium text-muted-foreground">Action Type</span>
          </div>
          <div className="py-1">
            {actionTypes.map(({ type, icon, description }) => (
              <button
                key={type}
                onClick={() => handleSelect(type)}
                className="w-full flex items-start gap-3 px-3 py-2.5 text-left hover:bg-muted transition-colors"
              >
                <div className="flex-shrink-0 mt-0.5 text-primary">{icon}</div>
                <div className="flex-1 min-w-0">
                  <div className="font-medium text-sm">{getActionTypeLabel(type)}</div>
                  <div className="text-xs text-muted-foreground">{description}</div>
                </div>
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
