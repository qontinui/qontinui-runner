import { Wand2, FileText } from "lucide-react";

interface TerminalActionBarProps {
  hasSelection: boolean;
  onGenerateFromSelection: () => void;
  onImportTranscript: () => void;
  isGenerating: boolean;
}

export function TerminalActionBar({
  hasSelection,
  onGenerateFromSelection,
  onImportTranscript,
  isGenerating,
}: TerminalActionBarProps) {
  return (
    <div className="flex items-center gap-2 bg-[#13141f] border-b border-[#2a2d3d] px-3 h-8 shrink-0">
      <button
        onClick={onGenerateFromSelection}
        disabled={!hasSelection || isGenerating}
        className={`
          flex items-center gap-1.5 px-2 py-0.5 rounded text-xs font-medium transition-colors
          ${
            hasSelection && !isGenerating
              ? "text-[#7aa2f7] hover:bg-[#7aa2f7]/10 hover:text-[#89b4fa]"
              : "text-[#414868] cursor-not-allowed"
          }
        `}
        title={
          hasSelection
            ? "Generate a workflow from selected terminal text"
            : "Select text in the terminal first"
        }
      >
        <Wand2 className="w-3 h-3" />
        {isGenerating ? "Generating..." : "Generate from Selection"}
      </button>

      <div className="w-px h-4 bg-[#2a2d3d]" />

      <button
        onClick={onImportTranscript}
        disabled={isGenerating}
        className={`
          flex items-center gap-1.5 px-2 py-0.5 rounded text-xs font-medium transition-colors
          ${
            !isGenerating
              ? "text-[#9ece6a] hover:bg-[#9ece6a]/10 hover:text-[#a6da7a]"
              : "text-[#414868] cursor-not-allowed"
          }
        `}
        title="Import from a Claude Code transcript session"
      >
        <FileText className="w-3 h-3" />
        Import from Transcript
      </button>

      {isGenerating && (
        <>
          <div className="w-px h-4 bg-[#2a2d3d]" />
          <div className="flex items-center gap-1.5 text-xs text-[#e0af68]">
            <div className="w-3 h-3 border-2 border-[#e0af68] border-t-transparent rounded-full animate-spin" />
            Generating workflow...
          </div>
        </>
      )}
    </div>
  );
}
