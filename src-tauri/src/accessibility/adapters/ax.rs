//! macOS Accessibility (AXUIElement) adapter.
//!
//! Bridges macOS AXAccessibility API to the unified `PlatformAdapter` trait via
//! raw FFI bindings using `core-foundation` / `core-foundation-sys`. All blocking
//! AX calls are dispatched onto `tokio::task::spawn_blocking` to avoid stalling
//! the async runtime.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use anyhow::{bail, Context};
use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};

use super::super::events::A11yEvent;
use super::super::model::{
    InteractionPattern, NodeSource, UnifiedBounds, UnifiedNode, UnifiedRole, UnifiedState,
};
use super::super::traits::{
    ConnectionTarget, InteractionParams, InteractionResult, PlatformAdapter,
};

// ---------------------------------------------------------------------------
// FFI declarations
// ---------------------------------------------------------------------------

/// Opaque handle to an AXUIElement. This is a CFTypeRef under the hood.
type AXUIElementRef = *const c_void;
type AXError = i32;
type pid_t = i32;

const AX_ERROR_SUCCESS: AXError = 0;
const AX_ERROR_API_DISABLED: AXError = -25200;
const AX_ERROR_INVALID_UI_ELEMENT: AXError = -25201;
const AX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25204;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCreateApplication(pid: pid_t) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
    fn AXIsProcessTrusted() -> bool;
    fn AXValueGetValue(value: CFTypeRef, value_type: u32, value_ptr: *mut c_void) -> bool;
}

extern "C" {
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
}

/// AXValueType constants for decoding CGPoint / CGSize.
const K_AX_VALUE_TYPE_CGPOINT: u32 = 1;
const K_AX_VALUE_TYPE_CGSIZE: u32 = 2;

/// CGPoint for AXValue extraction.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

/// CGSize for AXValue extraction.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

// ---------------------------------------------------------------------------
// Send/Sync wrapper for AXUIElementRef
// ---------------------------------------------------------------------------

/// Newtype wrapper that lets us send AXUIElementRef across threads.
///
/// Apple documents that AXUIElement is thread-safe (the underlying Mach port
/// communication is process-global), so this `unsafe impl` is sound.
#[derive(Debug)]
struct SendableAXElement(AXUIElementRef);

unsafe impl Send for SendableAXElement {}
unsafe impl Sync for SendableAXElement {}

impl SendableAXElement {
    fn new(ptr: AXUIElementRef) -> Self {
        Self(ptr)
    }

    /// Retain-then-wrap so the caller owns an extra reference.
    fn retained(ptr: AXUIElementRef) -> Self {
        unsafe { CFRetain(ptr as CFTypeRef) };
        Self(ptr)
    }

    fn as_ptr(&self) -> AXUIElementRef {
        self.0
    }
}

impl Clone for SendableAXElement {
    fn clone(&self) -> Self {
        Self::retained(self.0)
    }
}

impl Drop for SendableAXElement {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0 as CFTypeRef) };
        }
    }
}

// ---------------------------------------------------------------------------
// Handle table
// ---------------------------------------------------------------------------

/// Thread-safe handle table mapping u64 ids to retained AXUIElementRefs.
#[derive(Debug)]
struct HandleTable {
    map: Mutex<HashMap<u64, SendableAXElement>>,
    next_id: AtomicU64,
}

impl HandleTable {
    fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Insert an element, returning the assigned handle id. The element is CFRetain'd.
    fn insert(&self, element: AXUIElementRef) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let entry = SendableAXElement::retained(element);
        self.map.lock().unwrap().insert(id, entry);
        id
    }

    /// Retrieve a clone (retain'd) of the element for a given handle.
    fn get(&self, handle: u64) -> Option<SendableAXElement> {
        self.map.lock().unwrap().get(&handle).cloned()
    }

    /// Release all stored elements.
    fn clear(&self) {
        self.map.lock().unwrap().clear(); // Drop impls call CFRelease
    }
}

// ---------------------------------------------------------------------------
// Attribute helpers (blocking — must run off async runtime)
// ---------------------------------------------------------------------------

/// Read a string attribute from an AXUIElement. Returns `None` on error / unsupported.
fn ax_string_attr(element: AXUIElementRef, attr_name: &str) -> Option<String> {
    let attr_cf = CFString::new(attr_name);
    let mut value: CFTypeRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(element, attr_cf.as_concrete_TypeRef(), &mut value)
    };
    if err != AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }
    // value is a CFStringRef — attempt to downcast
    let cf_type: CFType = unsafe { TCFType::wrap_under_create_rule(value) };
    // Try to interpret as CFString
    if cf_type.type_of() == unsafe { core_foundation::string::CFString::type_id() } {
        let s: CFString = unsafe { TCFType::wrap_under_get_rule(value as CFStringRef) };
        Some(s.to_string())
    } else {
        None
    }
}

/// Read a boolean attribute. Returns `None` on error / unsupported.
fn ax_bool_attr(element: AXUIElementRef, attr_name: &str) -> Option<bool> {
    let attr_cf = CFString::new(attr_name);
    let mut value: CFTypeRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(element, attr_cf.as_concrete_TypeRef(), &mut value)
    };
    if err != AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }
    let cf_type: CFType = unsafe { TCFType::wrap_under_create_rule(value) };
    if cf_type.type_of() == CFBoolean::type_id() {
        let b: CFBoolean = unsafe { TCFType::wrap_under_get_rule(value as *const _) };
        Some(b == CFBoolean::true_value())
    } else {
        None
    }
}

/// Read AXPosition + AXSize into `UnifiedBounds`.
fn ax_bounds(element: AXUIElementRef) -> Option<UnifiedBounds> {
    let position = ax_raw_value(element, "AXPosition")?;
    let size = ax_raw_value(element, "AXSize")?;

    let mut point = CGPoint::default();
    let mut cg_size = CGSize::default();

    let got_point = unsafe {
        AXValueGetValue(
            position,
            K_AX_VALUE_TYPE_CGPOINT,
            &mut point as *mut CGPoint as *mut c_void,
        )
    };
    let got_size = unsafe {
        AXValueGetValue(
            size,
            K_AX_VALUE_TYPE_CGSIZE,
            &mut cg_size as *mut CGSize as *mut c_void,
        )
    };

    // Release the AXValue objects we got from CopyAttributeValue
    unsafe {
        CFRelease(position);
        CFRelease(size);
    }

    if got_point && got_size {
        Some(UnifiedBounds {
            x: point.x as i32,
            y: point.y as i32,
            width: cg_size.width as i32,
            height: cg_size.height as i32,
        })
    } else {
        None
    }
}

/// Get a raw CFTypeRef attribute value (caller must CFRelease).
fn ax_raw_value(element: AXUIElementRef, attr_name: &str) -> Option<CFTypeRef> {
    let attr_cf = CFString::new(attr_name);
    let mut value: CFTypeRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(element, attr_cf.as_concrete_TypeRef(), &mut value)
    };
    if err != AX_ERROR_SUCCESS || value.is_null() {
        None
    } else {
        Some(value)
    }
}

/// Read a numeric attribute as f64. Returns `None` on error.
fn ax_number_attr(element: AXUIElementRef, attr_name: &str) -> Option<f64> {
    let attr_cf = CFString::new(attr_name);
    let mut value: CFTypeRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(element, attr_cf.as_concrete_TypeRef(), &mut value)
    };
    if err != AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }
    let cf_type: CFType = unsafe { TCFType::wrap_under_create_rule(value) };
    if cf_type.type_of() == CFNumber::type_id() {
        let n: CFNumber = unsafe { TCFType::wrap_under_get_rule(value as *const _) };
        n.to_f64()
    } else {
        None
    }
}

/// Read the children array for an element. Returns retained AXUIElementRefs that
/// the caller owns (they are *not* wrapped — caller must CFRelease each one or
/// store them in the handle table).
fn ax_children(element: AXUIElementRef) -> Vec<AXUIElementRef> {
    let attr_cf = CFString::new("AXChildren");
    let mut value: CFTypeRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(element, attr_cf.as_concrete_TypeRef(), &mut value)
    };
    if err != AX_ERROR_SUCCESS || value.is_null() {
        return vec![];
    }
    // value is a CFArrayRef
    let array: CFArray<*const c_void> =
        unsafe { CFArray::wrap_under_create_rule(value as *const _) };
    let count = array.len();
    let mut children = Vec::with_capacity(count as usize);
    for i in 0..count {
        let child = unsafe { *array.get(i) };
        if !child.is_null() {
            // Retain so that the child outlives the parent array
            unsafe { CFRetain(child as CFTypeRef) };
            children.push(child as AXUIElementRef);
        }
    }
    children
}

// ---------------------------------------------------------------------------
// Role mapping
// ---------------------------------------------------------------------------

fn map_ax_role(role_str: &str) -> UnifiedRole {
    match role_str {
        "AXButton" => UnifiedRole::Button,
        "AXCheckBox" => UnifiedRole::Checkbox,
        "AXComboBox" => UnifiedRole::Combobox,
        "AXDialog" => UnifiedRole::Dialog,
        "AXTextField" => UnifiedRole::Textbox,
        "AXTextArea" => UnifiedRole::Textbox,
        "AXStaticText" => UnifiedRole::StaticText,
        "AXLink" => UnifiedRole::Link,
        "AXList" => UnifiedRole::List,
        "AXRow" => UnifiedRole::Row,
        "AXMenu" => UnifiedRole::Menu,
        "AXMenuBar" => UnifiedRole::Menubar,
        "AXMenuItem" => UnifiedRole::Menuitem,
        "AXMenuButton" => UnifiedRole::Button,
        "AXRadioButton" => UnifiedRole::Radio,
        "AXRadioGroup" => UnifiedRole::Radiogroup,
        "AXSlider" => UnifiedRole::Slider,
        "AXTabGroup" => UnifiedRole::Tablist,
        "AXTab" => UnifiedRole::Tab,
        "AXToolbar" => UnifiedRole::Toolbar,
        "AXScrollBar" => UnifiedRole::Scrollbar,
        "AXImage" => UnifiedRole::Img,
        "AXGroup" => UnifiedRole::Group,
        "AXHeading" => UnifiedRole::Heading,
        "AXTable" => UnifiedRole::Table,
        "AXCell" => UnifiedRole::Cell,
        "AXWindow" => UnifiedRole::Window,
        "AXSheet" => UnifiedRole::Dialog,
        "AXPopUpButton" => UnifiedRole::Combobox,
        "AXApplication" => UnifiedRole::Application,
        "AXSplitter" => UnifiedRole::Separator,
        "AXProgressIndicator" => UnifiedRole::Progressbar,
        "AXBrowser" => UnifiedRole::Tree,
        "AXOutline" => UnifiedRole::Tree,
        "AXOutlineRow" => UnifiedRole::Treeitem,
        "AXStepper" => UnifiedRole::Spinbutton,
        "AXSwitch" => UnifiedRole::Switch,
        "AXWebArea" => UnifiedRole::Document,
        "AXValueIndicator" => UnifiedRole::Slider,
        "AXDisclosureTriangle" => UnifiedRole::Button,
        "AXPopover" => UnifiedRole::Dialog,
        _ => UnifiedRole::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Recursive tree builder (blocking)
// ---------------------------------------------------------------------------

/// Build a `UnifiedNode` tree from an AXUIElement, recording handles in the table.
fn build_tree(
    element: AXUIElementRef,
    handles: &HandleTable,
    ref_counter: &AtomicU64,
    depth: u32,
    max_depth: Option<u32>,
    include_hidden: bool,
) -> Option<UnifiedNode> {
    if element.is_null() {
        return None;
    }

    // Check hidden state early to skip if not wanted
    let hidden = ax_bool_attr(element, "AXHidden").unwrap_or(false);
    if hidden && !include_hidden {
        // Release the element since we won't store it
        unsafe { CFRelease(element as CFTypeRef) };
        return None;
    }

    let role_str = ax_string_attr(element, "AXRole").unwrap_or_default();
    let role = map_ax_role(&role_str);

    // Name: prefer AXTitle, fall back to AXDescription
    let name =
        ax_string_attr(element, "AXTitle").or_else(|| ax_string_attr(element, "AXDescription"));

    let value = ax_string_attr(element, "AXValue");
    let description = ax_string_attr(element, "AXRoleDescription");

    let bounds = ax_bounds(element);

    let enabled = ax_bool_attr(element, "AXEnabled").unwrap_or(true);
    let focused = ax_bool_attr(element, "AXFocused").unwrap_or(false);
    let selected = ax_bool_attr(element, "AXSelected");
    let expanded = ax_bool_attr(element, "AXExpanded");

    let state = UnifiedState {
        is_focused: focused,
        is_disabled: !enabled,
        is_hidden: hidden,
        is_expanded: super::super::model::TriBool::from_option(expanded),
        is_selected: super::super::model::TriBool::from_option(selected),
        is_focusable: ax_bool_attr(element, "AXFocused").is_some(), // presence implies focusable
        is_editable: matches!(role, UnifiedRole::Textbox),
        ..Default::default()
    };

    let is_interactive = role.is_interactive_role();

    let handle_id = handles.insert(element);
    let ref_idx = ref_counter.fetch_add(1, Ordering::Relaxed);
    let ref_id = format!("@e{}", ref_idx);

    // Determine supported patterns
    let mut patterns = Vec::new();
    if is_interactive {
        patterns.push(InteractionPattern::Invoke);
    }
    if matches!(role, UnifiedRole::Textbox | UnifiedRole::Searchbox) {
        patterns.push(InteractionPattern::Value);
    }
    if matches!(role, UnifiedRole::Slider | UnifiedRole::Spinbutton) {
        patterns.push(InteractionPattern::RangeValue);
    }
    if expanded.is_some() {
        patterns.push(InteractionPattern::ExpandCollapse);
    }
    if bounds.is_some() {
        patterns.push(InteractionPattern::CoordinateClick);
    }

    // Recurse into children unless we hit max_depth
    let children = if max_depth.map_or(true, |md| depth < md) {
        let child_refs = ax_children(element);
        let mut child_nodes = Vec::with_capacity(child_refs.len());
        for child_ref in child_refs {
            if let Some(child_node) = build_tree(
                child_ref,
                handles,
                ref_counter,
                depth + 1,
                max_depth,
                include_hidden,
            ) {
                child_nodes.push(child_node);
            } else {
                // build_tree releases on skip, but if it returned None for
                // reasons other than hidden we still need to release
                // (build_tree handles its own release in the hidden case;
                // in the null case nothing to release)
            }
        }
        child_nodes
    } else {
        vec![]
    };

    Some(UnifiedNode {
        ref_id,
        role,
        name,
        value,
        description,
        bounds,
        state,
        is_interactive,
        level: None,
        automation_id: ax_string_attr(element, "AXIdentifier"),
        class_name: None,
        html_tag: None,
        url: ax_string_attr(element, "AXURL"),
        children,
        source: NodeSource::Ax,
        platform_handle: Some(handle_id),

        supported_patterns: patterns,
        generation: 0,
    })
}

// ---------------------------------------------------------------------------
// Window-title matching helper (blocking)
// ---------------------------------------------------------------------------

/// Iterate running applications via the system-wide element, looking for a window
/// whose AXTitle contains `title` (case-insensitive). Returns a retained
/// AXUIElementRef for the matching window.
fn find_window_by_title(title: &str) -> anyhow::Result<AXUIElementRef> {
    // We enumerate via NSWorkspace is simplest, but staying in pure AX:
    // system-wide -> each app's AXWindows -> match title.
    //
    // There is no "all windows" attribute on the system-wide element. Instead we
    // use the Carbon approach: iterate pids via procinfo isn't great, so we use
    // a small CGWindowList shortcut — but to avoid adding more FFI surface,
    // just iterate a reasonable set of known pids. In practice the UIA/AT-SPI
    // adapters use similar OS-level process listing. For now we use a simpler
    // NSWorkspace-like approach via the AX API.
    //
    // Actually, the cleanest pure-AX approach: create system-wide element, then
    // use CGWindowListCopyWindowInfo to get a list of (pid, window title) and
    // match on title. We only need one more FFI call.

    // Approach: use CoreGraphics CGWindowListCopyWindowInfo
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFTypeRef;
    }

    const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1;
    const K_CG_NULL_WINDOW_ID: u32 = 0;

    let info_list = unsafe {
        CGWindowListCopyWindowInfo(K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY, K_CG_NULL_WINDOW_ID)
    };
    if info_list.is_null() {
        bail!("CGWindowListCopyWindowInfo returned null");
    }

    let array: CFArray<*const c_void> =
        unsafe { CFArray::wrap_under_create_rule(info_list as *const _) };

    let title_lower = title.to_lowercase();

    for i in 0..array.len() {
        let dict_ref = unsafe { *array.get(i) };
        if dict_ref.is_null() {
            continue;
        }

        // Extract kCGWindowOwnerPID and kCGWindowName from the dict
        let pid_key = CFString::new("kCGWindowOwnerPID");
        let name_key = CFString::new("kCGWindowName");

        let pid_val = unsafe {
            core_foundation::dictionary::CFDictionary::wrap_under_get_rule(dict_ref as *const _)
        };

        // Use find() on the dictionary
        let maybe_pid = pid_val.find(pid_key.as_CFTypeRef());
        let maybe_name = pid_val.find(name_key.as_CFTypeRef());

        if let (Some(&pid_ref), Some(&name_ref)) = (maybe_pid, maybe_name) {
            // Extract window name
            if name_ref.is_null() {
                continue;
            }
            let win_name: CFString =
                unsafe { TCFType::wrap_under_get_rule(name_ref as CFStringRef) };
            let win_name_str = win_name.to_string();

            if win_name_str.to_lowercase().contains(&title_lower) {
                // Extract PID
                let pid_number: CFNumber =
                    unsafe { TCFType::wrap_under_get_rule(pid_ref as *const _) };
                if let Some(pid) = pid_number.to_i32() {
                    // Create an app element for this PID, then find the window
                    let app = unsafe { AXUIElementCreateApplication(pid) };
                    if app.is_null() {
                        continue;
                    }
                    // Get windows of this application
                    let windows_key = CFString::new("AXWindows");
                    let mut windows_val: CFTypeRef = std::ptr::null();
                    let err = unsafe {
                        AXUIElementCopyAttributeValue(
                            app,
                            windows_key.as_concrete_TypeRef(),
                            &mut windows_val,
                        )
                    };
                    if err != AX_ERROR_SUCCESS || windows_val.is_null() {
                        unsafe { CFRelease(app as CFTypeRef) };
                        continue;
                    }
                    let windows: CFArray<*const c_void> =
                        unsafe { CFArray::wrap_under_create_rule(windows_val as *const _) };

                    for j in 0..windows.len() {
                        let win = unsafe { *windows.get(j) };
                        if win.is_null() {
                            continue;
                        }
                        let win_title =
                            ax_string_attr(win as AXUIElementRef, "AXTitle").unwrap_or_default();
                        if win_title.to_lowercase().contains(&title_lower) {
                            // Found it — retain the window element, release the app
                            unsafe { CFRetain(win as CFTypeRef) };
                            unsafe { CFRelease(app as CFTypeRef) };
                            return Ok(win as AXUIElementRef);
                        }
                    }
                    unsafe { CFRelease(app as CFTypeRef) };
                }
            }
        }
    }

    bail!("no on-screen window found matching title {:?}", title)
}

// ---------------------------------------------------------------------------
// AxAdapter
// ---------------------------------------------------------------------------

/// macOS Accessibility adapter using AXUIElement FFI.
pub struct AxAdapter {
    /// The root AXUIElement we are connected to.
    root: Arc<Mutex<Option<SendableAXElement>>>,
    /// Whether we are connected.
    connected: Arc<AtomicBool>,
    /// Handle table mapping u64 -> retained AXUIElementRef.
    handles: Arc<HandleTable>,
}

impl AxAdapter {
    pub fn new() -> Self {
        Self {
            root: Arc::new(Mutex::new(None)),
            connected: Arc::new(AtomicBool::new(false)),
            handles: Arc::new(HandleTable::new()),
        }
    }
}

#[async_trait]
impl PlatformAdapter for AxAdapter {
    fn backend_name(&self) -> &'static str {
        "ax"
    }

    async fn connect(&mut self, target: ConnectionTarget, _timeout_ms: u64) -> anyhow::Result<()> {
        let root_arc = self.root.clone();
        let connected = self.connected.clone();
        let handles = self.handles.clone();

        tokio::task::spawn_blocking(move || {
            // Check accessibility trust
            let trusted = unsafe { AXIsProcessTrusted() };
            if !trusted {
                bail!(
                    "accessibility access not granted — enable this app in \
                     System Settings > Privacy & Security > Accessibility"
                );
            }

            let element: AXUIElementRef = match target {
                ConnectionTarget::Desktop => {
                    let el = unsafe { AXUIElementCreateSystemWide() };
                    if el.is_null() {
                        bail!("AXUIElementCreateSystemWide returned null");
                    }
                    el
                }
                ConnectionTarget::ProcessId(pid) => {
                    let el = unsafe { AXUIElementCreateApplication(pid as pid_t) };
                    if el.is_null() {
                        bail!("AXUIElementCreateApplication returned null for pid {}", pid);
                    }
                    el
                }
                ConnectionTarget::WindowTitle(ref title) => find_window_by_title(title)?,
            };

            handles.clear();
            // element is already +1 from the Create/find function — store without extra retain
            *root_arc.lock().unwrap() = Some(SendableAXElement::new(element));
            connected.store(true, Ordering::Release);
            debug!("AX adapter connected");
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.handles.clear();
        // Drop the root element (CFRelease via SendableAXElement::drop)
        *self.root.lock().unwrap() = None;
        self.connected.store(false, Ordering::Release);
        debug!("AX adapter disconnected");
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
            bail!("AX adapter is not connected");
        }

        let root_arc = self.root.clone();
        let handles = self.handles.clone();

        tokio::task::spawn_blocking(move || {
            let guard = root_arc.lock().unwrap();
            let root_el = guard.as_ref().context("no root element — not connected")?;

            // Clear old handles before building a fresh tree
            handles.clear();

            let ref_counter = AtomicU64::new(0);

            build_tree(
                root_el.as_ptr(),
                &handles,
                &ref_counter,
                0,
                max_depth,
                include_hidden,
            )
            .context("root AXUIElement produced no tree")
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn subscribe_events(&self) -> anyhow::Result<Option<mpsc::Receiver<A11yEvent>>> {
        // AXObserver-based event streaming is deferred to a later phase.
        Ok(None)
    }

    async fn interact(
        &self,
        platform_handle: u64,
        pattern: InteractionPattern,
        params: InteractionParams,
    ) -> anyhow::Result<InteractionResult> {
        let handles = self.handles.clone();

        tokio::task::spawn_blocking(move || {
            let element = match handles.get(platform_handle) {
                Some(e) => e,
                None => {
                    return Ok(InteractionResult::err(
                        pattern,
                        format!("handle {} not found in AX handle table", platform_handle),
                    ))
                }
            };
            let ptr = element.as_ptr();

            match pattern {
                InteractionPattern::Invoke => {
                    let action = CFString::new("AXPress");
                    let err =
                        unsafe { AXUIElementPerformAction(ptr, action.as_concrete_TypeRef()) };
                    if err == AX_ERROR_SUCCESS {
                        Ok(InteractionResult::ok(pattern))
                    } else {
                        Ok(InteractionResult::err(
                            pattern,
                            format!("AXPress failed with AXError {}", err),
                        ))
                    }
                }
                InteractionPattern::Value => {
                    if let InteractionParams::Text {
                        value,
                        clear_first: _,
                    } = params
                    {
                        // Set AXValue
                        let attr = CFString::new("AXValue");
                        let val = CFString::new(&value);
                        let err = unsafe {
                            AXUIElementSetAttributeValue(
                                ptr,
                                attr.as_concrete_TypeRef(),
                                val.as_CFTypeRef(),
                            )
                        };
                        if err == AX_ERROR_SUCCESS {
                            Ok(InteractionResult::ok(pattern))
                        } else {
                            Ok(InteractionResult::err(
                                pattern,
                                format!("set AXValue failed with AXError {}", err),
                            ))
                        }
                    } else {
                        Ok(InteractionResult::err(
                            pattern,
                            "Value pattern requires Text params",
                        ))
                    }
                }
                InteractionPattern::Toggle => {
                    // Toggle is just AXPress on macOS
                    let action = CFString::new("AXPress");
                    let err =
                        unsafe { AXUIElementPerformAction(ptr, action.as_concrete_TypeRef()) };
                    if err == AX_ERROR_SUCCESS {
                        Ok(InteractionResult::ok(pattern))
                    } else {
                        Ok(InteractionResult::err(
                            pattern,
                            format!("AXPress (toggle) failed with AXError {}", err),
                        ))
                    }
                }
                InteractionPattern::ExpandCollapse => {
                    if let InteractionParams::ExpandCollapse { expand } = params {
                        // Use AXPress to toggle, or set AXExpanded if writable
                        let attr = CFString::new("AXExpanded");
                        let val = if expand {
                            CFBoolean::true_value()
                        } else {
                            CFBoolean::false_value()
                        };
                        let err = unsafe {
                            AXUIElementSetAttributeValue(
                                ptr,
                                attr.as_concrete_TypeRef(),
                                val.as_CFTypeRef(),
                            )
                        };
                        if err == AX_ERROR_SUCCESS {
                            Ok(InteractionResult::ok(pattern))
                        } else {
                            // Fallback: AXPress
                            let action = CFString::new("AXPress");
                            let err2 = unsafe {
                                AXUIElementPerformAction(ptr, action.as_concrete_TypeRef())
                            };
                            if err2 == AX_ERROR_SUCCESS {
                                Ok(InteractionResult::ok(pattern))
                            } else {
                                Ok(InteractionResult::err(
                                    pattern,
                                    format!(
                                        "expand/collapse failed: set AXExpanded={}, AXPress={}",
                                        err, err2
                                    ),
                                ))
                            }
                        }
                    } else {
                        Ok(InteractionResult::err(
                            pattern,
                            "ExpandCollapse pattern requires ExpandCollapse params",
                        ))
                    }
                }
                InteractionPattern::RangeValue => {
                    if let InteractionParams::RangeValue { value } = params {
                        let attr = CFString::new("AXValue");
                        let val = CFNumber::from(value);
                        let err = unsafe {
                            AXUIElementSetAttributeValue(
                                ptr,
                                attr.as_concrete_TypeRef(),
                                val.as_CFTypeRef(),
                            )
                        };
                        if err == AX_ERROR_SUCCESS {
                            Ok(InteractionResult::ok(pattern))
                        } else {
                            Ok(InteractionResult::err(
                                pattern,
                                format!("set AXValue (range) failed with AXError {}", err),
                            ))
                        }
                    } else {
                        Ok(InteractionResult::err(
                            pattern,
                            "RangeValue pattern requires RangeValue params",
                        ))
                    }
                }
                InteractionPattern::CoordinateClick => {
                    // Return bounds center — the caller (interaction engine) handles
                    // the actual mouse move + click via platform mouse APIs.
                    let b = ax_bounds(ptr);
                    if let Some(bounds) = b {
                        Ok(InteractionResult {
                            success: true,
                            error: None,
                            pattern_used: pattern,
                        })
                    } else {
                        Ok(InteractionResult::err(pattern, "element has no bounds"))
                    }
                }
                InteractionPattern::Selection | InteractionPattern::Scroll => {
                    // AXPress as a best-effort for selection; scroll not natively
                    // supported via simple AX calls.
                    let action = CFString::new("AXPress");
                    let err =
                        unsafe { AXUIElementPerformAction(ptr, action.as_concrete_TypeRef()) };
                    if err == AX_ERROR_SUCCESS {
                        Ok(InteractionResult::ok(pattern))
                    } else {
                        Ok(InteractionResult::err(
                            pattern,
                            format!("AXPress (selection/scroll) failed with AXError {}", err),
                        ))
                    }
                }
                InteractionPattern::Text => Ok(InteractionResult::err(
                    pattern,
                    format!("{:?} pattern is not supported by the AX adapter", pattern),
                )),
            }
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn supported_patterns(&self, platform_handle: u64) -> Vec<InteractionPattern> {
        let handles = self.handles.clone();

        tokio::task::spawn_blocking(move || {
            let element = match handles.get(platform_handle) {
                Some(e) => e,
                None => return vec![],
            };
            let ptr = element.as_ptr();

            let role_str = ax_string_attr(ptr, "AXRole").unwrap_or_default();
            let role = map_ax_role(&role_str);

            let mut patterns = Vec::new();

            if role.is_interactive_role() {
                patterns.push(InteractionPattern::Invoke);
            }
            if matches!(role, UnifiedRole::Textbox | UnifiedRole::Searchbox) {
                patterns.push(InteractionPattern::Value);
            }
            if matches!(role, UnifiedRole::Checkbox | UnifiedRole::Switch) {
                patterns.push(InteractionPattern::Toggle);
            }
            if matches!(role, UnifiedRole::Slider | UnifiedRole::Spinbutton) {
                patterns.push(InteractionPattern::RangeValue);
            }
            if ax_bool_attr(ptr, "AXExpanded").is_some() {
                patterns.push(InteractionPattern::ExpandCollapse);
            }
            if ax_bounds(ptr).is_some() {
                patterns.push(InteractionPattern::CoordinateClick);
            }

            patterns
        })
        .await
        .unwrap_or_default()
    }
}

impl Drop for AxAdapter {
    fn drop(&mut self) {
        self.handles.clear();
        // root element released via SendableAXElement::drop when the Mutex is dropped
    }
}
