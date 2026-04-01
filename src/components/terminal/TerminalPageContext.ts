import { createContext, useContext } from "react";

const TerminalPageContext = createContext<string>("default");

export const TerminalPageProvider = TerminalPageContext.Provider;

export function useTerminalPageId(): string {
  return useContext(TerminalPageContext);
}

/**
 * Namespace a storage key by terminal page ID.
 * The "default" page uses un-prefixed keys for backward compatibility.
 */
export function pageKey(pageId: string, key: string): string {
  return pageId === "default" ? key : `page:${pageId}:${key}`;
}
