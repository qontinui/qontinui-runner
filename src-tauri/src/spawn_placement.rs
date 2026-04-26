//! Spawn-placement resolution: translates a monitor-local logical
//! placement (configured per-instance in `settings.json`) into an
//! absolute, virtual-desktop physical position the OS can use to
//! position a Tauri window at launch.
//!
//! All coordinates returned by [`resolve_to_global_physical`] are in
//! the same physical-pixel coordinate space Tauri/Windows uses for
//! window positioning (the virtual desktop). Sizes are physical
//! pixels too — callers can pass them straight through to
//! `WebviewWindowBuilder::position()` / `.inner_size()` (or, in our
//! case, to env vars consumed by the spawned child).
//!
//! Scope: this module is intentionally read-only and side-effect-free
//! against the underlying monitor list. All IO is `app.available_monitors()`.

use serde::Serialize;
use tauri::Manager;

use crate::settings::{MonitorDescriptor, SpawnPlacement};

/// Default window size (logical CSS pixels) when a `SpawnPlacement`
/// omits width/height.
const DEFAULT_LOGICAL_WIDTH: u32 = 1920;
const DEFAULT_LOGICAL_HEIGHT: u32 = 1080;

/// Resolved placement in absolute virtual-desktop physical pixels.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedPlacement {
    pub global_x: i32,
    pub global_y: i32,
    pub width: u32,
    pub height: u32,
    /// For diagnostics/logging — never relied on by callers.
    pub monitor_label: String,
}

/// Resolve a [`SpawnPlacement`] against the live monitor list.
///
/// Returns an error string suitable for `tracing::warn!` when no
/// matching monitor is found or when Tauri can't enumerate monitors.
pub fn resolve_to_global_physical(
    app: &tauri::AppHandle,
    placement: &SpawnPlacement,
) -> Result<ResolvedPlacement, String> {
    let monitors = app
        .available_monitors()
        .map_err(|e| format!("available_monitors failed: {}", e))?;

    if monitors.is_empty() {
        return Err("no monitors reported by Tauri".into());
    }

    let primary = app.primary_monitor().ok().flatten();

    let (chosen_idx, chosen) = pick_monitor(&monitors, primary.as_ref(), &placement.monitor)?;

    let pos = chosen.position();
    let scale = chosen.scale_factor();

    let logical_w = placement.width.unwrap_or(DEFAULT_LOGICAL_WIDTH);
    let logical_h = placement.height.unwrap_or(DEFAULT_LOGICAL_HEIGHT);

    let local_phys_x = (placement.x as f64 * scale).round() as i32;
    let local_phys_y = (placement.y as f64 * scale).round() as i32;
    let phys_w = (logical_w as f64 * scale).round().max(1.0) as u32;
    let phys_h = (logical_h as f64 * scale).round().max(1.0) as u32;

    let global_x = pos.x + local_phys_x;
    let global_y = pos.y + local_phys_y;

    let monitor_label = format!(
        "monitor[{}] name={:?} pos=({}, {}) scale={}",
        chosen_idx,
        chosen.name(),
        pos.x,
        pos.y,
        scale
    );

    Ok(ResolvedPlacement {
        global_x,
        global_y,
        width: phys_w,
        height: phys_h,
        monitor_label,
    })
}

/// Pick the monitor matching the descriptor. Mirrors the labeling
/// logic used by `mcp::monitors::get_monitors` so "left"/"center"/
/// "right" mean the same thing in both places.
fn pick_monitor<'a>(
    monitors: &'a [tauri::Monitor],
    primary: Option<&tauri::Monitor>,
    descriptor: &MonitorDescriptor,
) -> Result<(usize, &'a tauri::Monitor), String> {
    match descriptor {
        MonitorDescriptor::Index { index } => monitors
            .get(*index)
            .map(|m| (*index, m))
            .ok_or_else(|| format!("monitor index {} out of range (have {})", index, monitors.len())),

        MonitorDescriptor::Name { name } => monitors
            .iter()
            .enumerate()
            .find(|(_, m)| m.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("no monitor with name {:?}", name)),

        MonitorDescriptor::Position { position } => {
            let pos = position.to_lowercase();

            if pos == "primary" {
                if let Some(prim) = primary {
                    // Find the primary in the list to get its index.
                    if let Some((idx, m)) = monitors.iter().enumerate().find(|(_, m)| {
                        let mp = m.position();
                        let mpr = prim.position();
                        let ms = m.size();
                        let mss = prim.size();
                        mp.x == mpr.x
                            && mp.y == mpr.y
                            && ms.width == mss.width
                            && ms.height == mss.height
                    }) {
                        return Ok((idx, m));
                    }
                }
                // Fall back to the first monitor if Tauri couldn't
                // identify a primary — matches `get_monitors`.
                return Ok((0, &monitors[0]));
            }

            // Spatial roles: replicate the "sort by x" logic.
            let xs: Vec<i32> = monitors.iter().map(|m| m.position().x).collect();
            let min_x = *xs.iter().min().unwrap_or(&0);
            let max_x = *xs.iter().max().unwrap_or(&0);

            // Single-monitor case: every spatial role collapses to "center".
            if monitors.len() == 1 {
                return Ok((0, &monitors[0]));
            }

            let want = pos.as_str();
            let result = monitors.iter().enumerate().find(|(_, m)| {
                let mx = m.position().x;
                match want {
                    "left" => mx == min_x,
                    "right" => mx == max_x,
                    "center" => mx != min_x && mx != max_x,
                    _ => false,
                }
            });

            result.ok_or_else(|| format!("no monitor matching position={:?}", position))
        }
    }
}
