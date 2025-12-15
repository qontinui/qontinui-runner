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

export interface ConfigurationLoadMenuProps {
  onLoadFromFile: () => void;
  onLoadRAG: () => void;
}

export function ConfigurationLoadMenu({ onLoadFromFile, onLoadRAG }: ConfigurationLoadMenuProps) {
  const [open, setOpen] = useState(false);

  return (
    <DropdownMenu.Root open={open} onOpenChange={setOpen}>
      <DropdownMenu.Trigger asChild>
        <button className="w-full btn-primary flex items-center justify-center gap-2">
          <FileText className="w-4 h-4" />
          Load Configuration
          <ChevronDown className="w-4 h-4 ml-auto" />
        </button>
      </DropdownMenu.Trigger>

      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className="min-w-[220px] bg-card border border-border rounded-lg shadow-xl p-1 animate-slideDown z-50"
          sideOffset={5}
          align="start"
        >
          {/* Load from File */}
          <DropdownMenu.Item
            className="flex items-center gap-3 px-3 py-2 text-sm rounded-md cursor-pointer outline-none hover:bg-accent/20 hover:text-foreground transition-colors"
            onSelect={() => {
              setOpen(false);
              onLoadFromFile();
            }}
          >
            <FolderOpen className="w-4 h-4 text-primary" />
            <span className="flex-1">Load from File</span>
          </DropdownMenu.Item>

          {/* Separator */}
          <DropdownMenu.Separator className="h-px bg-border my-1" />

          {/* Load RAG */}
          <DropdownMenu.Item
            className="flex items-center gap-3 px-3 py-2 text-sm rounded-md cursor-pointer outline-none hover:bg-accent/20 hover:text-foreground transition-colors"
            onSelect={() => {
              setOpen(false);
              onLoadRAG();
            }}
          >
            <Sparkles className="w-4 h-4 text-primary" />
            <span className="flex-1">Load RAG</span>
          </DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
