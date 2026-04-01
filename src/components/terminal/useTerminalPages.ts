import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { instanceStorage } from "@/lib/instance-storage";

export interface TerminalPageConfig {
  id: string;
  name: string;
  createdAt: number;
}

const STORAGE_KEY = "qontinui-terminal-pages";
const ACTIVE_PAGE_KEY = "qontinui-terminal-active-page";

function loadPages(): TerminalPageConfig[] {
  const pages = instanceStorage.getJSON<TerminalPageConfig[]>(STORAGE_KEY, []);
  if (pages.length === 0) {
    return [{ id: "default", name: "Terminal", createdAt: 0 }];
  }
  return pages;
}

function savePages(pages: TerminalPageConfig[]) {
  instanceStorage.setJSON(STORAGE_KEY, pages);
}

export function useTerminalPages() {
  const [pages, setPages] = useState<TerminalPageConfig[]>(loadPages);
  const [activePageId, setActivePageIdState] = useState<string>(
    () => instanceStorage.getItem(ACTIVE_PAGE_KEY) || "default",
  );

  const setActivePageId = useCallback((id: string) => {
    setActivePageIdState(id);
    instanceStorage.setItem(ACTIVE_PAGE_KEY, id);
  }, []);

  const addPage = useCallback(
    (name: string) => {
      const id = crypto.randomUUID();
      const newPage: TerminalPageConfig = { id, name, createdAt: Date.now() };
      setPages((prev) => {
        const updated = [...prev, newPage];
        savePages(updated);
        return updated;
      });
      setActivePageId(id);
      return id;
    },
    [setActivePageId],
  );

  const removePage = useCallback(async (id: string) => {
    // Close all PTYs belonging to this page
    try {
      const result = await invoke<{
        success: boolean;
        data?: { terminals: Array<{ id: string; page_id: string }> };
      }>("terminal_list");
      if (result.success && result.data) {
        const pageTerminals = result.data.terminals.filter((t) => (t.page_id || "default") === id);
        for (const t of pageTerminals) {
          invoke("terminal_close", { terminalId: t.id }).catch(() => {});
        }
      }
    } catch {
      // Best effort
    }

    // Clean up namespaced instanceStorage keys
    if (id !== "default") {
      const prefix = `page:${id}:`;
      instanceStorage.removeByPrefix(prefix);
    }

    setPages((prev) => {
      if (prev.length <= 1) return prev; // Can't remove last page
      const updated = prev.filter((p) => p.id !== id);
      savePages(updated);
      return updated;
    });

    // Switch to another page if active was deleted
    setActivePageIdState((currentActive) => {
      if (currentActive === id) {
        // We need to find the first page that isn't the removed one
        // Use loadPages() to get the latest from storage after setPages runs
        const fallback = "default";
        instanceStorage.setItem(ACTIVE_PAGE_KEY, fallback);
        return fallback;
      }
      return currentActive;
    });
  }, []);

  const renamePage = useCallback((id: string, name: string) => {
    setPages((prev) => {
      const updated = prev.map((p) => (p.id === id ? { ...p, name } : p));
      savePages(updated);
      return updated;
    });
  }, []);

  return {
    pages,
    activePageId,
    setActivePageId,
    addPage,
    removePage,
    renamePage,
  };
}
