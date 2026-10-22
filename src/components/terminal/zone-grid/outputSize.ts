/**
 * The zone header's output figure: the TOTAL bytes the session's PTY has
 * produced, labelled as such. It replaced `{n} lines`, which counted the
 * capped tail preview (`lastOutputLines`) and so read `20 lines` on every busy
 * pane however much it had written.
 *
 * `null` when no total has been observed (`undefined`) or nothing has been
 * written yet, so the header shows no figure rather than an invented one.
 */
export function formatTotalOutput(totalBytes: number | undefined): string | null {
  if (totalBytes === undefined || !Number.isFinite(totalBytes) || totalBytes <= 0) return null;
  if (totalBytes < 1024) return `${totalBytes} B output`;
  const kb = Math.round(totalBytes / 1024);
  // Rounding up to 1024 KB is a megabyte: switch units there, not at the byte.
  if (kb < 1024) return `${kb} KB output`;
  return `${(totalBytes / (1024 * 1024)).toFixed(1)} MB output`;
}
