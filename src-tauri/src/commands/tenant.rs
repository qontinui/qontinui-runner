//! Tenant resolution Tauri commands — plan
//! [`2026-05-22-coord-native-session-coordination`] §D12 + §Phase 4.
//!
//! ## `active_tenant_id` semantics (Phase 8b, plan
//! `2026-07-02-session-scoped-multi-tenant-device-binding` §D4)
//!
//! `machine.json::active_tenant_id` is the **default for NEW sessions and
//! for device-level surfaces** (heartbeat, census, backstop, maintenance,
//! doctor, flag-poll) — NOT "the only tenant this device serves". A device
//! holds N concurrent tenant bindings (`paired_user.json` v2 +
//! `coord.tenant_devices`); each session records its own tenant at
//! creation (spawn input, else this default) and keeps it for life, so
//! switching the active tenant re-points FUTURE sessions only.
//!
//! The frontend [`TenantContext`] reads the active tenant id (per machine)
//! and offers a switcher when the operator belongs to >1 tenant. The
//! source of truth for the default is `~/.qontinui/machine.json`'s
//! `active_tenant_id` field (D12 explicitly nominates this file as the
//! per-machine pin). At first launch on a multi-tenant operator account
//! the file may not yet hold the field; in that case we fall back to the
//! DEFAULT binding available via the existing
//! [`qontinui_runner_lib::pair::read_paired_tenant_id_from_disk`] reader
//! (v2-aware: `default_tenant_id`) so the runner UI can render before the
//! operator has explicitly chosen.
//!
//! Note: a richer "list of tenants the operator belongs to" requires a
//! coord round-trip (Phase 5 dashboard). Phase 4 only needs the active
//! pin + persistence — the runner header switcher renders nothing when
//! the operator is in exactly one tenant, which is the common case.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::commands::CommandResponse;

/// On-disk shape of `~/.qontinui/machine.json` (subset). Additive — extra
/// fields (`device_id`, `hostname`, `name`) are preserved across reads via
/// `serde_json::Value` roundtripping in [`write_active_tenant_id`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MachineFileTenantSlice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_tenant_id: Option<String>,
}

fn machine_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("machine.json"))
}

fn read_machine_file(path: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes).ok()
}

fn read_active_tenant_id(path: &Path) -> Option<String> {
    let v = read_machine_file(path)?;
    v.get("active_tenant_id")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

/// Atomic rewrite: read → patch `active_tenant_id` → write to `.tmp` →
/// rename. Other top-level fields (`device_id`, `hostname`, `name`) are
/// preserved verbatim. Returns an error string fit for the frontend.
fn write_active_tenant_id(path: &Path, tenant_id: &str) -> Result<(), String> {
    // Read existing JSON object (if any) so we preserve sibling fields.
    let mut obj = match read_machine_file(path) {
        Some(serde_json::Value::Object(map)) => map,
        Some(_) | None => serde_json::Map::new(),
    };
    obj.insert(
        "active_tenant_id".to_string(),
        serde_json::Value::String(tenant_id.to_string()),
    );
    let pretty = serde_json::to_vec_pretty(&serde_json::Value::Object(obj))
        .map_err(|e| format!("tenant: serialize machine.json failed: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("tenant: create ~/.qontinui dir failed: {e}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &pretty)
        .map_err(|e| format!("tenant: write machine.json.tmp failed: {e}"))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("tenant: rename machine.json.tmp failed: {e}"))?;
    Ok(())
}

/// Return the active tenant id for this machine — the DEFAULT binding for
/// new sessions and device-level surfaces (Phase 8b semantics; existing
/// sessions keep their own recorded tenant). Resolution order:
///
/// 1. `~/.qontinui/machine.json` → `active_tenant_id` field (D12 pin)
/// 2. Cached pair file (`paired_user.json`) → `tenant_id` (fallback so
///    the runner UI has something to render before the operator pins)
///
/// Returns `{ active_tenant_id: string | null, source: "machine.json" |
/// "paired_user.json" | null, candidates: [] }`. Phase 4 ships the
/// `candidates` field empty — Phase 5 dashboard will populate it from a
/// coord round-trip. The frontend uses the empty list as "operator is in
/// exactly one tenant" and elides the switcher accordingly.
#[tauri::command]
pub fn get_active_tenant() -> Result<CommandResponse, String> {
    let machine_path = machine_file_path()
        .ok_or_else(|| "tenant: no home directory; cannot read machine.json".to_string())?;

    let (tenant, source) = if let Some(t) = read_active_tenant_id(&machine_path) {
        (Some(t), Some("machine.json"))
    } else if let Some(t) = qontinui_runner_lib::pair::read_paired_tenant_id_from_disk() {
        (Some(t), Some("paired_user.json"))
    } else {
        (None, None)
    };

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "active_tenant_id": tenant,
            "source": source,
            "candidates": Vec::<String>::new(),
        })),
    })
}

/// Persist the operator's tenant choice to
/// `~/.qontinui/machine.json::active_tenant_id`. Plan §D12 — stored
/// per-machine; Phase 8b semantics: this sets the DEFAULT for NEW sessions
/// and device-level surfaces on a device that may hold N concurrent
/// bindings. It does NOT migrate already-running sessions (each session is
/// stamped with its tenant at start and keeps it for life) and does NOT
/// unpair any other binding.
#[tauri::command]
pub fn set_active_tenant(tenant_id: String) -> Result<CommandResponse, String> {
    let trimmed = tenant_id.trim();
    if trimmed.is_empty() {
        return Err("tenant: tenant_id cannot be empty".to_string());
    }
    let machine_path = machine_file_path()
        .ok_or_else(|| "tenant: no home directory; cannot write machine.json".to_string())?;
    write_active_tenant_id(&machine_path, trimmed)?;
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "active_tenant_id": trimmed,
            "source": "machine.json",
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        // Seed with sibling fields to confirm preservation.
        std::fs::write(&path, br#"{"device_id":"abc","hostname":"x"}"#).unwrap();
        write_active_tenant_id(&path, "11111111-1111-1111-1111-111111111111").unwrap();
        let v = read_machine_file(&path).unwrap();
        // Sibling preserved.
        assert_eq!(v.get("device_id").and_then(|x| x.as_str()), Some("abc"));
        // New field present.
        let got = read_active_tenant_id(&path).unwrap();
        assert_eq!(got, "11111111-1111-1111-1111-111111111111");
    }

    #[test]
    fn write_creates_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        write_active_tenant_id(&path, "tenant-uuid").unwrap();
        let got = read_active_tenant_id(&path).unwrap();
        assert_eq!(got, "tenant-uuid");
    }

    #[test]
    fn read_missing_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(read_active_tenant_id(&path).is_none());
    }
}
