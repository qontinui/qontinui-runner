/**
 * The zone header's output figure: the TOTAL bytes the session's PTY has
 * produced, labelled as such. It replaced `{n} lines`, which counted the
 * capped tail preview (`lastOutputLines`) and so read `20 lines` on every busy
 * pane however much it had written.
 *
 * Decimal units (1 KB = 1000 bytes), so the figure agrees with the byte count
 * `GET /terminals` reports as `totalBytesProduced` under the unit it is named.
 *
 * `null` when no total has been observed (`undefined`) or nothing has been
 * written yet, so the header shows no figure rather than an invented one.
 */
export function formatTotalOutput(totalBytes: number | undefined): string | null {
  if (totalBytes === undefined || !Number.isFinite(totalBytes) || totalBytes <= 0) return null;
  if (totalBytes < 1000) return `${totalBytes} B output`;
  const kb = Math.round(totalBytes / 1000);
  // Rounding up to 1000 KB is a megabyte: switch units there, not at the byte.
  if (kb < 1000) return `${kb} KB output`;
  return `${(totalBytes / 1_000_000).toFixed(1)} MB output`;
}
