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

/// Atomic rewrite: read → patch `active_tenant_id` → unique-temp write →
/// rename. Other top-level fields (`device_id`, `hostname`, `name`) are
/// preserved verbatim. Returns an error string fit for the frontend.
///
/// **FAILS CLOSED on a machine.json that has no device identity.** This used
/// to fall back to an EMPTY `serde_json::Map` whenever the file was missing,
/// unparseable, or not a JSON object — so a tenant switch wrote
/// `{"active_tenant_id": …}` with no `device_id`. That file is worse than no
/// file: the minter (`pair::ensure_device_initialized_at`) sees
/// `path.exists()` and delegates to the preserving backfill, which finds no
/// key to preserve, so the identity is permanently wedged; and the only
/// documented recovery — `rm machine.json` + `device init` — MINTS A FRESH
/// UUID, i.e. a brand-new `coord.devices` row for the same physical machine.
/// Refusing to write is strictly better: the existing identity file (if any)
/// is left intact and the operator gets guidance that does not re-mint.
///
/// The refusal messages distinguish **recoverable** from **unrecoverable**.
/// An ABSENT file has nothing to lose, so `device init` is the right advice.
/// A file that is present but corrupt / non-object may still hold this
/// machine's real UUID, so the advice is "inspect and repair by hand" and
/// explicitly NOT `rm` — `rm` + `device init` mints a fresh id, which is the
/// very outcome this function exists to prevent.
/// Plan `2026-08-06-device-identity-is-per-profile-not-per-machine` §0.3 H4.
fn write_active_tenant_id(path: &Path, tenant_id: &str) -> Result<(), String> {
    // Read the existing JSON object so we preserve sibling fields — and
    // REFUSE outright if there is no device identity to preserve.
    let mut obj = match read_machine_file(path) {
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => {
            return Err(format!(
                "tenant: refusing to write {} — it is not a JSON object, so it holds no \
                 device_id to preserve. Inspect it by hand; do NOT `rm` it (that mints a \
                 NEW device identity and a new coord.devices row).",
                path.display()
            ))
        }
        // `read_machine_file` collapses "absent" and "present but unreadable"
        // into one `None`, and the two need OPPOSITE guidance: `rm` is harmless
        // on an absent file and DESTRUCTIVE on a corrupt one (which may still
        // carry the machine's real UUID). Split on `path.exists()` so the
        // operator is not funnelled into a re-mint.
        None if path.exists() => {
            return Err(format!(
                "tenant: refusing to write {} — it EXISTS but is unreadable (I/O error or \
                 invalid JSON), so a write here would replace it with a machine.json that \
                 has no device_id and wedge this machine's identity. The file may still \
                 contain this machine's real device_id: inspect and repair it by hand; do \
                 NOT `rm` it (that mints a NEW device identity and a new coord.devices \
                 row). Then switch tenant again.",
                path.display()
            ))
        }
        None => {
            return Err(format!(
                "tenant: refusing to write {} — it does not exist, so a write here would \
                 produce a machine.json with no device_id and wedge this machine's \
                 identity. Nothing is at risk: run `qontinui_profile device init` (it \
                 mints only when there is genuinely no file, and re-uses any existing \
                 device_id), then switch tenant again.",
                path.display()
            ))
        }
    };
    // `machine_id` is the pre-rename spelling every reader still aliases, so a
    // legacy-shaped file counts as having an identity.
    let identity_key = ["device_id", "machine_id"].into_iter().find(|k| {
        obj.get(*k)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty())
    });
    let Some(identity_key) = identity_key else {
        return Err(format!(
            "tenant: refusing to write {} — it carries no device_id. Writing would \
             permanently wedge this machine's identity (the minter skips an existing \
             file). Restore the file or run `qontinui_profile device init`, then switch \
             tenant again.",
            path.display()
        ));
    };
    // Normalize surrounding whitespace on the identity we are about to
    // re-serialize. Every READER trims (`machine_identity::read_device_id_at`),
    // so an on-disk `" abc "` and the id presented to coord would otherwise
    // differ by exactly the padding — two `coord.devices` rows for one machine.
    // Writing the trimmed form makes disk and wire agree.
    let trimmed_identity = obj
        .get(identity_key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string());
    if let Some(trimmed) = trimmed_identity {
        obj.insert(identity_key.to_string(), serde_json::Value::String(trimmed));
    }
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
    // Unique-temp atomic write. The fixed `machine.json.tmp` this used to
    // share with the three other writers is raced by every runner instance.
    qontinui_runner_lib::fs_atomic::atomic_write(path, &pretty)
        .map_err(|e| format!("tenant: atomic write machine.json failed: {e}"))?;
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
/// "paired_user.json" | null, candidates: [tenant_id, …] }`. `candidates`
/// is the set of tenants this device is LOCALLY bound to, read straight
/// from `paired_user.json` v2 (`pair::read_paired_binding_tenant_ids`,
/// which migrates a legacy single-entry file into a one-element set) — a
/// local file read, NO coord round-trip, so the switcher renders offline.
/// The frontend treats `candidates.length <= 1` as "operator is in exactly
/// one tenant" and elides the switcher accordingly.
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

    // The device's bound tenants live locally in `paired_user.json` v2 — the
    // switcher must render OFFLINE, so this is a local read, not a coord query.
    let candidates: Vec<String> = qontinui_runner_lib::pair::read_paired_binding_tenant_ids()
        .into_iter()
        .map(|t| t.to_string())
        .collect();

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "active_tenant_id": tenant,
            "source": source,
            "candidates": candidates,
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
    fn read_missing_returns_none() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(read_active_tenant_id(&path).is_none());
    }

    // ------------------------------------------------------------------
    // Identity preservation + fail-closed — plan
    // `2026-08-06-device-identity-is-per-profile-not-per-machine` Phase 2.
    // ------------------------------------------------------------------

    /// A tenant switch preserves `device_id`, `hostname` and `name` verbatim.
    /// This is the invariant that keeps one machine to one `coord.devices`
    /// row: coord UPSERTs `ON CONFLICT (device_id)`, so losing the id here
    /// means the next `device init` mints a new one and a new row.
    #[test]
    fn write_preserves_device_id_hostname_and_name() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(
            &path,
            br#"{"device_id":"c79a07d5-0000-4000-8000-000000000001","hostname":"spaceship","name":"primary","extra":7}"#,
        )
        .unwrap();

        write_active_tenant_id(&path, "c231d9da-0000-4000-8000-000000000002").unwrap();

        let v = read_machine_file(&path).unwrap();
        assert_eq!(
            v.get("device_id").and_then(|x| x.as_str()),
            Some("c79a07d5-0000-4000-8000-000000000001"),
            "device_id must survive a tenant switch"
        );
        assert_eq!(
            v.get("hostname").and_then(|x| x.as_str()),
            Some("spaceship")
        );
        assert_eq!(v.get("name").and_then(|x| x.as_str()), Some("primary"));
        assert_eq!(v.get("extra").and_then(|x| x.as_i64()), Some(7));
        assert_eq!(
            read_active_tenant_id(&path).as_deref(),
            Some("c231d9da-0000-4000-8000-000000000002")
        );
    }

    /// FAIL CLOSED: a missing machine.json must NOT be created here. The old
    /// empty-map fallback wrote a `device_id`-less file, which permanently
    /// blocks the minter (`ensure_device_initialized_at` sees `path.exists()`
    /// and delegates to the preserving backfill, which finds nothing to
    /// preserve) — and the only documented recovery mints a fresh UUID, i.e.
    /// a new `coord.devices` row for the same machine.
    #[test]
    fn write_refuses_when_machine_json_is_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        let err = write_active_tenant_id(&path, "tenant-uuid")
            .expect_err("a missing machine.json must be refused");
        assert!(err.contains("refusing to write"), "got: {err}");
        // An ABSENT file is the one case where `device init` is right and `rm`
        // is harmless — the guidance must say exactly that.
        assert!(
            err.contains("does not exist"),
            "the absent case must name itself as absent, got: {err}"
        );
        assert!(
            err.contains("qontinui_profile device init"),
            "the absent case must point at `device init`, got: {err}"
        );
        assert!(
            !path.exists(),
            "the refusal must not leave a device_id-less machine.json behind"
        );
    }

    /// FAIL CLOSED on an unparseable, non-object, or `device_id`-less file —
    /// leave it byte-identical for inspection, AND give guidance specific to
    /// that case.
    ///
    /// Asserting only `contains("refusing to write")` is what let the original
    /// defect through: `read_machine_file` returns `None` for BOTH an absent
    /// and a corrupt file, and the single shared message told the operator to
    /// run `device init` — which itself refuses on a corrupt file and used to
    /// say `rm`. `rm` + `device init` mints a fresh UUID, i.e. the second
    /// `coord.devices` row this whole change exists to prevent. So the pin is
    /// per-case: does the message name THIS file's class, and does it give the
    /// right (non-re-minting) recovery for it?
    #[test]
    fn write_refuses_with_guidance_specific_to_each_bad_file() {
        let dir = tempdir().unwrap();
        for (label, contents, must_contain, must_not_contain) in [
            // Present but unreadable: the bytes may still hold the real UUID,
            // so the advice is hand-repair and an explicit "do NOT rm".
            (
                "corrupt",
                &b"{ not json"[..],
                &["EXISTS but is unreadable", "do NOT `rm`"][..],
                &["device init"][..],
            ),
            (
                "not an object",
                &b"[1,2,3]"[..],
                &["not a JSON object", "do NOT `rm`"][..],
                &["device init"][..],
            ),
            // Parsed fine and provably carries no identity: nothing to lose.
            (
                "no device_id",
                &br#"{"hostname":"spaceship"}"#[..],
                &["carries no device_id"][..],
                &[][..],
            ),
            (
                "blank device_id",
                &br#"{"device_id":"   "}"#[..],
                &["carries no device_id"][..],
                &[][..],
            ),
        ] {
            let path = dir.path().join(format!("{}.json", label.replace(' ', "_")));
            std::fs::write(&path, contents).unwrap();
            let err = match write_active_tenant_id(&path, "tenant-uuid") {
                Ok(()) => panic!("{label} must be refused, not written"),
                Err(e) => e,
            };
            assert!(
                err.contains("refusing to write"),
                "{label}: expected a refusal, got: {err}"
            );
            for needle in must_contain {
                assert!(
                    err.contains(needle),
                    "{label}: refusal must mention {needle:?}, got: {err}"
                );
            }
            for needle in must_not_contain {
                assert!(
                    !err.contains(needle),
                    "{label}: refusal must NOT mention {needle:?} (that path re-mints), got: {err}"
                );
            }
            assert_eq!(
                std::fs::read(&path).unwrap(),
                contents,
                "{label}: the file must be left byte-identical for inspection"
            );
        }
    }

    /// The absent and the present-but-corrupt refusals must not be the SAME
    /// message. They came from one `None` arm and therefore were, which is how
    /// a corrupt file's operator got told to run `device init` → which refuses
    /// → whose message said `rm` → which mints a new identity.
    #[test]
    fn absent_and_corrupt_refusals_are_not_the_same_message() {
        let dir = tempdir().unwrap();
        let absent = dir.path().join("absent.json");
        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, b"{ not json").unwrap();

        let absent_err = write_active_tenant_id(&absent, "t").expect_err("absent must refuse");
        let corrupt_err = write_active_tenant_id(&corrupt, "t").expect_err("corrupt must refuse");
        assert_ne!(
            absent_err.replace("absent.json", "X").as_str(),
            corrupt_err.replace("corrupt.json", "X").as_str(),
            "absent and corrupt need OPPOSITE guidance, so they cannot share a message"
        );
    }

    /// A padded on-disk `device_id` is normalized on write. Every READER trims
    /// (`machine_identity::read_device_id_at`), so leaving `" abc "` on disk
    /// means disk and the coord-facing value disagree by exactly the padding —
    /// and coord UPSERTs `ON CONFLICT (device_id)`, so that is two rows.
    #[test]
    fn write_trims_a_padded_device_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(&path, br#"{"device_id":"  padded-id  ","hostname":"x"}"#).unwrap();

        write_active_tenant_id(&path, "tenant-uuid").unwrap();

        let v = read_machine_file(&path).unwrap();
        assert_eq!(
            v.get("device_id").and_then(|x| x.as_str()),
            Some("padded-id"),
            "the identity must be written back TRIMMED, matching what readers see"
        );
    }

    /// Same normalization for the legacy `machine_id` spelling.
    #[test]
    fn write_trims_a_padded_legacy_machine_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(&path, br#"{"machine_id":"  legacy-id  "}"#).unwrap();

        write_active_tenant_id(&path, "tenant-uuid").unwrap();

        let v = read_machine_file(&path).unwrap();
        assert_eq!(
            v.get("machine_id").and_then(|x| x.as_str()),
            Some("legacy-id")
        );
    }

    /// A legacy `machine_id`-spelled file still HAS an identity, so a tenant
    /// switch on it is allowed (every reader aliases the old spelling).
    #[test]
    fn write_accepts_the_legacy_machine_id_spelling() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(&path, br#"{"machine_id":"legacy-id","hostname":"x"}"#).unwrap();
        write_active_tenant_id(&path, "tenant-uuid").unwrap();
        let v = read_machine_file(&path).unwrap();
        assert_eq!(
            v.get("machine_id").and_then(|x| x.as_str()),
            Some("legacy-id")
        );
        assert_eq!(read_active_tenant_id(&path).as_deref(), Some("tenant-uuid"));
    }

    /// Defect 4: this writer must not use the single shared
    /// `machine.json.tmp` path, which every runner instance races.
    #[test]
    fn write_does_not_use_the_shared_fixed_temp_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("machine.json");
        std::fs::write(&path, br#"{"device_id":"abc","hostname":"x"}"#).unwrap();
        let squatted = path.with_extension("json.tmp");
        std::fs::create_dir(&squatted).unwrap();

        write_active_tenant_id(&path, "tenant-uuid")
            .expect("write must succeed despite the squatted legacy tmp path");
        assert_eq!(read_active_tenant_id(&path).as_deref(), Some("tenant-uuid"));
        assert!(squatted.is_dir(), "the squatted path must be untouched");
    }
}
