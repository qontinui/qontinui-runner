/**
 * HelpTab.tsx
 *
 * Help tab component with sub-pages for:
 * - Shortcuts: Keyboard shortcuts and quick actions
 * - (Future: Documentation, FAQ, etc.)
 */

import { useState } from "react";
import * as Tabs from "@radix-ui/react-tabs";
import { Keyboard, HelpCircle, Command } from "lucide-react";

type HelpSubPage = "shortcuts";

interface ShortcutItem {
  keys: string[];
  description: string;
  context: string;
}

interface ShortcutCategory {
  name: string;
  shortcuts: ShortcutItem[];
}

const SHORTCUTS: ShortcutCategory[] = [
  {
    name: "Script Builder",
    shortcuts: [
      {
        keys: ["@"],
        description: "Open scriptlet selector popup",
        context:
          "In the Description field, type @ to search and insert a scriptlet at the cursor position",
      },
    ],
  },
  {
    name: "Scriptlet Selector",
    shortcuts: [
      {
        keys: ["\u2191", "\u2193"],
        description: "Navigate through scriptlet list",
        context: "When scriptlet selector popup is open",
      },
      {
        keys: ["Enter"],
        description: "Insert selected scriptlet",
        context: "When scriptlet selector popup is open",
      },
      {
        keys: ["Escape"],
        description: "Close scriptlet selector",
        context: "When scriptlet selector popup is open",
      },
    ],
  },
];

function ShortcutsPage() {
  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <div className="p-2 bg-primary/10 rounded-lg">
          <Keyboard className="w-5 h-5 text-primary" />
        </div>
        <div>
          <h2 className="text-lg font-semibold">Keyboard Shortcuts</h2>
          <p className="text-sm text-muted-foreground">
            Quick actions and keyboard shortcuts to speed up your workflow
          </p>
        </div>
      </div>

      <div className="space-y-6">
        {SHORTCUTS.map((category) => (
          <div key={category.name} className="space-y-3">
            <h3 className="text-sm font-medium text-muted-foreground uppercase tracking-wider">
              {category.name}
            </h3>
            <div className="bg-card border border-border rounded-lg divide-y divide-border">
              {category.shortcuts.map((shortcut, idx) => (
                <div key={idx} className="p-4 flex items-start gap-4">
                  <div className="flex items-center gap-1.5 flex-shrink-0">
                    {shortcut.keys.map((key, keyIdx) => (
                      <span key={keyIdx}>
                        <kbd className="px-2 py-1 text-sm font-mono bg-muted border border-border rounded shadow-sm">
                          {key}
                        </kbd>
                        {keyIdx < shortcut.keys.length - 1 && (
                          <span className="mx-1 text-muted-foreground">+</span>
                        )}
                      </span>
                    ))}
                  </div>
                  <div className="flex-1 min-w-0">
                    <div className="font-medium">{shortcut.description}</div>
                    <div className="text-sm text-muted-foreground mt-0.5">{shortcut.context}</div>
                  </div>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>

      {/* Tips section */}
      <div className="bg-blue-500/10 border border-blue-500/20 rounded-lg p-4">
        <div className="flex items-start gap-3">
          <HelpCircle className="w-5 h-5 text-blue-500 flex-shrink-0 mt-0.5" />
          <div>
            <h4 className="font-medium text-blue-500">Tip: Using Scriptlets</h4>
            <p className="text-sm text-muted-foreground mt-1">
              Scriptlets are reusable text snippets that capture learnings from AI debugging
              sessions. Create them in the <strong>Scriptlets</strong> tab, then insert them into
              script descriptions using the dropdown button or by typing{" "}
              <kbd className="px-1.5 py-0.5 text-xs bg-muted border border-border rounded">@</kbd>{" "}
              in the description field.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}

export function HelpTab() {
  const [activeSubPage, setActiveSubPage] = useState<HelpSubPage>("shortcuts");

  const subPages = [{ id: "shortcuts" as const, label: "Shortcuts", icon: Keyboard }];

  return (
    <div className="h-full p-4 flex flex-col overflow-hidden">
      <Tabs.Root
        value={activeSubPage}
        onValueChange={(value) => setActiveSubPage(value as HelpSubPage)}
        className="h-full flex flex-col min-h-0"
      >
        {/* Sub-page navigation - Fixed */}
        <Tabs.List className="flex gap-2 mb-4 flex-shrink-0">
          {subPages.map((page) => {
            const Icon = page.icon;
            return (
              <Tabs.Trigger
                key={page.id}
                value={page.id}
                className={`
                  flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg
                  transition-colors
                  data-[state=active]:bg-primary data-[state=active]:text-primary-foreground
                  data-[state=inactive]:bg-muted/50 data-[state=inactive]:text-muted-foreground
                  data-[state=inactive]:hover:bg-muted data-[state=inactive]:hover:text-foreground
                `}
              >
                <Icon className="w-4 h-4" />
                {page.label}
              </Tabs.Trigger>
            );
          })}
        </Tabs.List>

        {/* Sub-page content */}
        <div className="flex-1 min-h-0 overflow-y-auto">
          <Tabs.Content value="shortcuts" className="h-full outline-none">
            <ShortcutsPage />
          </Tabs.Content>
        </div>
      </Tabs.Root>
    </div>
  );
}
