fn main() {
    // Tell Cargo to re-run this build script (and re-embed the frontend) when
    // the dist directory changes.  Without this, incremental builds silently
    // serve a stale frontend bundle from the compile-time cache.
    println!("cargo:rerun-if-changed=../dist");
    // Force a re-run on every cargo build so RUNNER_BUILD_ID's millisecond
    // suffix actually changes between invocations. Without this, cargo will
    // cache the build-script output and re-stamp identical values into the
    // binary, defeating the SW-cache-invalidation watcher on rebuilds where
    // no other inputs changed.
    println!("cargo:rerun-if-changed=build.rs");

    // Embed the current git SHA so the running binary can report exactly which
    // commit it was built from. Surfaced via the runner /health endpoint and
    // the supervisor's spawn-test response — lets manual-test sessions assert
    // "this temp runner is the commit I'm debugging" without guessing from
    // binary mtime.
    let git_sha_short = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=QONTINUI_GIT_SHA={}", git_sha_short);

    // RUNNER_BUILD_ID — the SW-cache-invalidation signal. Format
    // `<git-sha-short>-<unix-ms>`, mirroring the build-id used by the
    // supervisor and qontinui-web. Read at runtime by the `get_build_id`
    // Tauri command and compared by the React refresh banner against the
    // value baked into index.html at Vite-build time. The Vite side computes
    // the same format independently — they don't have to match to the
    // millisecond, only differ across builds (Vite reruns first, so they
    // diverge on every rebuild, which is the whole point).
    //
    // Use a 7-char short SHA (vs the 12 above) to stay aligned with what the
    // Vite plugin emits (`git rev-parse --short HEAD` defaults to 7).
    let git_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());
    let unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    println!("cargo:rustc-env=RUNNER_BUILD_ID={}-{}", git_sha, unix_ms);

    // Re-run this script when HEAD moves. qontinui-runner's .git lives at
    // ../.git relative to src-tauri.
    //   - .git/HEAD fires on branch switch / detached-head jumps.
    //   - .git/refs/heads/ (directory) fires on any new commit to any local
    //     branch, since refs/heads/<branch> is the file git updates when
    //     advancing a branch ref. Without this a fresh commit on the
    //     currently-checked-out branch would keep the old QONTINUI_GIT_SHA
    //     embedded, defeating the purpose of this stamp.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");

    tauri_build::build()
}
