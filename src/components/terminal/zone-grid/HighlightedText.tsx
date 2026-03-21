export function HighlightedText({ text, query }: { text: string; query?: string }) {
  if (!query || !text) return <>{text}</>;
  const lower = text.toLowerCase();
  const qLower = query.toLowerCase();
  const idx = lower.indexOf(qLower);
  if (idx === -1) return <>{text}</>;
  return (
    <>
      {text.slice(0, idx)}
      <span className="bg-[#e0af68]/30 text-[#e0af68] rounded-xs px-0.5">
        {text.slice(idx, idx + query.length)}
      </span>
      {text.slice(idx + query.length)}
    </>
  );
}
