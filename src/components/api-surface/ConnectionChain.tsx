import { useMemo } from "react";
import { ArrowDown, FileCode } from "lucide-react";
import type { ApiSurface, ApiConnection } from "./types";
import { NODE_COLORS, type NodeCategory } from "./types";

interface Props {
  surface: ApiSurface;
  selectedNode: { type: string; name: string } | null;
}

interface ChainLink {
  type: string;
  name: string;
  file?: string;
  line?: number;
  detail?: string;
}

export function ConnectionChain({ surface, selectedNode }: Props) {
  const chain = useMemo(() => {
    if (!selectedNode) return [];
    return buildChain(surface, selectedNode.type, selectedNode.name);
  }, [surface, selectedNode]);

  if (!selectedNode) {
    return (
      <div className="p-4 text-center text-zinc-500 text-xs">
        Click a node to see its full call chain
      </div>
    );
  }

  if (chain.length === 0) {
    return (
      <div className="p-4 text-center text-zinc-500 text-xs">
        No connections found for {selectedNode.name}
      </div>
    );
  }

  return (
    <div className="p-3">
      <h3 className="text-xs font-semibold text-zinc-300 mb-3">
        Call Chain: {selectedNode.name}
      </h3>
      <div className="space-y-1">
        {chain.map((link, i) => {
          const colors = NODE_COLORS[link.type as NodeCategory];
          return (
            <div key={i}>
              {i > 0 && (
                <div className="flex justify-center py-0.5">
                  <ArrowDown className="w-3 h-3 text-zinc-600" />
                </div>
              )}
              <div
                className="rounded px-3 py-2 text-xs"
                style={{
                  background: colors?.bg ?? "rgba(113,113,122,0.15)",
                  borderLeft: `3px solid ${colors?.border ?? "#71717a"}`,
                }}
              >
                <div className="flex items-center gap-1.5 mb-0.5">
                  <span
                    className="text-[10px] font-medium uppercase tracking-wider"
                    style={{ color: colors?.text ?? "#a1a1aa" }}
                  >
                    {formatType(link.type)}
                  </span>
                </div>
                <div className="font-mono text-zinc-200">{link.name}</div>
                {link.file && (
                  <div className="flex items-center gap-1 mt-1 text-[10px] text-zinc-500">
                    <FileCode className="w-2.5 h-2.5" />
                    {link.file}
                    {link.line ? `:${link.line}` : ""}
                  </div>
                )}
                {link.detail && (
                  <div className="mt-1 text-[10px] text-zinc-500 font-mono truncate">
                    {link.detail}
                  </div>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function formatType(type: string): string {
  return type.replace(/_/g, " ");
}

function buildChain(surface: ApiSurface, type: string, name: string): ChainLink[] {
  const chain: ChainLink[] = [];
  const visited = new Set<string>();

  // Walk upstream (callers)
  const upstream = walkUpstream(surface, type, name, visited);
  chain.push(...upstream.reverse());

  // Current node
  const current = findNodeInfo(surface, type, name);
  chain.push(current);

  // Walk downstream (callees)
  const downstream = walkDownstream(surface, type, name, visited);
  chain.push(...downstream);

  return chain;
}

function findNodeInfo(surface: ApiSurface, type: string, name: string): ChainLink {
  if (type === "tauri_command") {
    const cmd = surface.tauriCommands.find((c) => c.name === name);
    return { type, name, file: cmd?.file, line: cmd?.line, detail: cmd?.returnType };
  }
  if (type === "mcp_route") {
    const route = surface.mcpRoutes.find((r) => `${r.method} ${r.path}` === name || r.path === name);
    return { type, name, file: route?.file, line: route?.line, detail: route?.handler };
  }
  if (type === "pg_method") {
    const method = surface.pgMethods.find((m) => m.name === name);
    return { type, name, file: method?.file, line: method?.line };
  }
  if (type === "clorinde_query") {
    const query = surface.clorindeQueries.find((q) => q.name === name);
    return { type, name, file: query?.file, line: query?.line, detail: query?.sqlPreview?.slice(0, 100) };
  }
  if (type === "db_table") {
    const table = surface.dbTables.find((t) => t.name === name);
    return { type, name, file: table?.file, line: table?.line };
  }
  if (type === "python_event") {
    const event = surface.pythonEvents.find((e) => e.value === name);
    return { type, name, file: event?.file, line: event?.line };
  }
  if (type === "ui_bridge_route") {
    const route = surface.uiBridgeRoutes.find((r) => r.path === name);
    return { type, name, file: route?.file, line: route?.line, detail: route ? `${route.method} ${route.path}` : undefined };
  }
  if (type === "frontend") {
    return { type, name, file: name };
  }
  return { type, name };
}

function walkUpstream(
  surface: ApiSurface,
  type: string,
  name: string,
  visited: Set<string>,
): ChainLink[] {
  const key = `${type}:${name}`;
  if (visited.has(key)) return [];
  visited.add(key);

  const links: ChainLink[] = [];

  // Find connections where this is the target
  for (const conn of surface.connections) {
    if (conn.toType === type && conn.toName === name) {
      const parent = findNodeInfo(surface, conn.fromType, conn.fromName);
      const upstream = walkUpstream(surface, conn.fromType, conn.fromName, visited);
      links.push(...upstream, parent);
    }
  }

  return links;
}

function walkDownstream(
  surface: ApiSurface,
  type: string,
  name: string,
  visited: Set<string>,
): ChainLink[] {
  const key = `${type}:${name}`;
  if (visited.has(key)) return [];
  visited.add(key);

  const links: ChainLink[] = [];

  for (const conn of surface.connections) {
    if (conn.fromType === type && conn.fromName === name) {
      const child = findNodeInfo(surface, conn.toType, conn.toName);
      const downstream = walkDownstream(surface, conn.toType, conn.toName, visited);
      links.push(child, ...downstream);
    }
  }

  // For clorinde queries, add the DB table as terminal (if not already visited)
  if (type === "clorinde_query") {
    const query = surface.clorindeQueries.find((q) => q.name === name);
    if (query?.table && !visited.has(`db_table:${query.table}`)) {
      visited.add(`db_table:${query.table}`);
      const table = surface.dbTables.find((t) => t.name === query.table);
      if (table) {
        links.push({
          type: "db_table",
          name: table.name,
          file: table.file,
          line: table.line,
          detail: `${table.columnCount} columns`,
        });
      }
    }
  }

  return links;
}
