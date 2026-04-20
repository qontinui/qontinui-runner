//! Cross-platform desktop accessibility API layer.
//!
//! Provides Rust-native accessibility tree capture, querying, and interaction
//! for Windows (UIA), Linux (AT-SPI), and macOS (AX).
//! Replaces the Python HAL accessibility backends with a faster, more capable
//! Rust implementation.
//!
//! # Architecture
//!
//! ```text
//!                     Tauri Commands / MCP API
//!                             |
//!                     [AccessibilityManager]
//!                       |           |
//!               [QueryEngine]  [InteractionEngine]
//!                       |           |
//!                   [TreeCache + EventBus]
//!                       |
//!                   [FusionEngine]
//!                       |
//!               [NativeAdapter]
//!                       |
//!             +---------+---------+
//!             |         |         |
//!           [UIA]   [AT-SPI]   [AX]
//! ```

pub mod adapters;
pub mod cache;
pub mod events;
#[cfg(test)]
mod flaui_conformance_test;
pub mod fusion;
#[cfg(test)]
mod integration_test;
pub mod interaction;
pub mod model;
pub mod query;
pub mod ref_manager;
pub mod traits;

use tracing::{debug, info, warn};

use self::adapters::create_platform_adapter;
use self::cache::TreeCache;
use self::events::{A11yEvent, EventReceiver};
use self::fusion::{FusionConfig, FusionEngine};
use self::interaction::InteractionEngine;
use self::model::{NodeSource, UnifiedNode, UnifiedSnapshot};
use self::query::QueryBuilder;
use self::ref_manager::RefManager;
use self::traits::{ConnectionTarget, InteractionParams, InteractionResult, PlatformAdapter};

#[cfg(windows)]
use self::adapters::jab::JabAdapter;

/// Top-level facade for the accessibility system.
///
/// Manages platform adapters, tree cache, ref assignment, and interaction
/// dispatch. Provides a unified API for Tauri commands and MCP tools.
pub struct AccessibilityManager {
    /// Platform-native adapter (UIA, AT-SPI, or AX depending on OS).
    native_adapter: Box<dyn PlatformAdapter>,

    /// Cached accessibility tree with handle index.
    cache: TreeCache,

    /// Ref assignment and persistence.
    ref_manager: RefManager,

    /// Fusion engine for merging trees (reserved for future use, e.g., UI Bridge data).
    fusion_engine: FusionEngine,

    /// Event sender for broadcasting accessibility events.
    event_tx: events::EventSender,
}

impl AccessibilityManager {
    /// Create a new AccessibilityManager with default configuration.
    pub fn new() -> Self {
        let (event_tx, _) = events::create_event_channel(256);
        let cache = TreeCache::new(event_tx.clone());

        let mut ref_manager = RefManager::new();
        if let Some(home) = dirs::home_dir() {
            ref_manager.set_persistence_dir(home.join(".qontinui").join("a11y_refs"));
        }

        Self {
            native_adapter: create_platform_adapter(),
            cache,
            ref_manager,
            fusion_engine: FusionEngine::new(FusionConfig::default()),
            event_tx,
        }
    }

    /// Connect to an accessibility source.
    pub async fn connect(
        &mut self,
        target: ConnectionTarget,
        timeout_ms: u64,
    ) -> anyhow::Result<()> {
        // On Windows, detect Java Swing/AWT windows (SunAwtFrame, SWT_Window0,
        // …) and route to the JAB adapter instead of UIA — UIA sees those
        // windows as opaque HWNDs with zero child elements.
        #[cfg(windows)]
        {
            if let Some(class) = win32_routing::window_class_for_target(&target) {
                if win32_routing::is_java_window_class(&class) {
                    info!(
                        window_class = %class,
                        "Detected Java window class — routing to JAB adapter"
                    );
                    let mut jab = Box::new(JabAdapter::new()) as Box<dyn PlatformAdapter>;
                    match jab.connect(target.clone(), timeout_ms).await {
                        Ok(()) => {
                            self.native_adapter = jab;
                            let _ = self.event_tx.send(A11yEvent::ConnectionChanged {
                                connected: true,
                                backend: self.native_adapter.backend_name().to_string(),
                            });
                            return Ok(());
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                "JAB connection failed for Java window — falling back to UIA. \
                                 If the tree ends up empty, run `jabswitch -enable`."
                            );
                            // fall through to UIA
                        }
                    }
                }
            }
        }

        info!(
            "Connecting to accessibility source via {}",
            self.native_adapter.backend_name()
        );
        self.native_adapter.connect(target, timeout_ms).await?;

        let _ = self.event_tx.send(A11yEvent::ConnectionChanged {
            connected: true,
            backend: self.native_adapter.backend_name().to_string(),
        });

        Ok(())
    }

    /// Disconnect from all sources.
    pub async fn disconnect(&mut self) -> anyhow::Result<()> {
        // Save refs before disconnecting
        if self.ref_manager.count() > 0 {
            let backend = self.native_adapter.backend_name();
            if let Err(e) = self.ref_manager.save(backend) {
                debug!("Failed to persist refs: {}", e);
            }
        }

        self.native_adapter.disconnect().await?;
        self.cache.clear().await;
        self.ref_manager.reset();

        let _ = self.event_tx.send(A11yEvent::ConnectionChanged {
            connected: false,
            backend: self.native_adapter.backend_name().to_string(),
        });

        info!("Disconnected from accessibility source");
        Ok(())
    }

    /// Whether connected to an accessibility source.
    pub fn is_connected(&self) -> bool {
        self.native_adapter.is_connected()
    }

    /// Capture the accessibility tree, assign refs, and update the cache.
    pub async fn capture(
        &mut self,
        max_depth: Option<u32>,
        include_hidden: bool,
    ) -> anyhow::Result<UnifiedSnapshot> {
        // Capture native tree
        let mut native_tree = self
            .native_adapter
            .capture_tree(max_depth, include_hidden)
            .await?;

        let source = self.native_adapter_source();

        // Assign refs
        self.ref_manager
            .assign_refs_to_tree(&mut native_tree, false);

        // Build snapshot
        let generation = self.cache.next_generation();
        let snapshot = UnifiedSnapshot::from_root(
            native_tree,
            source,
            None, // URL populated by adapter when available
            None, // Title populated by adapter
            generation,
        );

        // Update cache
        self.cache.replace(snapshot.clone()).await;

        let _ = self.event_tx.send(A11yEvent::TreeReplaced {
            generation,
            total_nodes: snapshot.total_nodes,
        });

        info!(
            "Captured accessibility tree: {} nodes ({} interactive), generation {}",
            snapshot.total_nodes, snapshot.interactive_nodes, generation
        );

        // On Windows: if UIA returned effectively-empty tree AND the last
        // connected HWND's process has jvm.dll loaded, emit a diagnostic
        // suggesting `jabswitch -enable`. This is a narrow diagnostic, not a
        // full resolver framework (see plan Phase 3).
        #[cfg(windows)]
        {
            if self.native_adapter.backend_name() == "uia"
                && snapshot.total_nodes <= 1
                && win32_routing::last_connected_target_is_jvm()
            {
                warn!(
                    total_nodes = snapshot.total_nodes,
                    hint = "jabswitch -enable",
                    "UIA returned empty tree for a JVM process. Java Swing/AWT apps require \
                     the Java Access Bridge. Run `jabswitch -enable` (ships with the JDK) to \
                     activate JAB, then reconnect."
                );
            }
        }

        Ok(snapshot)
    }

    /// Get the NodeSource for the native adapter.
    fn native_adapter_source(&self) -> NodeSource {
        match self.native_adapter.backend_name() {
            "uia" => NodeSource::Uia,
            // JAB is a Windows-family backend; until NodeSource grows a
            // dedicated variant we continue to tag JAB-sourced trees as Uia.
            "jab" => NodeSource::Uia,
            "atspi" => NodeSource::Atspi,
            "ax" => NodeSource::Ax,
            _ => NodeSource::Uia,
        }
    }

    /// Create a query builder for searching the cached tree.
    pub fn query(&self) -> QueryBuilder {
        QueryBuilder::new()
    }

    /// Click an element by ref ID.
    pub async fn click(&self, ref_id: &str) -> anyhow::Result<InteractionResult> {
        let node = self
            .cache
            .node_by_ref(ref_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Element not found: {}", ref_id))?;

        InteractionEngine::click(&node, self.native_adapter.as_ref()).await
    }

    /// Type text into an element by ref ID.
    pub async fn type_text(
        &self,
        ref_id: &str,
        text: &str,
        clear_first: bool,
    ) -> anyhow::Result<InteractionResult> {
        let node = self
            .cache
            .node_by_ref(ref_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Element not found: {}", ref_id))?;

        InteractionEngine::type_text(&node, text, clear_first, self.native_adapter.as_ref()).await
    }

    /// Focus an element by ref ID.
    pub async fn focus(&self, ref_id: &str) -> anyhow::Result<InteractionResult> {
        let node = self
            .cache
            .node_by_ref(ref_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Element not found: {}", ref_id))?;

        InteractionEngine::focus(&node, self.native_adapter.as_ref()).await
    }

    /// Subscribe to accessibility events.
    pub fn subscribe(&self) -> EventReceiver {
        self.event_tx.subscribe()
    }

    /// Generate an AI-friendly text representation of the current tree.
    ///
    /// Matches the output format of the Python `IAccessibilityCapture.to_ai_context()`.
    pub async fn to_ai_context(&self, max_elements: usize, interactive_only: bool) -> String {
        let snapshot = match self.cache.snapshot().await {
            Some(s) => s,
            None => return "## Accessibility Tree\n\nNo tree captured.".to_string(),
        };

        let mut lines = Vec::new();
        lines.push("## Accessibility Tree".to_string());

        if let Some(url) = &snapshot.url {
            lines.push(format!("URL: {}", url));
        }
        if let Some(title) = &snapshot.title {
            lines.push(format!("Title: {}", title));
        }

        lines.push(String::new());
        lines.push("### Interactive Elements".to_string());

        let mut count = 0;

        fn walk(
            node: &UnifiedNode,
            interactive_only: bool,
            max_elements: usize,
            count: &mut usize,
            lines: &mut Vec<String>,
        ) {
            if *count >= max_elements {
                return;
            }

            if interactive_only && !node.is_interactive {
                for child in &node.children {
                    walk(child, interactive_only, max_elements, count, lines);
                }
                return;
            }

            let name_str = node
                .name
                .as_ref()
                .map(|n| format!(" \"{}\"", n))
                .unwrap_or_default();
            let value_str = node
                .value
                .as_ref()
                .map(|v| format!(" = {}", v))
                .unwrap_or_default();

            let mut state_parts = Vec::new();
            if node.state.is_disabled {
                state_parts.push("disabled");
            }
            if node.state.is_focused {
                state_parts.push("focused");
            }
            if node.state.is_checked.is_true() {
                state_parts.push("checked");
            }
            if node.state.is_expanded.is_true() {
                state_parts.push("expanded");
            }

            let state_suffix = if state_parts.is_empty() {
                String::new()
            } else {
                format!(" [{}]", state_parts.join(", "))
            };

            lines.push(format!(
                "- {}: {}{}{}{}",
                node.ref_id,
                node.role.as_str(),
                name_str,
                value_str,
                state_suffix
            ));
            *count += 1;

            for child in &node.children {
                walk(child, interactive_only, max_elements, count, lines);
            }
        }

        walk(
            &snapshot.root,
            interactive_only,
            max_elements,
            &mut count,
            &mut lines,
        );

        if count >= max_elements {
            lines.push(format!("... (truncated, {} total)", snapshot.total_nodes));
        }

        lines.join("\n")
    }

    /// Get the current cached snapshot.
    pub async fn snapshot(&self) -> Option<UnifiedSnapshot> {
        self.cache.snapshot().await
    }

    /// Get the native adapter's backend name.
    pub fn backend_name(&self) -> &'static str {
        self.native_adapter.backend_name()
    }
}

impl Default for AccessibilityManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Windows-only routing helpers
//
// - window_class_for_target: resolve a ConnectionTarget to its HWND's class
//   name, so we can detect Swing/AWT frames and route to JAB.
// - is_java_window_class: membership test over the known Java class prefixes.
// - last_connected_target_is_jvm: EnumProcessModules-based probe for `jvm.dll`
//   in the PID behind the most recent connection. Used to emit the jabswitch
//   diagnostic from the capture path.
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win32_routing {
    use std::sync::Mutex;

    use once_cell::sync::Lazy;
    use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HMODULE, HWND, LPARAM};
    use windows::Win32::System::ProcessStatus::{EnumProcessModules, GetModuleFileNameExW};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible,
    };

    use super::ConnectionTarget;

    /// The last `(hwnd, pid)` pair `AccessibilityManager::connect` resolved.
    /// Read by `last_connected_target_is_jvm`. Stored as two primitives so the
    /// `Mutex` is trivially `Send + Sync`.
    static LAST_TARGET: Lazy<Mutex<Option<(isize, u32)>>> = Lazy::new(|| Mutex::new(None));

    /// Resolve a ConnectionTarget into the class name of its top-level HWND,
    /// if one exists. Returns None for Desktop and for titles/PIDs we can't
    /// locate.
    pub(super) fn window_class_for_target(target: &ConnectionTarget) -> Option<String> {
        let hwnd = match target {
            ConnectionTarget::Desktop => return None,
            ConnectionTarget::ProcessId(pid) => find_hwnd_by_pid(*pid)?,
            ConnectionTarget::WindowTitle(title) => find_hwnd_by_title(title)?,
        };
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        *LAST_TARGET.lock().unwrap() = Some((hwnd.0 as isize, pid));

        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        if n <= 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..n as usize]))
    }

    /// Known Java top-level window classes. These are the classes Swing/AWT
    /// and SWT emit; UIA sees them as opaque `Pane` elements with no children.
    pub(super) fn is_java_window_class(class: &str) -> bool {
        // Swing/AWT prefixes: SunAwtFrame, SunAwtDialog, SunAwtWindow,
        // SunAwtCanvas. SWT uses SWT_Window0. Eclipse JRE uses JavaEmbedded.
        class.starts_with("SunAwt") || class == "SWT_Window0" || class == "JavaEmbeddedFrame"
    }

    /// Probe the last-connected PID for `jvm.dll`. Returns `false` if we've
    /// never connected, or if the process is not a JVM, or if we can't
    /// enumerate modules (usually a permissions / architecture mismatch).
    pub(super) fn last_connected_target_is_jvm() -> bool {
        let pid = match *LAST_TARGET.lock().unwrap() {
            Some((_, pid)) => pid,
            None => return false,
        };
        process_has_jvm_dll(pid)
    }

    /// EnumProcessModules-based probe. Safe for 64-bit → 64-bit inspection;
    /// 32-bit processes opened from a 64-bit runner will return `false`
    /// silently (the module list read path requires matching bitness).
    fn process_has_jvm_dll(pid: u32) -> bool {
        let handle = unsafe {
            match OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) {
                Ok(h) => h,
                Err(_) => return false,
            }
        };
        if handle.is_invalid() {
            return false;
        }
        let result = unsafe { probe_modules(handle) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        result
    }

    unsafe fn probe_modules(process: HANDLE) -> bool {
        // First call to size the buffer.
        let mut needed: u32 = 0;
        let ok = EnumProcessModules(process, std::ptr::null_mut(), 0, &mut needed);
        if ok.is_err() || needed == 0 {
            return false;
        }
        let count = (needed as usize) / std::mem::size_of::<HMODULE>();
        if count == 0 {
            return false;
        }
        let mut modules = vec![HMODULE::default(); count];
        let ok = EnumProcessModules(process, modules.as_mut_ptr(), needed, &mut needed);
        if ok.is_err() {
            return false;
        }

        for hmod in &modules {
            let mut name_buf = [0u16; 260];
            let n = GetModuleFileNameExW(process, *hmod, &mut name_buf);
            if n == 0 {
                continue;
            }
            let path_str = String::from_utf16_lossy(&name_buf[..n as usize]);
            let lower = path_str.to_ascii_lowercase();
            if lower.ends_with("\\jvm.dll") || lower.ends_with("/jvm.dll") {
                return true;
            }
        }
        false
    }

    /// Enumerate visible top-level windows and return the first whose title
    /// contains `needle` (case-insensitive).
    fn find_hwnd_by_title(needle: &str) -> Option<HWND> {
        struct Ctx {
            needle: String,
            found: Option<HWND>,
        }
        unsafe extern "system" fn proc(hwnd: HWND, l: LPARAM) -> BOOL {
            let ctx = &mut *(l.0 as *mut Ctx);
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return BOOL(1);
            }
            let mut buf = vec![0u16; (len + 1) as usize];
            let got = GetWindowTextW(hwnd, &mut buf);
            if got <= 0 {
                return BOOL(1);
            }
            let title = String::from_utf16_lossy(&buf[..got as usize]);
            if title.to_lowercase().contains(&ctx.needle) {
                ctx.found = Some(hwnd);
                return BOOL(0);
            }
            BOOL(1)
        }
        let mut ctx = Ctx {
            needle: needle.to_lowercase(),
            found: None,
        };
        unsafe {
            let _ = EnumWindows(Some(proc), LPARAM(&mut ctx as *mut _ as isize));
        }
        ctx.found
    }

    /// Enumerate visible top-level windows and return the first one belonging
    /// to `pid`.
    fn find_hwnd_by_pid(pid: u32) -> Option<HWND> {
        struct Ctx {
            pid: u32,
            found: Option<HWND>,
        }
        unsafe extern "system" fn proc(hwnd: HWND, l: LPARAM) -> BOOL {
            let ctx = &mut *(l.0 as *mut Ctx);
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let mut win_pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut win_pid));
            if win_pid == ctx.pid {
                ctx.found = Some(hwnd);
                return BOOL(0);
            }
            BOOL(1)
        }
        let mut ctx = Ctx { pid, found: None };
        unsafe {
            let _ = EnumWindows(Some(proc), LPARAM(&mut ctx as *mut _ as isize));
        }
        ctx.found
    }
}
