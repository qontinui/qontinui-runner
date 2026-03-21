export function ActivitySparkline({ data }: { data: number[] }) {
  if (data.length < 2) return null;
  const max = Math.max(...data, 1);
  const w = 48;
  const h = 12;
  const points = data
    .map((v, i) => {
      const x = (i / (data.length - 1)) * w;
      const y = h - (v / max) * (h - 1);
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return (
    <svg width={w} height={h} className="shrink-0 opacity-60">
      <polyline
        points={points}
        fill="none"
        stroke="#7aa2f7"
        strokeWidth="1"
        strokeLinejoin="round"
      />
    </svg>
  );
}
