/**
 * Reads the bound tenants' coord slugs for `/spawn-ai --tenant <name>`
 * (`resolveTenantArg`). One coord round-trip through the `get_tenant_slugs`
 * Tauri command; a failed read REJECTS rather than answering `[]`, so the
 * caller can tell "names unknown" from "no tenant has a name".
 */

import { invoke } from "@tauri-apps/api/core";

import type { TenantSlug } from "./useTerminalCommands";

interface SlugWire {
  tenant_id: string;
  slug: string;
}

interface Envelope {
  success: boolean;
  message?: string | null;
  data?: SlugWire[] | null;
}

/** Map the wire rows to `TenantSlug`, dropping any row that is not a pair of non-empty strings. */
export function slugsFromWire(rows: unknown): TenantSlug[] {
  if (!Array.isArray(rows)) return [];
  const out: TenantSlug[] = [];
  for (const r of rows as Partial<SlugWire>[]) {
    if (typeof r?.tenant_id === "string" && typeof r?.slug === "string" && r.slug.trim()) {
      out.push({ tenantId: r.tenant_id, slug: r.slug.trim() });
    }
  }
  return out;
}

export async function fetchTenantSlugs(): Promise<TenantSlug[]> {
  const res = await invoke<Envelope>("get_tenant_slugs");
  if (!res?.success) throw new Error(res?.message ?? "get_tenant_slugs failed");
  return slugsFromWire(res.data);
}
