import type { RefObject } from "react";
import {
  TerminalInstance,
  type TerminalInstanceHandle,
  type ShellIntegrationEvent,
} from "../TerminalInstance";
import type { TerminalTab } from "../useTerminalManager";

export function HiddenTerminal({
  tab,
  terminalRef,
  onExit,
  onFirstInput,
  onUserInputLine,
  onShellIntegration,
  onReconnected,
  onTitleChange,
}: {
  tab: TerminalTab;
  terminalRef: RefObject<TerminalInstanceHandle | null> | undefined;
  onExit: (terminalId: string, exitCode: number | null) => void;
  onFirstInput: (terminalId: string, input: string) => void;
  onUserInputLine?: (terminalId: string, input: string) => void;
  onShellIntegration: (tabId: string, event: ShellIntegrationEvent) => void;
  onReconnected: (tabId: string) => void;
  onTitleChange?: (tabId: string, title: string) => void;
}) {
  return (
    <div className="hidden">
      <TerminalInstance
        ref={terminalRef}
        terminalId={tab.id}
        visible={false}
        isReconnecting={tab.isReconnecting}
        onReconnected={() => onReconnected(tab.id)}
        onExit={(code) => onExit(tab.id, code)}
        onFirstInput={(input) => onFirstInput(tab.id, input)}
        onUserInputLine={
          onUserInputLine ? (input) => onUserInputLine(tab.id, input) : undefined
        }
        onShellIntegration={(event) => onShellIntegration(tab.id, event)}
        onTitleChange={onTitleChange ? (title) => onTitleChange(tab.id, title) : undefined}
      />
    </div>
  );
}
