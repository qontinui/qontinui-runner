/**
 * Zone-profile persistence — the single source of truth for the profile
 * shape + its `setting_get` / `setting_set` keys.
 *
 * Extracted verbatim from `ZoneProfilePicker.tsx` (which now imports from
 * here) so a profile can be applied PROGRAMMATICALLY — by name, without the
 * picker's dropdown state — as projects-dashboard plan §7.2 step 3 requires
 * ("restore the project's `zone_profile` if set"). There is exactly one
 * key-derivation and one load/save implementation; `useZoneProfileRestore`
 * and the picker both go through it.
 *
 * Profiles are stored in the runner's Rust-side settings (NOT localStorage),
 * page-namespaced: the `"default"` page keeps the unsuffixed key for
 * back-compat with profiles saved before terminal pages existed.
 */

import { invoke } from "@tauri-apps/api/core";

/** A Claude session pinned to a zone index by a saved profile. */
export interface ZoneSessionInfo {
  zoneIndex: number;
  claudeSessionId: string;
  claudeConfigDir?: string;
}

export interface ZoneProfile {
  layoutId: string;
  labels: Record<number, string>;
  notes: Record<number, string>;
  pins: number[];
  autoApprovePatterns: string[];
  /** Claude session IDs per zone (added in later version, may be absent in old profiles) */
  sessions?: ZoneSessionInfo[];
}

export function profileSettingKey(pageId: string): string {
  return pageId === "default" ? "zone-profiles" : `zone-profiles:${pageId}`;
}

export function activeProfileSettingKey(pageId: string): string {
  return pageId === "default" ? "zone-active-profile" : `zone-active-profile:${pageId}`;
}

export async function loadProfilesFromDb(pageId: string): Promise<Record<string, ZoneProfile>> {
  try {
    const result = await invoke<Record<string, ZoneProfile> | null>("setting_get", {
      key: profileSettingKey(pageId),
    });
    return result ?? {};
  } catch {
    return {};
  }
}

export async function saveProfilesToDb(profiles: Record<string, ZoneProfile>, pageId: string) {
  try {
    await invoke("setting_set", {
      key: profileSettingKey(pageId),
      value: profiles,
    });
  } catch {
    /* ignore save failures */
  }
}

export async function loadActiveProfileFromDb(pageId: string): Promise<string | null> {
  try {
    const result = await invoke<string | null>("setting_get", {
      key: activeProfileSettingKey(pageId),
    });
    return result ?? null;
  } catch {
    return null;
  }
}

export async function saveActiveProfileToDb(name: string | null, pageId: string) {
  try {
    await invoke("setting_set", {
      key: activeProfileSettingKey(pageId),
      value: name,
    });
  } catch {
    /* ignore save failures */
  }
}
