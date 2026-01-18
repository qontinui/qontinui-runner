/**
 * ConfigurationLoadMenu Component
 *
 * Dropdown menu for loading configurations with two options:
 * - Load from File: Opens file dialog to load JSON configuration
 * - Load RAG: Opens modal to select and load a RAG project
 */

import { useState } from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { FileText, FolderOpen, Sparkles, ChevronDown } from "lucide-react";
import { getAccentColors } from "@/design-system";

export interface ConfigurationLoadMenuProps {
  onLoadFromFile: () => void;
  onLoadRAG: () => void;
}

export function ConfigurationLoadMenu({ onLoadFromFile, onLoadRAG }: ConfigurationLoadMenuProps) {
  const [open, setOpen] = useState(false);

  return (
    <DropdownMenu.Root open={open} onOpenChange={setOpen}>
      <DropdownMenu.Trigger asChild>
        <button
          data-tutorial-id="load-config-button"
          className={`w-full flex items-center justify-center gap-2 px-3 py-2 text-xs rounded-lg ${getAccentColors("blue").bg} ${getAccentColors("blue").text} hover:opacity-80 transition-colors`}
        >
          <FileText className="w-3.5 h-3.5" />
          Load Configuration
          <ChevronDown className="w-3.5 h-3.5 ml-auto" />
        </button>
      </DropdownMenu.Trigger>

      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className="min-w-[200px] bg-card/95 backdrop-blur rounded-lg shadow-xl p-1 animate-slideDown z-50"
          sideOffset={5}
          align="start"
        >
          {/* Load from File */}
          <DropdownMenu.Item
            className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-none hover:bg-muted/50 transition-colors"
            onSelect={() => {
              setOpen(false);
              onLoadFromFile();
            }}
          >
            <FolderOpen className={`w-3.5 h-3.5 ${getAccentColors("blue").text}`} />
            <span className="flex-1">Load from File</span>
          </DropdownMenu.Item>

          {/* Separator */}
          <DropdownMenu.Separator className="h-px bg-border/50 my-1" />

          {/* Load RAG */}
          <DropdownMenu.Item
            className="flex items-center gap-2 px-3 py-2 text-xs rounded-md cursor-pointer outline-none hover:bg-muted/50 transition-colors"
            onSelect={() => {
              setOpen(false);
              onLoadRAG();
            }}
          >
            <Sparkles className={`w-3.5 h-3.5 ${getAccentColors("purple").text}`} />
            <span className="flex-1">Load RAG Project</span>
          </DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
