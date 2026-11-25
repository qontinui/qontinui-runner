//! Shared by the integration tests: tear down a test's pane dir.
//!
//! Since plan 2026-11-04 (socket-overflow) Phase 4 a holder's Unix socket lives
//! in the per-user socket root (`pane::socket_root`), not in its pane dir, so
//! `remove_dir_all(pane_dir)` alone would leave every test holder's socket
//! behind in that shared root. This removes, for every per-pane file stem
//! found in the dir (and one level of subdirectories), that pane's socket and
//! staged twin under every candidate root — derived, never listed, exactly as
//! the boot sweep does — and then the dir itself.

use std::io;
use std::path::Path;

pub fn remove_pane_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut dirs = vec![dir.to_path_buf()];
        if let Ok(entries) = std::fs::read_dir(dir) {
            dirs.extend(
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && !p.is_symlink()),
            );
        }
        for d in dirs {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(stem) = name.to_str().and_then(|n| n.split('.').next()) else {
                    continue;
                };
                let Ok(pane) = qontinui_pty_holder::pane::PaneId::new(stem) else {
                    continue;
                };
                for sock in qontinui_pty_holder::pane::socket_candidates(&d, &pane) {
                    let _ = std::fs::remove_file(qontinui_pty_holder::transport::staged_path(&sock));
                    let _ = std::fs::remove_file(&sock);
                }
            }
        }
    }
    std::fs::remove_dir_all(dir)
}
