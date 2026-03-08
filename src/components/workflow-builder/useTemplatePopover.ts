/**
 * useTemplatePopover.ts
 *
 * Hook for managing template popover visibility, save state,
 * and click-away dismissal for the AiGeneratePanel.
 */

import { useState, useEffect, useRef } from "react";

export function useTemplatePopover() {
  const [showTemplates, setShowTemplates] = useState(false);
  const [isSavingTemplate, setIsSavingTemplate] = useState(false);
  const templatePopoverRef = useRef<HTMLDivElement>(null);

  // Click-away listener for template popover
  useEffect(() => {
    if (!showTemplates) return;
    const handleClickOutside = (e: MouseEvent) => {
      if (templatePopoverRef.current && !templatePopoverRef.current.contains(e.target as Node)) {
        setShowTemplates(false);
      }
    };
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, [showTemplates]);

  return {
    showTemplates,
    setShowTemplates,
    isSavingTemplate,
    setIsSavingTemplate,
    templatePopoverRef,
  };
}
