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
      const updated = [...pages, newPage];
      setPages(updated);
      savePages(updated);
      setActivePageId(id);
      return id;
    },
    [pages, setActivePageId],
  );

  const removePage = useCallback(
    async (id: string) => {
      if (pages.length <= 1) return; // Can't remove last page
      if (id === "default" && pages.length === 1) return;

      // Close all PTYs belonging to this page
      try {
        const result = await invoke<{
          success: boolean;
          data?: { terminals: Array<{ id: string; page_id: string }> };
        }>("terminal_list");
        if (result.success && result.data) {
          const pageTerminals = result.data.terminals.filter(
            (t) => (t.page_id || "default") === id,
          );
          for (const t of pageTerminals) {
            invoke("terminal_close", { terminalId: t.id }).catch(() => {});
          }
        }
      } catch {
        // Best effort
      }

      // Clean up namespaced localStorage keys
      if (id !== "default") {
        const prefix = `page:${id}:`;
        const keysToRemove: string[] = [];
        for (let i = 0; i < localStorage.length; i++) {
          const key = localStorage.key(i);
          if (key && key.includes(prefix)) {
            keysToRemove.push(key);
          }
        }
        for (const key of keysToRemove) {
          localStorage.removeItem(key);
        }
      }

      const updated = pages.filter((p) => p.id !== id);
      setPages(updated);
      savePages(updated);

      // Switch to another page if active was deleted
      if (activePageId === id) {
        setActivePageId(updated[0].id);
      }
    },
    [pages, activePageId, setActivePageId],
  );

  const renamePage = useCallback(
    (id: string, name: string) => {
      const updated = pages.map((p) => (p.id === id ? { ...p, name } : p));
      setPages(updated);
      savePages(updated);
    },
    [pages],
  );

  return {
    pages,
    activePageId,
    setActivePageId,
    addPage,
    removePage,
    renamePage,
  };
}
