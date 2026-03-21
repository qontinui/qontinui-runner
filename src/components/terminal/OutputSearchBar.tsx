interface OutputSearchBarProps {
  outputSearch: string;
  onSearchChange: (value: string) => void;
  onClose: () => void;
  lastOutputLines: Record<string, string[]>;
}

export function OutputSearchBar({
  outputSearch,
  onSearchChange,
  onClose,
  lastOutputLines,
}: OutputSearchBarProps) {
  return (
    <div className="flex items-center gap-2 px-3 h-8 bg-[#13141f] border-b border-[#2a2d3d] shrink-0">
      <span className="text-[10px] text-[#565f89] shrink-0">Search:</span>
      <input
        autoFocus
        value={outputSearch}
        onChange={(e) => onSearchChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            onClose();
          }
          e.stopPropagation();
        }}
        placeholder="Search across all session output..."
        className="flex-1 bg-[#1a1b26] border border-[#2a2d3d] rounded px-2 py-0.5 text-xs text-[#c0caf5] placeholder-[#565f89] outline-hidden focus:border-[#7aa2f7] transition-colors"
      />
      {outputSearch &&
        (() => {
          const query = outputSearch.toLowerCase();
          const matchCount = Object.entries(lastOutputLines).filter(([, lines]) =>
            lines.some((l) => l.toLowerCase().includes(query)),
          ).length;
          return (
            <span
              className={`text-[10px] shrink-0 ${matchCount > 0 ? "text-[#9ece6a]" : "text-[#565f89]"}`}
            >
              {matchCount} match{matchCount !== 1 ? "es" : ""}
            </span>
          );
        })()}
      <button
        onClick={onClose}
        className="p-1 rounded text-[#565f89] hover:text-[#a9b1d6] hover:bg-[#2a2d3d] transition-colors shrink-0"
      >
        <span className="text-xs">&#x2715;</span>
      </button>
    </div>
  );
}
