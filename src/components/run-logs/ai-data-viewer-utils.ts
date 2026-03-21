export function formatTimestamp(ts: string): string {
  try {
    return new Date(ts).toLocaleString();
  } catch {
    return ts;
  }
}

export function formatDurationMs(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.floor((ms % 60_000) / 1000);
  return `${minutes}m ${seconds}s`;
}

export function formatSessionDuration(startedAt: string, stoppedAt: string | null): string {
  if (!stoppedAt) return "running";
  try {
    const ms = new Date(stoppedAt).getTime() - new Date(startedAt).getTime();
    if (ms < 0) return "\u2014";
    if (ms < 1000) return `${ms}ms`;
    if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
    const minutes = Math.floor(ms / 60_000);
    const seconds = Math.floor((ms % 60_000) / 1000);
    if (minutes < 60) return `${minutes}m ${seconds}s`;
    const hours = Math.floor(minutes / 60);
    const remainingMinutes = minutes % 60;
    return `${hours}h ${remainingMinutes}m`;
  } catch {
    return "\u2014";
  }
}

export function getSpanColor(name: string): string {
  if (name.startsWith("workflow.phase.")) return "text-blue-400";
  if (name.startsWith("workflow.")) return "text-blue-300";
  if (name.startsWith("ai.")) return "text-purple-400";
  if (name.startsWith("python.")) return "text-yellow-400";
  if (name.startsWith("playwright.")) return "text-green-400";
  if (name.startsWith("image.")) return "text-orange-400";
  if (name.startsWith("api.")) return "text-cyan-400";
  return "text-muted-foreground";
}

export function parseJson(jsonStr: string | null | undefined): unknown {
  if (!jsonStr) return null;
  try {
    return JSON.parse(jsonStr);
  } catch {
    return jsonStr;
  }
}

export function formatJson(data: unknown): string {
  if (data === null || data === undefined) return "";
  if (typeof data === "string") return data;
  try {
    return JSON.stringify(data, null, 2);
  } catch {
    return String(data);
  }
}

export function prettyPrintJson(jsonStr: string): string {
  try {
    return JSON.stringify(JSON.parse(jsonStr), null, 2);
  } catch {
    return jsonStr;
  }
}

export function truncateUrl(url: string, maxLength: number = 60): string {
  if (url.length <= maxLength) return url;
  return url.substring(0, maxLength - 3) + "...";
}

export function truncateWithEllipsis(text: string, maxLength: number): string {
  if (text.length <= maxLength) return text;
  return text.substring(0, maxLength) + "\n... (truncated)";
}
