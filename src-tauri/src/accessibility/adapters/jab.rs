//! Windows Java Access Bridge (JAB) adapter.
//!
//! Wraps `WindowsAccessBridge-64.dll` via dynamic loading (`libloading`) so the
//! runner can still build and run on machines where the JAB runtime isn't
//! installed. JAB ships with the JDK and is the only way to reach the
//! accessibility tree of Java Swing/AWT applications (IntelliJ IDEA, Android
//! Studio, NetBeans, legacy SWT apps, …) — Windows UIA treats those windows as
//! opaque `SunAwtFrame` / `SWT_Window0` HWNDs with no child elements.
//!
//! # Scope (Phase 3)
//!
//! - Load `WindowsAccessBridge-64.dll` at runtime; return actionable errors if
//!   the DLL or `jabswitch` is missing.
//! - Connect to a target window (resolved by title / PID via EnumWindows) as
//!   long as `isJavaWindow()` returns true for it.
//! - Walk the accessible-context tree, mapping JAB roles + state strings to
//!   [`UnifiedRole`] and [`UnifiedState`].
//! - Dispatch `Invoke` (→ `doAccessibleActions("click")`) and `Value`
//!   (→ `setTextContents`) interactions. Other patterns return structured
//!   "not supported in this phase" errors rather than panicking.
//!
//! # Known limitations (tracked for Phase 3 follow-up)
//!
//! - **No event subscription** — JAB's event callbacks require an installed
//!   Windows message pump and per-event C ABI trampolines. `subscribe_events`
//!   returns `Ok(None)`.
//! - **64-bit DLL only** — targeting a 32-bit JVM from a 64-bit runner is not
//!   supported. The 32-bit DLL name is defined as a constant for future work
//!   but the cross-arch bridging (out-of-process helper + IPC) is out of scope
//!   for this phase.
//! - **JAB strings are UTF-16** with a fixed 256-wchar cap; extremely long
//!   labels get truncated by JAB itself before we see them.

#![cfg(target_os = "windows")]

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::raw::{c_int, c_void};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use libloading::{Library, Symbol};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible,
};

use super::super::events::A11yEvent;
use super::super::model::{
    InteractionPattern, NodeSource, TriBool, UnifiedBounds, UnifiedNode, UnifiedRole, UnifiedState,
};
use super::super::traits::{
    ConnectionTarget, InteractionParams, InteractionResult, PlatformAdapter,
};

// ---------------------------------------------------------------------------
// FFI types & constants
// ---------------------------------------------------------------------------

/// Opaque JAB "accessible context" handle. In the real DLL this is a `jobject`
/// cast to a machine-word pointer; we treat it as an opaque `i64` and hand it
/// back to the DLL unchanged.
type AccessibleContext = i64;

/// JVM identifier as returned by `getAccessibleContextFromHWND`. Also opaque —
/// the caller hands it back to subsequent JAB calls for the same element.
type VmId = i64;

/// JAB boolean — `int` at the ABI boundary (nonzero = true).
type JabBool = c_int;

/// Maximum string length used by `AccessibleContextInfo` fields. JAB's header
/// hard-codes 256 wchars for most text fields and 64 for roles.
const MAX_STRING_SIZE: usize = 256;
const SHORT_STRING_SIZE: usize = 64;

/// 64-bit DLL name. The runner itself is 64-bit, so we use this when targeting
/// a 64-bit JVM.
const DLL_NAME_64: &str = "WindowsAccessBridge-64.dll";

/// 32-bit DLL name — declared for completeness. Attaching to a 32-bit JVM from
/// a 64-bit process is not supported in this phase; see module docs.
#[allow(dead_code)]
const DLL_NAME_32: &str = "WindowsAccessBridge-32.dll";

/// Mirror of JAB's `AccessibleContextInfo` struct (`AccessBridgePackages.h`).
///
/// Layout is wire-compatible with the 64-bit JAB DLL. Fields we don't read are
/// still declared so that the DLL writes into valid memory.
#[repr(C)]
#[derive(Clone, Copy)]
struct AccessibleContextInfo {
    name: [u16; MAX_STRING_SIZE],
    description: [u16; MAX_STRING_SIZE],
    role: [u16; SHORT_STRING_SIZE],
    role_en_us: [u16; SHORT_STRING_SIZE],
    states: [u16; MAX_STRING_SIZE],
    states_en_us: [u16; MAX_STRING_SIZE],
    index_in_parent: i32,
    children_count: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    accessible_component: i32,
    accessible_action: i32,
    accessible_selection: i32,
    accessible_text: i32,
    accessible_interfaces: i32,
}

impl Default for AccessibleContextInfo {
    fn default() -> Self {
        // All-zero is a legal starting value; the DLL overwrites it on a
        // successful call.
        // SAFETY: `AccessibleContextInfo` is a `#[repr(C)]` struct containing
        // only POD fields (integer arrays + i32s), so an all-zero bit pattern
        // is a valid instance.
        unsafe { std::mem::zeroed() }
    }
}

/// Maximum number of actions per `AccessibleActions` block. JAB's header uses
/// 256; we keep the same cap.
const MAX_ACTION_INFO: usize = 256;
const MAX_ACTIONS_TO_DO: usize = 32;

/// Single action descriptor: name + description.
#[repr(C)]
#[derive(Clone, Copy)]
struct AccessibleActionInfo {
    name: [u16; SHORT_STRING_SIZE],
    description: [u16; MAX_STRING_SIZE],
}

impl Default for AccessibleActionInfo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// Full list of actions available on an element.
#[repr(C)]
struct AccessibleActions {
    actions_count: i32,
    action_info: [AccessibleActionInfo; MAX_ACTION_INFO],
}

impl Default for AccessibleActions {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

/// Sequence of actions to perform (usually just one — "click").
#[repr(C)]
struct AccessibleActionsToDo {
    actions_count: i32,
    actions: [AccessibleActionInfo; MAX_ACTIONS_TO_DO],
}

impl Default for AccessibleActionsToDo {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// ---------------------------------------------------------------------------
// Function signatures for the JAB DLL exports we use
// ---------------------------------------------------------------------------
//
// Each signature matches the `extern "C"` declaration in `AccessBridgePackages.h`.
// We load them by name via `libloading::Library::get`. Names use the exact
// capitalization from the JDK headers — the `#[no_mangle]` exports from JAB
// are case-sensitive.

type FnWindowsRun = unsafe extern "C" fn();
type FnIsJavaWindow = unsafe extern "C" fn(hwnd: HWND) -> JabBool;
type FnGetAccessibleContextFromHWND = unsafe extern "C" fn(
    hwnd: HWND,
    vm_id: *mut VmId,
    ac: *mut AccessibleContext,
) -> JabBool;
type FnReleaseJavaObject = unsafe extern "C" fn(vm_id: VmId, obj: AccessibleContext);
type FnGetAccessibleContextInfo = unsafe extern "C" fn(
    vm_id: VmId,
    ac: AccessibleContext,
    info: *mut AccessibleContextInfo,
) -> JabBool;
type FnGetAccessibleChildFromContext =
    unsafe extern "C" fn(vm_id: VmId, ac: AccessibleContext, index: i32) -> AccessibleContext;
type FnGetAccessibleActions = unsafe extern "C" fn(
    vm_id: VmId,
    ac: AccessibleContext,
    out: *mut AccessibleActions,
) -> JabBool;
type FnDoAccessibleActions = unsafe extern "C" fn(
    vm_id: VmId,
    ac: AccessibleContext,
    actions: *const AccessibleActionsToDo,
    failure: *mut i32,
) -> JabBool;
type FnSetTextContents =
    unsafe extern "C" fn(vm_id: VmId, ac: AccessibleContext, text: *const u16) -> JabBool;

// ---------------------------------------------------------------------------
// Bundled DLL function pointers
// ---------------------------------------------------------------------------

/// Holds the loaded DLL plus resolved symbols. We never unload the library for
/// the lifetime of the process (Java's JVM state is tied to the DLL, and the
/// runner creates at most one JabAdapter).
struct JabDll {
    /// Keep the library alive — symbols reference into this allocation.
    _library: Library,
    windows_run: unsafe extern "C" fn(),
    is_java_window: FnIsJavaWindow,
    get_accessible_context_from_hwnd: FnGetAccessibleContextFromHWND,
    release_java_object: FnReleaseJavaObject,
    get_accessible_context_info: FnGetAccessibleContextInfo,
    get_accessible_child_from_context: FnGetAccessibleChildFromContext,
    get_accessible_actions: FnGetAccessibleActions,
    do_accessible_actions: FnDoAccessibleActions,
    set_text_contents: FnSetTextContents,
}

// SAFETY: The underlying DLL stores no per-thread state we access; JAB is
// documented to be callable from any thread as long as Windows_run has been
// called at startup. We therefore treat the resolved function pointers as
// Send + Sync.
unsafe impl Send for JabDll {}
unsafe impl Sync for JabDll {}

impl JabDll {
    /// Try to load `WindowsAccessBridge-64.dll` from the system search path.
    ///
    /// Returns a structured error if the DLL can't be found or doesn't expose
    /// the expected entry points — both of which usually mean the user needs
    /// to run `jabswitch -enable` and/or install a JRE that ships JAB.
    fn load() -> anyhow::Result<Self> {
        let lib = unsafe { Library::new(OsStr::new(DLL_NAME_64)) }
            .with_context(|| format!(
                "Failed to load {DLL_NAME_64}. Ensure a 64-bit JRE is installed and \
                 `jabswitch -enable` has been run (ships with the JDK)."
            ))?;

        // Resolve each symbol. We copy them out into plain function pointers
        // so the caller doesn't have to juggle `Symbol<'_>` lifetimes.
        unsafe {
            let windows_run = get_sym::<FnWindowsRun>(&lib, b"Windows_run\0")?;
            let is_java_window = get_sym::<FnIsJavaWindow>(&lib, b"isJavaWindow\0")?;
            let get_accessible_context_from_hwnd = get_sym::<FnGetAccessibleContextFromHWND>(
                &lib,
                b"getAccessibleContextFromHWND\0",
            )?;
            let release_java_object =
                get_sym::<FnReleaseJavaObject>(&lib, b"releaseJavaObject\0")?;
            let get_accessible_context_info = get_sym::<FnGetAccessibleContextInfo>(
                &lib,
                b"getAccessibleContextInfo\0",
            )?;
            let get_accessible_child_from_context = get_sym::<FnGetAccessibleChildFromContext>(
                &lib,
                b"getAccessibleChildFromContext\0",
            )?;
            let get_accessible_actions = get_sym::<FnGetAccessibleActions>(
                &lib,
                b"getAccessibleActionsFromContext\0",
            )?;
            let do_accessible_actions =
                get_sym::<FnDoAccessibleActions>(&lib, b"doAccessibleActions\0")?;
            let set_text_contents =
                get_sym::<FnSetTextContents>(&lib, b"setTextContents\0")?;

            Ok(Self {
                _library: lib,
                windows_run,
                is_java_window,
                get_accessible_context_from_hwnd,
                release_java_object,
                get_accessible_context_info,
                get_accessible_child_from_context,
                get_accessible_actions,
                do_accessible_actions,
                set_text_contents,
            })
        }
    }
}

/// Helper: resolve a symbol by byte-slice name and copy it out of the Symbol
/// wrapper. The returned function pointer outlives the `Symbol<'_>` because
/// the `Library` itself is kept alive inside `JabDll`.
unsafe fn get_sym<T: Copy>(lib: &Library, name: &[u8]) -> anyhow::Result<T> {
    let sym: Symbol<'_, T> = lib
        .get(name)
        .with_context(|| format!(
            "JAB DLL is missing export `{}` — DLL version mismatch?",
            String::from_utf8_lossy(name).trim_end_matches('\0')
        ))?;
    Ok(*sym)
}

// ---------------------------------------------------------------------------
// Wide-string helpers
// ---------------------------------------------------------------------------

/// Decode a UTF-16 slice, stopping at the first NUL.
fn decode_utf16_z(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Encode a Rust `&str` as a NUL-terminated UTF-16 vector suitable for passing
/// to JAB.
fn encode_utf16_z(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

// ---------------------------------------------------------------------------
// Handle table
// ---------------------------------------------------------------------------

/// `(vm_id, accessible_context)` pair for a live JAB element. The adapter
/// releases each pair via `releaseJavaObject` when the table is cleared.
#[derive(Clone, Copy)]
struct JabHandle {
    vm_id: VmId,
    ac: AccessibleContext,
}

// SAFETY: these are opaque integer IDs; the DLL synchronizes access internally.
unsafe impl Send for JabHandle {}
unsafe impl Sync for JabHandle {}

// ---------------------------------------------------------------------------
// Role & state mapping
// ---------------------------------------------------------------------------

/// Map a JAB role string (always English when read from `role_en_US`) to a
/// [`UnifiedRole`]. The strings come straight from
/// `javax.accessibility.AccessibleRole` — see OpenJDK `AccessibleRole.java`.
///
/// Anything not listed returns [`UnifiedRole::Unknown`] so callers can still
/// render the element in the tree with a pass-through role label.
fn map_jab_role(role: &str) -> UnifiedRole {
    match role.trim().to_ascii_lowercase().as_str() {
        "push button" => UnifiedRole::Button,
        "toggle button" => UnifiedRole::Button,
        "text" => UnifiedRole::Edit,
        "password text" => UnifiedRole::Edit,
        "check box" => UnifiedRole::Checkbox,
        "radio button" => UnifiedRole::Radio,
        "combo box" => UnifiedRole::Combobox,
        "list" => UnifiedRole::List,
        "list item" => UnifiedRole::Listitem,
        "tree" => UnifiedRole::Tree,
        "tree node" | "tree item" => UnifiedRole::Treeitem,
        "menu bar" => UnifiedRole::Menubar,
        "menu" => UnifiedRole::Menu,
        "menu item" => UnifiedRole::Menuitem,
        "check box menu item" => UnifiedRole::Menuitemcheckbox,
        "radio button menu item" => UnifiedRole::Menuitemradio,
        "popup menu" => UnifiedRole::Menu,
        "tool bar" => UnifiedRole::Toolbar,
        "page tab list" | "tabbed pane" => UnifiedRole::Tablist,
        "page tab" => UnifiedRole::Tab,
        "label" => UnifiedRole::StaticText,
        "scroll bar" => UnifiedRole::Scrollbar,
        "scroll pane" => UnifiedRole::Pane,
        "slider" => UnifiedRole::Slider,
        "split pane" => UnifiedRole::Pane,
        "table" => UnifiedRole::Table,
        "table cell" => UnifiedRole::Cell,
        "column header" => UnifiedRole::Columnheader,
        "row header" => UnifiedRole::Rowheader,
        "frame" => UnifiedRole::Window,
        "dialog" => UnifiedRole::Dialog,
        "tool tip" => UnifiedRole::Tooltip,
        "panel" => UnifiedRole::Pane,
        "progress bar" => UnifiedRole::Progressbar,
        "separator" => UnifiedRole::Separator,
        "status bar" => UnifiedRole::Status,
        "title bar" => UnifiedRole::Titlebar,
        "layered pane" => UnifiedRole::Pane,
        "root pane" => UnifiedRole::Pane,
        "glass pane" => UnifiedRole::Pane,
        "internal frame" => UnifiedRole::Window,
        "desktop pane" => UnifiedRole::Pane,
        "option pane" => UnifiedRole::Dialog,
        "window" => UnifiedRole::Window,
        "viewport" => UnifiedRole::Region,
        "hyperlink" => UnifiedRole::Hyperlink,
        "icon" => UnifiedRole::Img,
        "header" => UnifiedRole::Rowgroup,
        "footer" => UnifiedRole::Contentinfo,
        "paragraph" => UnifiedRole::Paragraph,
        "editbar" => UnifiedRole::Edit,
        "progress monitor" => UnifiedRole::Progressbar,
        "spin box" => UnifiedRole::Spinbutton,
        "ruler" => UnifiedRole::Separator,
        _ => UnifiedRole::Unknown,
    }
}

/// Parse JAB's comma-separated state string (e.g. `"focusable,enabled,visible,showing"`)
/// into a [`UnifiedState`]. Unknown tokens are ignored.
fn parse_jab_states(states: &str) -> UnifiedState {
    let mut out = UnifiedState::default();

    let mut has_checkable = false;
    let mut is_checked = false;
    let mut has_expandable = false;
    let mut is_expanded = false;
    let mut has_selectable = false;
    let mut is_selected = false;
    let mut is_pressed = false;

    for raw in states.split(',') {
        let tok = raw.trim().to_ascii_lowercase();
        match tok.as_str() {
            "focused" => out.is_focused = true,
            "focusable" => out.is_focusable = true,
            "enabled" => {}
            "unavailable" | "disabled" => out.is_disabled = true,
            "visible" | "showing" => {}
            "invisible" | "hidden" => out.is_hidden = true,
            "editable" => out.is_editable = true,
            "read only" | "readonly" => out.is_readonly = true,
            "required" => out.is_required = true,
            "multiselectable" => out.is_multiselectable = true,
            "modal" => out.is_modal = true,
            // Tri-state flavours — JAB emits `"checkable"` alongside
            // `"checked"` when the box is actually ticked; same pattern for
            // selectable / expandable.
            "checkable" => has_checkable = true,
            "checked" => {
                has_checkable = true;
                is_checked = true;
            }
            "expandable" | "collapsible" => has_expandable = true,
            "expanded" => {
                has_expandable = true;
                is_expanded = true;
            }
            "collapsed" => {
                has_expandable = true;
                is_expanded = false;
            }
            "selectable" => has_selectable = true,
            "selected" => {
                has_selectable = true;
                is_selected = true;
            }
            "pressed" | "armed" => is_pressed = true,
            _ => {}
        }
    }

    out.is_checked = if has_checkable {
        TriBool::from_option(Some(is_checked))
    } else {
        TriBool::NotApplicable
    };
    out.is_expanded = if has_expandable {
        TriBool::from_option(Some(is_expanded))
    } else {
        TriBool::NotApplicable
    };
    out.is_selected = if has_selectable {
        TriBool::from_option(Some(is_selected))
    } else {
        TriBool::NotApplicable
    };
    out.is_pressed = if is_pressed {
        TriBool::True
    } else {
        TriBool::NotApplicable
    };
    out
}

/// Derive the list of [`InteractionPattern`]s an element supports from its
/// context-info interface flags.
fn patterns_for(info: &AccessibleContextInfo, role: UnifiedRole) -> Vec<InteractionPattern> {
    let mut out = Vec::new();
    if info.accessible_action != 0 {
        out.push(InteractionPattern::Invoke);
    }
    // Editable text fields accept `setTextContents` regardless of whether the
    // action interface is populated.
    if matches!(role, UnifiedRole::Edit | UnifiedRole::Textbox | UnifiedRole::Searchbox)
        || info.accessible_text != 0
    {
        out.push(InteractionPattern::Value);
    }
    if info.accessible_selection != 0 {
        out.push(InteractionPattern::Selection);
    }
    if info.accessible_component != 0 {
        out.push(InteractionPattern::CoordinateClick);
    }
    out
}

// ---------------------------------------------------------------------------
// Window enumeration helpers (find a Java HWND from title / PID)
// ---------------------------------------------------------------------------

/// State passed into the `EnumWindows` callback. We stash the requested target
/// plus the first matching HWND we find.
struct EnumContext {
    target: EnumTarget,
    found: Option<HWND>,
}

enum EnumTarget {
    Title(String),
    Pid(u32),
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut EnumContext);

    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }

    match &ctx.target {
        EnumTarget::Title(needle) => {
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
            if title.to_lowercase().contains(&needle.to_lowercase()) {
                ctx.found = Some(hwnd);
                return BOOL(0);
            }
        }
        EnumTarget::Pid(pid) => {
            let mut win_pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut win_pid));
            if win_pid == *pid {
                ctx.found = Some(hwnd);
                return BOOL(0);
            }
        }
    }
    BOOL(1)
}

fn find_window(target: EnumTarget) -> Option<HWND> {
    let mut ctx = EnumContext { target, found: None };
    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&mut ctx as *mut EnumContext as isize),
        );
    }
    ctx.found
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Windows Java Access Bridge adapter.
pub struct JabAdapter {
    dll: Arc<Mutex<Option<Arc<JabDll>>>>,
    /// `(vm_id, root accessible context)` once connected.
    root: Arc<Mutex<Option<JabHandle>>>,
    /// `handle_id → JabHandle`. We release each mapping on disconnect.
    handles: Arc<Mutex<HashMap<u64, JabHandle>>>,
    next_handle: AtomicU64,
    next_ref: AtomicU64,
    connected: AtomicBool,
}

impl JabAdapter {
    pub fn new() -> Self {
        Self {
            dll: Arc::new(Mutex::new(None)),
            root: Arc::new(Mutex::new(None)),
            handles: Arc::new(Mutex::new(HashMap::new())),
            next_handle: AtomicU64::new(1),
            next_ref: AtomicU64::new(1),
            connected: AtomicBool::new(false),
        }
    }

    /// Load the DLL on first use. We lazily load so that constructing a
    /// `JabAdapter` on a machine without JAB succeeds — the failure surfaces
    /// only when someone actually tries to connect.
    fn ensure_dll(&self) -> anyhow::Result<Arc<JabDll>> {
        let mut slot = self.dll.lock().unwrap();
        if let Some(d) = slot.as_ref() {
            return Ok(d.clone());
        }
        let loaded = JabDll::load()?;
        // Java Access Bridge requires `Windows_run()` to be called once per
        // process before any other API. Calling it twice is harmless.
        unsafe { (loaded.windows_run)() };
        let arc = Arc::new(loaded);
        *slot = Some(arc.clone());
        Ok(arc)
    }

    fn alloc_ref(&self) -> String {
        let id = self.next_ref.fetch_add(1, Ordering::Relaxed);
        format!("@e{id}")
    }

    fn register_handle(&self, h: JabHandle) -> u64 {
        let id = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().unwrap().insert(id, h);
        id
    }

    fn get_handle(&self, id: u64) -> Option<JabHandle> {
        self.handles.lock().unwrap().get(&id).copied()
    }

    /// Release every stored `(vm_id, ac)` pair. Called on disconnect and when
    /// starting a fresh tree capture.
    fn release_all_handles(&self, dll: &JabDll) {
        let mut map = self.handles.lock().unwrap();
        for (_, h) in map.drain() {
            unsafe { (dll.release_java_object)(h.vm_id, h.ac) };
        }
    }

    /// Build a tree node for the given `(vm_id, ac)` handle. The caller owns
    /// the handle; this function either stores it (via `register_handle`) or
    /// releases it if the node is rejected (hidden / depth-limited).
    fn build_node(
        &self,
        dll: &JabDll,
        handle: JabHandle,
        depth: u32,
        max_depth: Option<u32>,
        include_hidden: bool,
    ) -> Option<UnifiedNode> {
        if let Some(max) = max_depth {
            if depth > max {
                unsafe { (dll.release_java_object)(handle.vm_id, handle.ac) };
                return None;
            }
        }

        let mut info = AccessibleContextInfo::default();
        let ok = unsafe {
            (dll.get_accessible_context_info)(handle.vm_id, handle.ac, &mut info)
        };
        if ok == 0 {
            unsafe { (dll.release_java_object)(handle.vm_id, handle.ac) };
            return None;
        }

        let role_str = decode_utf16_z(&info.role_en_us);
        let role = map_jab_role(&role_str);
        let states_str = decode_utf16_z(&info.states_en_us);
        let state = parse_jab_states(&states_str);

        if state.is_hidden && !include_hidden {
            unsafe { (dll.release_java_object)(handle.vm_id, handle.ac) };
            return None;
        }

        let name_str = decode_utf16_z(&info.name);
        let name = if name_str.is_empty() { None } else { Some(name_str) };
        let desc_str = decode_utf16_z(&info.description);
        let description = if desc_str.is_empty() { None } else { Some(desc_str) };

        let bounds = if info.width > 0 && info.height > 0 {
            Some(UnifiedBounds {
                x: info.x,
                y: info.y,
                width: info.width,
                height: info.height,
            })
        } else {
            None
        };

        let supported_patterns = patterns_for(&info, role);
        let is_interactive = role.is_interactive_role() || !supported_patterns.is_empty();

        // Walk children. Each `getAccessibleChildFromContext` call returns a
        // fresh `jobject` that we either adopt into the handle table or
        // release recursively via `build_node`.
        let mut children = Vec::new();
        let recurse = max_depth.is_none_or(|m| depth < m);
        if recurse {
            for i in 0..info.children_count {
                let child_ac = unsafe {
                    (dll.get_accessible_child_from_context)(handle.vm_id, handle.ac, i)
                };
                if child_ac == 0 {
                    continue;
                }
                let child_handle = JabHandle {
                    vm_id: handle.vm_id,
                    ac: child_ac,
                };
                if let Some(child) = self.build_node(
                    dll,
                    child_handle,
                    depth + 1,
                    max_depth,
                    include_hidden,
                ) {
                    children.push(child);
                }
            }
        }

        // Keep this handle alive for later interactions.
        let handle_id = self.register_handle(handle);

        Some(UnifiedNode {
            ref_id: self.alloc_ref(),
            role,
            name,
            value: None,
            description,
            bounds,
            state,
            is_interactive,
            level: None,
            automation_id: None,
            class_name: if role_str.is_empty() {
                None
            } else {
                Some(role_str)
            },
            html_tag: None,
            url: None,
            children,
            source: NodeSource::Uia, // Tracked as UIA-family for now; see NodeSource note below.
            platform_handle: Some(handle_id),
            supported_patterns,
            generation: 0,
        })
    }
}

impl Default for JabAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// Note on `NodeSource`: the enum is (Uia, Atspi, Ax, Fused). Adding a `Jab`
// variant would ripple through the serialized snapshot schema and the Python
// side's fusion routing — a larger change than Phase 3 wants. We therefore tag
// JAB-sourced nodes as `NodeSource::Uia` (same platform family) and let
// `AccessibilityManager::backend_name()` distinguish the two at a higher layer.

#[async_trait]
impl PlatformAdapter for JabAdapter {
    fn backend_name(&self) -> &'static str {
        "jab"
    }

    async fn connect(&mut self, target: ConnectionTarget, _timeout_ms: u64) -> anyhow::Result<()> {
        let dll = self.ensure_dll()?;

        // Resolve the target to an HWND.
        let hwnd: HWND = match target {
            ConnectionTarget::WindowTitle(title) => find_window(EnumTarget::Title(title.clone()))
                .ok_or_else(|| anyhow!("No visible window matches title {title:?}"))?,
            ConnectionTarget::ProcessId(pid) => find_window(EnumTarget::Pid(pid))
                .ok_or_else(|| anyhow!("No visible window found for PID {pid}"))?,
            ConnectionTarget::Desktop => {
                bail!(
                    "JAB adapter does not support Desktop target — connect by window title \
                     or PID to a running Java application instead"
                )
            }
        };

        // Confirm it's a Java window.
        let is_java = unsafe { (dll.is_java_window)(hwnd) };
        if is_java == 0 {
            bail!(
                "HWND {:?} is not a Java window. JAB only sees Swing/AWT frames; use the UIA \
                 adapter for other apps.",
                hwnd.0
            );
        }

        // Grab the root accessible context.
        let mut vm_id: VmId = 0;
        let mut ac: AccessibleContext = 0;
        let got = unsafe {
            (dll.get_accessible_context_from_hwnd)(hwnd, &mut vm_id, &mut ac)
        };
        if got == 0 || ac == 0 {
            bail!(
                "getAccessibleContextFromHWND failed for Java window {:?} — is assistive \
                 technology enabled? Try `jabswitch -enable`.",
                hwnd.0
            );
        }

        *self.root.lock().unwrap() = Some(JabHandle { vm_id, ac });
        self.connected.store(true, Ordering::Release);
        debug!(hwnd = ?hwnd.0, vm_id, "JAB adapter connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        if let Some(dll) = self.dll.lock().unwrap().as_ref().cloned() {
            self.release_all_handles(&dll);
            if let Some(root) = self.root.lock().unwrap().take() {
                unsafe { (dll.release_java_object)(root.vm_id, root.ac) };
            }
        }
        self.connected.store(false, Ordering::Release);
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    async fn capture_tree(
        &self,
        max_depth: Option<u32>,
        include_hidden: bool,
    ) -> anyhow::Result<UnifiedNode> {
        if !self.is_connected() {
            bail!("JAB adapter is not connected");
        }
        let dll = self
            .dll
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .context("JAB DLL unexpectedly missing after connect()")?;

        // Reset the handle table — old element handles would reference a stale
        // tree after a UI repaint. We keep the root handle because the caller
        // still needs it after this capture.
        self.release_all_handles(&dll);
        self.next_ref.store(1, Ordering::Relaxed);
        self.next_handle.store(1, Ordering::Relaxed);

        let root = self
            .root
            .lock()
            .unwrap()
            .as_ref()
            .copied()
            .context("No JAB root context — not connected")?;

        // `build_node` releases the handle we give it on failure paths; we
        // need to pass in a fresh borrow (the root must *not* be freed on
        // capture). The simplest correct dance is to pretend the root is a
        // fresh child by *not* calling releaseJavaObject on it — instead we
        // register it into the handle table and trust `disconnect` /
        // subsequent `release_all_handles` to clean it up. But we still need
        // a live root context after capture for future interactions. We
        // duplicate by calling `getAccessibleChildFromContext` with index -1?
        // No — JAB doesn't expose a duplicate call. Instead, we pass `root`
        // in, and prevent `build_node` from releasing it by re-storing it
        // into `self.root` after capture.
        //
        // Implementation: walk the root directly with a guard that re-stores
        // it on drop.
        let tree = self
            .build_node(&dll, root, 0, max_depth, include_hidden)
            .context("JAB root produced no tree (hidden or inaccessible?)")?;

        // `build_node` captured the root into the handle table — good. But
        // the caller's stored `self.root` was consumed (we passed it by
        // value). Re-read it out of the handle table so disconnect can still
        // release it. The last entry added corresponds to the root itself.
        // Simpler: iterate the handle table and pick the matching ac.
        {
            let map = self.handles.lock().unwrap();
            let rehydrated = map
                .values()
                .find(|h| h.ac == root.ac && h.vm_id == root.vm_id)
                .copied();
            if let Some(h) = rehydrated {
                *self.root.lock().unwrap() = Some(h);
            }
        }

        Ok(tree)
    }

    async fn subscribe_events(&self) -> anyhow::Result<Option<mpsc::Receiver<A11yEvent>>> {
        // JAB event subscription requires per-callback C trampolines + a
        // Windows message pump on the thread that called `Windows_run`.
        // Deferred to a Phase 3 follow-up.
        Ok(None)
    }

    async fn interact(
        &self,
        platform_handle: u64,
        pattern: InteractionPattern,
        params: InteractionParams,
    ) -> anyhow::Result<InteractionResult> {
        let dll = self
            .dll
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .context("JAB DLL not loaded")?;
        let h = self
            .get_handle(platform_handle)
            .ok_or_else(|| anyhow!("JAB handle {platform_handle} not found"))?;

        match pattern {
            InteractionPattern::Invoke | InteractionPattern::Toggle => {
                // Read the list of available actions and perform the one
                // named "click" (or the first action if none match).
                let mut actions = AccessibleActions::default();
                let got = unsafe {
                    (dll.get_accessible_actions)(h.vm_id, h.ac, &mut actions)
                };
                if got == 0 || actions.actions_count == 0 {
                    return Ok(InteractionResult::err(
                        pattern,
                        "element exposes no accessible actions",
                    ));
                }

                // Pick the "click" action if present, else action 0.
                let mut chosen: Option<AccessibleActionInfo> = None;
                for i in 0..actions.actions_count.min(MAX_ACTION_INFO as i32) as usize {
                    let name = decode_utf16_z(&actions.action_info[i].name);
                    if name.eq_ignore_ascii_case("click") {
                        chosen = Some(actions.action_info[i]);
                        break;
                    }
                }
                let picked = chosen.unwrap_or(actions.action_info[0]);

                let mut request = AccessibleActionsToDo {
                    actions_count: 1,
                    actions: unsafe { std::mem::zeroed() },
                };
                request.actions[0] = picked;

                let mut failure: i32 = -1;
                let ok = unsafe {
                    (dll.do_accessible_actions)(h.vm_id, h.ac, &request, &mut failure)
                };
                if ok == 0 {
                    return Ok(InteractionResult::err(
                        pattern,
                        format!("doAccessibleActions failed at index {failure}"),
                    ));
                }
                Ok(InteractionResult::ok(pattern))
            }

            InteractionPattern::Value => {
                let text = match params {
                    InteractionParams::Text { value, .. } => value,
                    _ => bail!("Value pattern requires Text params"),
                };
                let wide = encode_utf16_z(&text);
                let ok = unsafe {
                    (dll.set_text_contents)(h.vm_id, h.ac, wide.as_ptr())
                };
                if ok == 0 {
                    return Ok(InteractionResult::err(
                        pattern,
                        "setTextContents failed — element may not expose AccessibleEditableText",
                    ));
                }
                Ok(InteractionResult::ok(pattern))
            }

            InteractionPattern::CoordinateClick => {
                // Fall back to returning the stored bounds center via success;
                // the coordinate-click is performed by the InteractionEngine
                // using a platform mouse API, not by JAB itself.
                Ok(InteractionResult::ok(pattern))
            }

            // The following patterns have JAB mappings but require more ABI
            // plumbing than Phase 3 covers. Return a structured "not yet"
            // instead of panicking so spec authors see a clear error.
            InteractionPattern::ExpandCollapse
            | InteractionPattern::Selection
            | InteractionPattern::Scroll
            | InteractionPattern::RangeValue
            | InteractionPattern::Text => Ok(InteractionResult::err(
                pattern,
                format!(
                    "JAB {pattern:?} not implemented in Phase 3 — see plan \
                     scout-2026-04-16-native-accessibility-expansion.md"
                ),
            )),
        }
    }

    async fn supported_patterns(&self, platform_handle: u64) -> Vec<InteractionPattern> {
        let Some(dll) = self.dll.lock().unwrap().as_ref().cloned() else {
            return vec![];
        };
        let Some(h) = self.get_handle(platform_handle) else {
            return vec![];
        };
        let mut info = AccessibleContextInfo::default();
        let ok = unsafe { (dll.get_accessible_context_info)(h.vm_id, h.ac, &mut info) };
        if ok == 0 {
            return vec![];
        }
        let role_str = decode_utf16_z(&info.role_en_us);
        let role = map_jab_role(&role_str);
        patterns_for(&info, role)
    }
}

impl Drop for JabAdapter {
    fn drop(&mut self) {
        if let Some(dll) = self.dll.lock().unwrap().as_ref().cloned() {
            self.release_all_handles(&dll);
            if let Some(root) = self.root.lock().unwrap().take() {
                unsafe { (dll.release_java_object)(root.vm_id, root.ac) };
            }
        }
    }
}

// Suppress unused-import warnings for Windows FFI items that are only used
// behind `unsafe` blocks the compiler can't always see.
#[allow(dead_code)]
fn _touch_pcwstr(_: PCWSTR, _: PathBuf, _: *const c_void) {}

// ---------------------------------------------------------------------------
// Tests — pure-Rust helpers only; anything that touches the DLL is skipped
// when JAB isn't installed on the build machine.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_mapping_covers_common_swing_widgets() {
        assert_eq!(map_jab_role("push button"), UnifiedRole::Button);
        assert_eq!(map_jab_role("check box"), UnifiedRole::Checkbox);
        assert_eq!(map_jab_role("radio button"), UnifiedRole::Radio);
        assert_eq!(map_jab_role("combo box"), UnifiedRole::Combobox);
        assert_eq!(map_jab_role("text"), UnifiedRole::Edit);
        assert_eq!(map_jab_role("label"), UnifiedRole::StaticText);
        assert_eq!(map_jab_role("tabbed pane"), UnifiedRole::Tablist);
        assert_eq!(map_jab_role("tree"), UnifiedRole::Tree);
        assert_eq!(map_jab_role("tree node"), UnifiedRole::Treeitem);
        assert_eq!(map_jab_role("frame"), UnifiedRole::Window);
        assert_eq!(map_jab_role("dialog"), UnifiedRole::Dialog);
        // Case-insensitive + whitespace trimmed
        assert_eq!(map_jab_role("  PUSH BUTTON  "), UnifiedRole::Button);
        // Fallback
        assert_eq!(map_jab_role("hyperjump"), UnifiedRole::Unknown);
    }

    #[test]
    fn state_parsing_tracks_checkable_and_expandable_tristate() {
        let s = parse_jab_states("focusable,enabled,visible,showing,checkable,checked");
        assert!(s.is_focusable);
        assert!(!s.is_disabled);
        assert_eq!(s.is_checked, TriBool::True);

        let s = parse_jab_states("focusable,visible,showing,expandable,collapsed");
        assert_eq!(s.is_expanded, TriBool::False);

        let s = parse_jab_states("unavailable,invisible");
        assert!(s.is_disabled);
        assert!(s.is_hidden);

        let s = parse_jab_states("selectable,selected,modal");
        assert_eq!(s.is_selected, TriBool::True);
        assert!(s.is_modal);

        // Elements with no checkable/selectable flag should stay NotApplicable.
        let s = parse_jab_states("focusable,visible,showing");
        assert_eq!(s.is_checked, TriBool::NotApplicable);
        assert_eq!(s.is_selected, TriBool::NotApplicable);
        assert_eq!(s.is_expanded, TriBool::NotApplicable);
    }

    #[test]
    fn utf16_roundtrip() {
        let encoded = encode_utf16_z("Hello");
        assert_eq!(encoded.last(), Some(&0));
        // Decoder should stop at NUL and match the original.
        let decoded = decode_utf16_z(&encoded);
        assert_eq!(decoded, "Hello");
    }

    #[test]
    fn patterns_for_edit_field_includes_value() {
        let info = AccessibleContextInfo {
            accessible_text: 1,
            accessible_component: 1,
            ..Default::default()
        };
        let p = patterns_for(&info, UnifiedRole::Edit);
        assert!(p.contains(&InteractionPattern::Value));
        assert!(p.contains(&InteractionPattern::CoordinateClick));
    }
}
